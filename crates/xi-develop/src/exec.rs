//! `xi develop exec -- cmd args...`
//!
//! Run a command with the devshell environment applied.
//! Falls back to sync eval if daemon hasn't cached the env.
//! Uses `exec` (Unix) to replace the current process.

use std::path::Path;

use color_eyre::Result;

use crate::args::ExecArgs;
use crate::daemon::client;
use crate::{dirs, env_file, trust};

/// Execute a command with the devshell environment.
///
/// # Errors
///
/// Returns an error if the flake cannot be resolved, the environment
/// cannot be loaded, or the exec syscall fails.
pub fn exec(args: &ExecArgs) -> Result<()> {
  let flake_ref = args.flake.as_deref().unwrap_or(".");
  let flake_root = std::fs::canonicalize(flake_ref)?;

  // Trust check
  if !trust::is_trusted(&flake_root) {
    color_eyre::eyre::bail!(
      "Flake is not trusted. Run `xi develop trust` first."
    );
  }

  let target = &args.target;
  let dev_env = load_env(&flake_root, target)?;

  // Apply env vars to current process
  for (key, value) in &dev_env.env_vars {
    // SAFETY: single-threaded at this point, about to exec
    unsafe { std::env::set_var(key, value) };
  }

  // Build PATH: nix paths + original PATH
  if !dev_env.nix_paths.is_empty() {
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let nix_path = dev_env.nix_paths.join(":");
    unsafe { std::env::set_var("PATH", format!("{nix_path}:{orig_path}")) };
  }

  unsafe { std::env::set_var("IN_NIX_SHELL", "impure") };

  // exec into the command — replaces this process
  let cmd = &args.command[0];

  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt;

    let err = std::process::Command::new(cmd)
      .args(&args.command[1..])
      .exec();
    // exec() only returns on error
    color_eyre::eyre::bail!("exec {cmd}: {err}");
  }

  #[cfg(not(unix))]
  {
    let status = std::process::Command::new(cmd)
      .args(&args.command[1..])
      .status()?;
    std::process::exit(status.code().unwrap_or(1));
  }
}

/// Load the devshell environment, preferring nix's eval cache.
fn load_env(flake_root: &Path, target: &str) -> Result<env_file::DevEnv> {
  let fid = dirs::flake_id(flake_root);
  let socket_path = dirs::daemon_socket_path(&fid);

  // If daemon is running and ready, nix's eval cache will be warm,
  // making the sync eval below nearly instant.
  if client::is_alive(&socket_path)
    && let Ok(status) = client::status(&socket_path)
    && status.state == crate::daemon::protocol::DaemonState::Ready
  {
    tracing::debug!("Daemon ready — nix eval cache should be warm");
  }

  let fid = dirs::flake_id(flake_root);
  let state_dir = dirs::state_dir(&fid);
  let profile = dirs::profile_path(&state_dir, target);
  env_file::eval_devshell_niced(
    &flake_root.display().to_string(),
    target,
    Some(&profile),
  )
}
