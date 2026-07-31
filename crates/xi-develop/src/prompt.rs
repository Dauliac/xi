//! Shell prompt hook — the Rust-side handler for `xi develop prompt`.
//!
//! This replaces the old `hook.rs` with a cleaner architecture:
//! - All shell logic is in Rust (no bash-side flake detection or trust checks)
//! - Three modes: parent (detect flake → spawn subshell),
//!   subshell (re-source / exit), exit (deregister consumer)
//! - Communicates with daemon via Unix socket for all state
//! - Outputs shell commands to stdout (eval'd by shell)
//! - Outputs notifications to stderr (rendered with colors)

use color_eyre::Result;
use tracing::debug;

use crate::args::PromptArgs;
use crate::daemon::protocol::{Notification, PromptResponse};
use crate::daemon::{client, lifecycle};
use crate::shell::ShellType;
use crate::{dirs, flake_detect, subshell, trust};

/// Handle the `xi develop prompt` command.
///
/// Dispatches to parent, subshell, or exit mode based on flags.
///
/// # Errors
///
/// Returns an error only on critical failures (not on missing daemon etc.).
pub fn handle_prompt(args: &PromptArgs) -> Result<()> {
  let start = std::time::Instant::now();
  let shell = ShellType::parse(&args.shell)?;

  let mode = if args.exit {
    "exit"
  } else if args.subshell {
    "subshell"
  } else if std::env::var_os("__XI_IN_DEVSHELL").is_some() {
    "parent→subshell"
  } else {
    "parent"
  };
  debug!(mode, shell = %shell, pid = ?args.pid, "prompt hook fired");

  let result = if args.exit {
    handle_exit(args)
  } else if args.subshell {
    handle_subshell(args, shell)
  } else {
    handle_parent(args, shell)
  };

  debug!(
    mode,
    elapsed_ms = start.elapsed().as_millis(),
    "prompt hook done"
  );
  result
}

/// Parent mode: detect flake, check trust, spawn subshell.
///
/// Called from the parent shell's prompt hook (the activation script).
/// Outputs a subshell spawn command to stdout if a trusted flake is found.
fn handle_parent(args: &PromptArgs, shell: ShellType) -> Result<()> {
  // Guard: if already inside a devshell, delegate to subshell mode.
  // The user's .zshrc/.bashrc contains the activation hook which calls
  // this (parent mode), but we're already in a subshell. Instead of
  // spawning another subshell (recursion), handle it as subshell mode
  // which does the right thing (re-source env/hook if needed).
  if std::env::var_os("__XI_IN_DEVSHELL").is_some() {
    debug!("already in devshell, redirecting to subshell mode");
    return handle_subshell(args, shell);
  }

  let cwd = std::env::current_dir()?;

  // Check suppress flag — prevents re-entry loop after explicit exit.
  // Cleared when user cds away from the suppressed flake directory.
  if let Ok(suppress) = std::env::var("__XI_SUPPRESS") {
    let suppress_path = std::path::Path::new(&suppress);
    if cwd.starts_with(suppress_path) {
      debug!(suppress = %suppress, "re-entry suppressed");
      return Ok(());
    }
    // CWD moved away from suppressed flake — clear suppress
    debug!(suppress = %suppress, "clearing stale suppress");
    println!("{}", shell.unset("__XI_SUPPRESS"));
  }

  // Detect nearest flake root
  let Some(flake_root) = flake_detect::find_flake_root(&cwd) else {
    debug!("no flake.nix found, nothing to do");
    return Ok(());
  };
  let flake_root = &flake_root;
  debug!(?flake_root, "detected flake root");

  // Quick check: does the flake likely have devShells?
  // Read flake.nix and look for devShell/devShells/mkShell keywords.
  // This avoids showing "trust" prompts for flakes without devshells.
  if !flake_likely_has_devshell(flake_root) {
    debug!("flake.nix does not appear to have devShells, skipping");
    return Ok(());
  }

  // Trust check
  if !trust::is_trusted(flake_root) {
    eprintln!(
      "{}",
      Notification::warn("run \"xi develop trust\" to activate devshell")
        .render()
    );
    return Ok(());
  }

  // Ensure daemon is running
  let socket_path = match lifecycle::ensure(flake_root) {
    Ok(path) => path,
    Err(e) => {
      debug!("daemon not available: {e}");
      eprintln!(
        "{}",
        Notification::warn("devshell daemon could not start").render()
      );
      return Ok(());
    },
  };

  let consumer_pid = resolve_pid(args);
  let cwd_str = cwd.display().to_string();

  // Query daemon
  match client::prompt(
    &socket_path,
    consumer_pid,
    &args.target,
    &cwd_str,
    false,
    None,
  ) {
    Ok(response) => {
      render_notifications(&response);
    },
    Err(e) => {
      debug!("daemon prompt call failed: {e}");
    },
  }

  // Generate subshell spawn command — simple: set env vars + spawn shell
  let xi_bin = resolve_xi_bin();
  let spawn_cmd =
    subshell::generate_spawn_command(shell, flake_root, &cwd, &xi_bin);

  // Output spawn command to stdout (shell evals this)
  println!("{spawn_cmd}");

  Ok(())
}

/// Subshell mode: check daemon for re-source needs, handle exit/nesting.
///
/// Called from inside the devshell subshell's prompt hook.
/// Outputs source commands, "exit 0", or a nested subshell spawn to stdout.
fn handle_subshell(args: &PromptArgs, shell: ShellType) -> Result<()> {
  let cwd = std::env::current_dir()?;
  let cwd_str = cwd.display().to_string();

  // Read our flake root from the env (set by the init script)
  let our_flake_root = std::env::var("__XI_FLAKE_ROOT")
    .ok()
    .map(std::path::PathBuf::from);

  // Find the nearest flake root from CWD
  let nearest_flake = flake_detect::find_flake_root(&cwd);

  let sentinel = subshell::EXIT_CD_OUT;

  // Case 1: No flake at all — left all flake directories → exit (cd-out)
  let Some(ref nearest) = nearest_flake else {
    debug!("left all flake directories, signaling cd-out exit");
    println!("exit {sentinel}");
    return Ok(());
  };

  // Case 2: CWD is outside our flake root — cd'd out → exit (cd-out)
  if let Some(ref our_root) = our_flake_root
    && !cwd.starts_with(our_root)
  {
    debug!(our_root = %our_root.display(), cwd = %cwd.display(), "left our flake directory, signaling cd-out exit");
    println!("exit {sentinel}");
    return Ok(());
  }

  // Case 3: Nearest flake is different from ours — nested flake detected
  // Determine the effective flake root: if we're in a nested untrusted child,
  // stay with the parent flake instead of deactivating.
  let effective_root = if let Some(ref our_root) = our_flake_root {
    if nearest != our_root && cwd.starts_with(our_root) {
      // We're still inside our flake root, but CWD has a closer flake.nix
      // → spawn a nested subshell for the child flake (if not suppressed)
      let suppressed = std::env::var("__XI_SUPPRESS").ok().is_some_and(|s| {
        nearest.starts_with(s.as_str()) || s == nearest.display().to_string()
      });
      if !suppressed && trust::is_trusted(nearest) {
        debug!(child_flake = %nearest.display(), "nested flake detected, spawning subshell");

        // Ensure child daemon
        if let Ok(child_socket) = lifecycle::ensure(nearest) {
          let consumer_pid = resolve_pid(args);
          // Notify child daemon
          let _ = client::prompt(
            &child_socket,
            consumer_pid,
            &args.target,
            &cwd_str,
            false,
            Some(consumer_pid),
          );
        }

        let xi_bin = resolve_xi_bin();
        let spawn_cmd =
          subshell::generate_spawn_command(shell, nearest, &cwd, &xi_bin);
        println!("{spawn_cmd}");
        return Ok(());
      }
      // Child flake untrusted or suppressed — ignore, stay in parent.
      // Clear suppress if user cd'd back to parent from the suppressed child.
      if suppressed && !cwd.starts_with(nearest) {
        debug!("clearing suppress for nested flake");
        println!("{}", shell.unset("__XI_SUPPRESS"));
      }
      our_root.as_path()
    } else {
      // nearest == our_root — no nesting, clear any stale suppress
      if std::env::var("__XI_SUPPRESS").is_ok() {
        println!("{}", shell.unset("__XI_SUPPRESS"));
      }
      nearest.as_path()
    }
  } else {
    nearest.as_path()
  };

  // Case 4: Trust check on our flake (not a nested child)
  if !trust::is_trusted(effective_root) {
    debug!("flake untrusted, signaling exit");
    eprintln!(
      "{}",
      Notification::warn("devshell deactivated (untrusted)").render()
    );
    println!("exit {sentinel}");
    return Ok(());
  }

  // Case 5: Normal subshell operation — ensure daemon + query for re-source
  let t0 = std::time::Instant::now();
  let socket_path = match lifecycle::ensure(effective_root) {
    Ok(path) => {
      debug!(elapsed_ms = t0.elapsed().as_millis(), "daemon ensure");
      path
    },
    Err(e) => {
      debug!("daemon not available: {e}");
      return Ok(());
    },
  };

  let consumer_pid = resolve_pid(args);

  let t1 = std::time::Instant::now();
  let response = match client::prompt(
    &socket_path,
    consumer_pid,
    &args.target,
    &cwd_str,
    true,
    args.pid,
  ) {
    Ok(resp) => {
      debug!(
        elapsed_ms = t1.elapsed().as_millis(),
        source_env = resp.should_source_env,
        source_hook = resp.should_source_hook,
        notifications = resp.notifications.len(),
        "daemon response"
      );
      resp
    },
    Err(e) => {
      debug!("daemon call failed: {e}");
      return Ok(());
    },
  };

  render_notifications(&response);

  let output = generate_subshell_output(&response, shell, args);
  if !output.is_empty() {
    print!("{output}");
  }

  Ok(())
}

/// Exit mode: deregister consumer from daemon.
///
/// Called from the subshell's EXIT trap.
/// Best-effort: doesn't fail if daemon is down.
fn handle_exit(args: &PromptArgs) -> Result<()> {
  let consumer_pid = resolve_pid(args);
  let cwd = std::env::current_dir()?;

  // Find flake root to locate daemon socket
  if let Some(flake_root) = flake_detect::find_flake_root(&cwd) {
    let fid = dirs::flake_id(
      &std::fs::canonicalize(&flake_root)
        .unwrap_or_else(|_| flake_root.clone()),
    );
    let socket_path = dirs::daemon_socket_path(&fid);

    // Best-effort deregister
    match client::deregister(&socket_path, consumer_pid) {
      Ok(resp) => {
        debug!(
          pid = consumer_pid,
          remaining = resp.remaining_consumers,
          "deregistered from daemon"
        );
      },
      Err(e) => {
        debug!("deregister failed (daemon may be down): {e}");
      },
    }
  }

  Ok(())
}

/// Generate shell output from a `PromptResponse`.
///
/// This is the core shell code generation layer.
/// The daemon returns structured data; we convert to shell commands.
fn generate_subshell_output(
  resp: &PromptResponse,
  shell: ShellType,
  args: &PromptArgs,
) -> String {
  let mut lines = Vec::new();

  // Exit takes priority over everything
  if resp.should_exit {
    return format!("exit {}\n", subshell::EXIT_CD_OUT);
  }

  // Re-source env if needed
  if resp.should_source_env
    && let Some(ref path) = resp.env_file_path
  {
    lines.push(source_command(shell, path));
  }

  // Re-source hook if needed
  if resp.should_source_hook
    && let Some(ref path) = resp.hook_file_path
  {
    lines.push(source_command(shell, path));
  }

  // Install EXIT trap for deregistration (idempotent, safe to re-set)
  if resp.should_source_env || resp.should_source_hook {
    let pid = resolve_pid(args);
    let shell_name = shell.name();
    match shell {
      ShellType::Fish => {
        lines.push(format!(
          "function __xi_exit --on-event fish_exit; \
           $__XI_BIN develop prompt --exit -s {shell_name} --pid {pid}; \
           end"
        ));
      },
      ShellType::Bash | ShellType::Zsh => {
        lines.push(format!(
          "trap '$__XI_BIN develop prompt --exit -s {shell_name} --pid {pid}' EXIT"
        ));
      },
    }
  }

  if lines.is_empty() {
    String::new()
  } else {
    lines.join("\n") + "\n"
  }
}

/// Generate a source command for the given shell.
fn source_command(_shell: ShellType, path: &str) -> String {
  format!("source '{path}'")
}

/// Render notifications from daemon response to stderr.
fn render_notifications(response: &PromptResponse) {
  for notif in &response.notifications {
    eprintln!("{}", notif.render());
  }
}

/// Resolve the shell PID from args or fall back to parent PID.
fn resolve_pid(args: &PromptArgs) -> u32 {
  args.pid.unwrap_or_else(|| {
    #[cfg(unix)]
    {
      std::os::unix::process::parent_id()
    }
    #[cfg(not(unix))]
    {
      std::process::id()
    }
  })
}

/// Fast heuristic: check if flake.nix likely declares devShells.
///
/// Reads the flake.nix file and looks for keywords that indicate devShell
/// outputs (`devShells`, `devShell`, `mkShell`). This avoids showing
/// "trust" prompts for flakes that only have packages/modules/etc.
fn flake_likely_has_devshell(flake_root: &std::path::Path) -> bool {
  let flake_nix = flake_root.join("flake.nix");
  let Ok(contents) = std::fs::read_to_string(&flake_nix) else {
    return false;
  };
  contents.contains("devShells")
    || contents.contains("devShell")
    || contents.contains("mkShell")
}

/// Resolve the xi binary path (from persisted state or current exe).
fn resolve_xi_bin() -> String {
  let bin_path = dirs::state_base().join("xi-bin");
  std::fs::read_to_string(&bin_path).unwrap_or_else(|_| {
    std::env::current_exe()
      .map_or_else(|_| "xi".to_string(), |p| p.display().to_string())
  })
}
