use std::os::unix::process::CommandExt;

use color_eyre::Result;
use color_eyre::eyre::bail;
use nix_command::{CommandKind, NixCommand};
use subprocess::{Exec, Redirection};
use tracing::debug;

use crate::args::DevelopArgs;
use crate::diff::PackageDiff;
use crate::shell::ShellType;
use crate::{dirs, env_file, meta, ui};

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
  let flake_ref = args.flake.as_deref().unwrap_or(".");
  let target = args.target.as_deref().unwrap_or("default");
  let installable = format_installable(flake_ref, target);

  ensure_flake_locked(flake_ref)?;

  // Build with nom (shows build tree)
  if !args.no_nom {
    ui::loading(format!("building devshell \"{installable}\""));
    let build_cmd = NixCommand::new(CommandKind::Build)
      .print_build_logs(false)
      .arg(&installable);

    let result = run_with_nom(build_cmd.to_exec());
    if result.is_err() {
      xi_core::suggest::print_suggestions_on_failure(
        flake_ref,
        target,
        Some("devShells"),
      );
    }
    result?;
  }

  // Eval and show diff, write env files
  let eval_result = eval_and_update(flake_ref, target);
  if eval_result.is_err() {
    xi_core::suggest::print_suggestions_on_failure(
      flake_ref,
      target,
      Some("devShells"),
    );
  }
  let (dev_env, _state_dir) = eval_result?;

  // Determine user shell
  let shell_type = detect_shell(args.shell.as_deref())?;

  // If --command was passed, run it with env vars and exit
  if let Some(ref shell_cmd) = args.command {
    return run_command_in_env(&dev_env, shell_cmd, flake_ref);
  }

  // Apply nix env vars to the current process, then exec user's shell.
  // The shell inherits all env vars including the prepended PATH.
  ui::success("entering devshell");
  exec_with_env(&dev_env, shell_type)?;

  Ok(())
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
fn exec_with_env(dev_env: &env_file::DevEnv, shell: ShellType) -> Result<()> {
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
  let Ok(fid) = dirs::flake_id_from_ref(flake_ref) else {
    let dev_env = eval_devshell(flake_ref, target, None)?;
    return Ok((dev_env, std::env::temp_dir()));
  };
  let state_dir = dirs::state_dir(&fid);
  let profile_path = dirs::profile_path(&state_dir, target);

  let old_meta = meta::load(&state_dir).ok();
  let old_packages = old_meta
    .as_ref()
    .map(|m| m.packages.clone())
    .unwrap_or_default();

  let new_env = eval_devshell(flake_ref, target, Some(&profile_path))?;

  let diff = PackageDiff::compute(&old_packages, &new_env.packages);
  if !diff.is_empty() {
    diff.print_full();
  }

  meta::save(
    &state_dir,
    &meta::DevShellMeta {
      env_hash: new_env.env_hash.clone(),
      target: target.to_string(),
      flake_root: flake_ref.to_string(),
      store_path: None,
      packages: new_env.packages.clone(),
      timestamp: meta::now_secs(),
      eval_duration_ms: 0,
      lock_hash: None,
      input_hash: None,
    },
  )?;

  for shell in [ShellType::Bash, ShellType::Zsh, ShellType::Fish] {
    env_file::write_env_file(&state_dir, &new_env, shell, target)?;
  }

  Ok((new_env, state_dir))
}

fn eval_devshell(
  flake_ref: &str,
  target: &str,
  profile_path: Option<&std::path::Path>,
) -> Result<env_file::DevEnv> {
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

  env_file::DevEnv::from_nix_json(&output.stdout)
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
