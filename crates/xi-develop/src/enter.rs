use std::os::unix::process::CommandExt;

use color_eyre::{Result, eyre::bail};
use nix_command::{CommandKind, NixCommand};
use subprocess::{Exec, Redirection};
use tracing::debug;

use crate::{
  args::DevelopArgs,
  diff::PackageDiff,
  dirs,
  env_file,
  meta,
  shell::ShellType,
  ui,
};

/// Enter a development shell (blocking, sync mode).
///
/// Uses `nix print-dev-env --json` to extract the environment, then
/// spawns the user's shell as a subshell with the devshell env applied.
/// Uses the same `subshell.rs` init script generator as the async path,
/// ensuring consistent behavior between `xi develop` and the prompt hook.
///
/// # Errors
///
/// Returns an error if the build or shell entry fails.
pub fn enter(args: &DevelopArgs) -> Result<()> {
  let t0 = std::time::Instant::now();
  let flake_ref = args.flake.as_deref().unwrap_or(".");
  let target = args.target.as_deref().unwrap_or("default");

  ensure_flake_locked(flake_ref)?;
  debug!(elapsed_ms = t0.elapsed().as_millis(), "phase: lock check");

  // Warm fast path: if the eval inputs (flake.lock/flake.nix, per the
  // configured eval-cache mode) are unchanged since the last successful
  // eval, reuse the cached print-dev-env output  -  zero nix invocations,
  // no build tree, no package diff. Staleness is self-healing: the
  // subshell's prompt hook ensures the daemon, which re-evals on real
  // changes and re-sources the running shell via the generation counter.
  let dev_env = if let Some(cached) = load_cached_env(flake_ref, target) {
    debug!(
      elapsed_ms = t0.elapsed().as_millis(),
      "phase: warm entry (input hash match, cached env reused)"
    );
    cached
  } else {
    let cold = cold_eval(flake_ref, target, args.no_nom);
    if cold.is_err() {
      xi_core::suggest::print_suggestions_on_failure(
        flake_ref,
        target,
        Some("devShells"),
      );
    }
    let dev_env = cold?;
    debug!(
      elapsed_ms = t0.elapsed().as_millis(),
      "phase: cold build+eval done"
    );
    dev_env
  };

  // Determine user shell
  let shell_type = detect_shell(args.shell.as_deref())?;

  // If --command was passed, run it with env vars and exit
  if let Some(ref shell_cmd) = args.command {
    return run_command_in_env(&dev_env, shell_cmd, flake_ref);
  }

  // Apply nix env vars to the current process, then exec user's shell.
  // The shell inherits all env vars including the prepended PATH.
  ui::success("entering devshell");
  exec_with_env(&dev_env, shell_type, flake_ref)?;

  Ok(())
}

/// The configured eval-cache mode, shared with the daemon so the input
/// hashes the two sides write/compare stay in the same scheme.
fn eval_cache_mode() -> crate::daemon::server::EvalCacheMode {
  let (_, _, mode) = crate::daemon::lifecycle::read_develop_config();
  crate::daemon::server::EvalCacheMode::parse(&mode)
}

/// On-disk cache of the raw `nix print-dev-env --json` output.
fn devenv_cache_path(
  state_dir: &std::path::Path,
  target: &str,
) -> std::path::PathBuf {
  state_dir.join(format!("devenv-{target}.json"))
}

/// Try the warm path: inputs unchanged since the last successful eval
/// for this target → parse the cached print-dev-env JSON directly.
fn load_cached_env(flake_ref: &str, target: &str) -> Option<env_file::DevEnv> {
  let canonical = std::fs::canonicalize(flake_ref).ok()?;
  let fid = dirs::flake_id(&canonical);
  let state_dir = dirs::state_dir(&fid);

  let current_hash =
    crate::daemon::server::compute_input_hash(&canonical, eval_cache_mode())?;

  let meta = meta::load(&state_dir).ok()?;
  if meta.input_hash.as_deref() != Some(current_hash.as_str())
    || meta.target != target
  {
    return None;
  }

  let raw = std::fs::read(devenv_cache_path(&state_dir, target)).ok()?;
  env_file::DevEnv::from_nix_json(&raw).ok()
}

/// Cold path: realize + eval the devshell.
///
/// The nom build (progress UX) and `nix print-dev-env` (the data we
/// actually need) target the same derivation; the nix daemon dedups the
/// realization, so running them concurrently costs max(build, eval)
/// wall-time instead of build + eval.
fn cold_eval(
  flake_ref: &str,
  target: &str,
  no_nom: bool,
) -> Result<env_file::DevEnv> {
  let installable = format_installable(flake_ref, target);

  if no_nom {
    let (dev_env, _) = eval_and_update(flake_ref, target)?;
    return Ok(dev_env);
  }

  ui::loading(format!("building devshell \"{installable}\""));
  let build_cmd = NixCommand::new(CommandKind::Build)
    .print_build_logs(false)
    .arg(&installable);

  std::thread::scope(|scope| {
    let build_handle = scope.spawn(|| run_with_nom(build_cmd.to_exec()));
    let eval_result = eval_and_update(flake_ref, target);
    let build_result = build_handle.join().unwrap_or_else(|_| {
      Err(color_eyre::eyre::eyre!("nom build thread panicked"))
    });

    // Eval failure is the more informative error (it carries nix's
    // stderr); report it first, fall back to the build failure.
    let (dev_env, _state_dir) = eval_result?;
    build_result?;
    Ok(dev_env)
  })
}

/// Detect the shell type from --shell flag or $SHELL.
fn detect_shell(shell_arg: Option<&str>) -> Result<ShellType> {
  if let Some(s) = shell_arg {
    return ShellType::parse(s);
  }

  let shell_env = std::env::var("SHELL").unwrap_or_default();
  let shell_name = std::path::Path::new(&shell_env)
    .file_name()
    .and_then(|n| n.to_str())
    .unwrap_or("bash");

  ShellType::parse(shell_name)
}

/// Apply nix env vars and exec the user's shell.
/// Replaces the current process — does not return on success.
fn exec_with_env(
  dev_env: &env_file::DevEnv,
  shell: ShellType,
  flake_ref: &str,
) -> Result<()> {
  // Prepend nix paths to PATH
  if !dev_env.nix_paths.is_empty() {
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let nix_path = dev_env.nix_paths.join(":");
    // SAFETY: single-threaded at this point, about to exec
    unsafe { std::env::set_var("PATH", format!("{nix_path}:{orig_path}")) };
  }

  // Set env vars
  for (key, value) in &dev_env.env_vars {
    unsafe { std::env::set_var(key, value) };
  }
  unsafe {
    std::env::set_var("IN_NIX_SHELL", "impure");
    std::env::set_var("__XI_IN_DEVSHELL", "1");
    // Let the subshell prompt hook know which flake owns this session,
    // so its nesting / cd-out logic and `xi develop status`'s session
    // line behave the same as an async-activated devshell.
    if let Ok(root) = std::fs::canonicalize(flake_ref) {
      std::env::set_var("__XI_FLAKE_ROOT", root.as_os_str());
    }
  }

  // Run shellHook if present (in a subshell so it doesn't affect exec)
  if let Some(ref hook) = dev_env.shell_hook
    && !hook.trim().is_empty()
  {
    let _ = std::process::Command::new("sh")
      .arg("-c")
      .arg(hook)
      .status();
  }

  // Exec the user's shell
  let shell_name = shell.name();
  let mut cmd = std::process::Command::new(shell_name);
  debug!(?cmd, "exec {shell_name}");
  let err = cmd.exec();
  bail!("exec {shell_name} failed: {err}");
}

/// Run a command with nix env vars applied (for --command mode).
fn run_command_in_env(
  dev_env: &env_file::DevEnv,
  shell_cmd: &str,
  flake_ref: &str,
) -> Result<()> {
  let mut path_parts = dev_env.nix_paths.clone();
  if let Ok(existing) = std::env::var("PATH") {
    path_parts.push(existing);
  }
  let new_path = path_parts.join(":");

  let mut cmd = std::process::Command::new("sh");
  cmd
    .arg("-c")
    .arg(shell_cmd)
    .current_dir(flake_ref)
    .env("PATH", &new_path)
    .env("IN_NIX_SHELL", "impure");

  for (key, value) in &dev_env.env_vars {
    cmd.env(key, value);
  }

  let status = cmd.status()?;
  if !status.success() {
    bail!("command exited with status {}", status.code().unwrap_or(-1));
  }
  Ok(())
}

fn eval_and_update(
  flake_ref: &str,
  target: &str,
) -> Result<(env_file::DevEnv, std::path::PathBuf)> {
  let eval_start = std::time::Instant::now();

  let Ok(fid) = dirs::flake_id_from_ref(flake_ref) else {
    let (dev_env, _) = eval_devshell(flake_ref, target, None)?;
    return Ok((dev_env, std::env::temp_dir()));
  };
  let state_dir = dirs::state_dir(&fid);
  std::fs::create_dir_all(&state_dir)?;
  let profile_path = dirs::profile_path(&state_dir, target);

  // Hash inputs BEFORE the eval so edits racing with a long eval
  // invalidate the cache next time instead of being missed (same
  // semantics as the daemon's eval cache).
  let input_hash = std::fs::canonicalize(flake_ref).ok().and_then(|root| {
    crate::daemon::server::compute_input_hash(&root, eval_cache_mode())
  });

  let old_meta = meta::load(&state_dir).ok();
  let old_packages = old_meta
    .as_ref()
    .map(|m| m.packages.clone())
    .unwrap_or_default();

  let (new_env, raw_json) =
    eval_devshell(flake_ref, target, Some(&profile_path))?;

  let diff = PackageDiff::compute(&old_packages, &new_env.packages);
  if !diff.is_empty() {
    diff.print_full();
  }

  meta::save(&state_dir, &meta::DevShellMeta {
    env_hash: new_env.env_hash.clone(),
    target: target.to_string(),
    flake_root: flake_ref.to_string(),
    store_path: None,
    packages: new_env.packages.clone(),
    timestamp: meta::now_secs(),
    eval_duration_ms: {
      #[allow(clippy::cast_possible_truncation)]
      {
        eval_start.elapsed().as_millis() as u64
      }
    },
    lock_hash: None,
    input_hash,
  })?;

  for shell in [ShellType::Bash, ShellType::Zsh, ShellType::Fish] {
    env_file::write_env_file(&state_dir, &new_env, shell, target)?;
  }

  // Cache the raw print-dev-env output for the warm fast path.
  let _ = std::fs::write(devenv_cache_path(&state_dir, target), &raw_json);

  Ok((new_env, state_dir))
}

fn eval_devshell(
  flake_ref: &str,
  target: &str,
  profile_path: Option<&std::path::Path>,
) -> Result<(env_file::DevEnv, Vec<u8>)> {
  use std::time::Duration;

  let installable = format_installable(flake_ref, target);

  let mut nix_cmd = NixCommand::new(CommandKind::PrintDevEnv)
    .arg(&installable)
    .arg("--json")
    .args(["--option", "connect-timeout", "5"])
    .args(["--option", "download-attempts", "2"])
    .with_timeout(Duration::from_mins(2));

  if let Some(profile) = profile_path {
    nix_cmd = nix_cmd
      .arg("--profile")
      .arg(profile.display().to_string().as_str());
  }

  debug!(argv = ?nix_cmd.argv(), "running nix print-dev-env (sync)");

  let mut std_cmd = nix_cmd.to_std_command();
  std_cmd
    .current_dir(flake_ref)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());

  let output = std_cmd.output()?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("nix print-dev-env failed:\n{stderr}");
  }

  let dev_env = env_file::DevEnv::from_nix_json(&output.stdout)?;
  Ok((dev_env, output.stdout))
}

fn format_installable(flake_ref: &str, target: &str) -> String {
  let system = current_nix_system();
  format!("{flake_ref}#devShells.{system}.{target}")
}

fn current_nix_system() -> String {
  let arch = std::env::consts::ARCH;
  let os = match std::env::consts::OS {
    "macos" => "darwin",
    other => other,
  };
  format!("{arch}-{os}")
}

fn ensure_flake_locked(flake_ref: &str) -> Result<()> {
  let dir = std::path::Path::new(flake_ref);
  let lock_path = dir.join("flake.lock");
  let flake_path = dir.join("flake.nix");

  if !flake_path.exists() || lock_path.exists() {
    return Ok(());
  }

  ui::loading(format!(
    "flake.lock not found in {}, running nix flake lock",
    dir.display()
  ));

  let cmd = NixCommand::new(CommandKind::Flake)
    .arg("lock")
    .arg(dir.to_string_lossy().as_ref());

  let status = cmd
    .run_with_logs()
    .map_err(|e| color_eyre::eyre::eyre!("nix flake lock failed: {e}"))?;

  if !status.success() {
    bail!("nix flake lock exited with status {status}");
  }

  Ok(())
}

/// Run a build command piped through nix-output-monitor.
fn run_with_nom(base_command: Exec) -> Result<()> {
  let pipeline = {
    base_command
      .args(["--log-format", "internal-json", "--verbose"])
      .stderr(Redirection::Merge)
      .stdout(Redirection::Pipe)
      | Exec::cmd("nom").args(["--json"])
  }
  .stdout(Redirection::None);

  debug!(?pipeline);

  let job = pipeline.start()?;

  for proc in &job.processes {
    proc.wait()?;
  }

  if let Some(nix_proc) = job.processes.first() {
    let exit_status = nix_proc.wait()?;
    if !exit_status.success() {
      bail!(xi_core::command::ExitError::new(exit_status));
    }
  }

  Ok(())
}
