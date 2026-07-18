//! Daemon lifecycle — start, stop, ensure, stale detection.
//!
//! The socket is the ONLY source of truth for liveness (cimera pattern).
//! PID file is advisory.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::Result;
use tracing::debug;

use super::client;
use crate::dirs;

const PID_FILE: &str = "daemon.pid";
const STARTUP_WAIT_MS: u64 = 100;
const STARTUP_RETRIES: u32 = 30; // 30 × 100ms = 3s max wait

/// `PID` file content: `pid:version:binary_hash`
struct PidInfo {
  pid: u32,
  version: String,
}

/// Ensure daemon is running. Start it if not.
///
/// # Errors
/// Returns an error if the daemon cannot be started.
pub fn ensure(flake_root: &Path) -> Result<PathBuf> {
  let fid = dirs::flake_id(&fs::canonicalize(flake_root)?);
  let socket_path = dirs::daemon_socket_path(&fid);

  // Already running and healthy?
  if client::is_alive(&socket_path) {
    // Check version
    if let Ok(status) = client::status(&socket_path) {
      if status.version == env!("CARGO_PKG_VERSION") {
        debug!("Daemon already running (pid check via socket)");
        return Ok(socket_path);
      }
      // Version mismatch — restart silently
      let _ = client::shutdown(&socket_path);
      std::thread::sleep(Duration::from_millis(200));
    }
  }

  // Cleanup stale socket/pid
  cleanup_stale(&fid);

  // Start daemon
  start(flake_root, &fid)?;

  // Wait for socket to appear
  for _ in 0..STARTUP_RETRIES {
    if client::is_alive(&socket_path) {
      debug!("Daemon started successfully");
      return Ok(socket_path);
    }
    std::thread::sleep(Duration::from_millis(STARTUP_WAIT_MS));
  }

  color_eyre::eyre::bail!(
    "Daemon failed to start within {}s",
    u64::from(STARTUP_RETRIES) * STARTUP_WAIT_MS / 1000
  )
}

/// Start the daemon as a background process.
///
/// # Errors
/// Returns an error if the process cannot be spawned.
fn start(flake_root: &Path, flake_id: &str) -> Result<()> {
  let nh_bin = resolve_nh_bin();
  let runtime_dir = dirs::daemon_runtime_dir(flake_id);
  fs::create_dir_all(&runtime_dir)?;

  // Read develop config from xi config.toml
  let (eval_interval, watch_extra, eval_cache) = read_develop_config();

  let mut args = vec![
    "develop".to_string(),
    "daemon".to_string(),
    "start".to_string(),
    "--flake".to_string(),
    flake_root.display().to_string(),
    "--eval-interval".to_string(),
    eval_interval.to_string(),
    "--eval-cache".to_string(),
    eval_cache,
  ];
  for pattern in &watch_extra {
    args.push("--watch-extra".to_string());
    args.push(pattern.clone());
  }

  let child = std::process::Command::new(&nh_bin)
    .args(&args)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()?;

  // Write PID file
  let pid_content = format!("{}:{}", child.id(), env!("CARGO_PKG_VERSION"));
  fs::write(runtime_dir.join(PID_FILE), pid_content)?;

  Ok(())
}

/// Stop the daemon.
///
/// # Errors
/// Returns an error if shutdown fails.
pub fn stop(flake_root: &Path) -> Result<()> {
  let fid = dirs::flake_id(&fs::canonicalize(flake_root)?);
  let socket_path = dirs::daemon_socket_path(&fid);

  if client::is_alive(&socket_path) {
    eprintln!(
      "{}",
      crate::daemon::protocol::Notification::info(
        "devshell daemon stopping..."
      )
      .render()
    );
    client::shutdown(&socket_path)?;
    // Wait for socket to disappear
    for _ in 0..20 {
      if !socket_path.exists() {
        return Ok(());
      }
      std::thread::sleep(Duration::from_millis(100));
    }
  }

  // Force cleanup
  cleanup_stale(&fid);
  eprintln!(
    "{}",
    crate::daemon::protocol::Notification::success("devshell daemon stopped")
      .render()
  );
  Ok(())
}

/// Clean up stale runtime files.
fn cleanup_stale(flake_id: &str) {
  let runtime_dir = dirs::daemon_runtime_dir(flake_id);

  // Try to kill stale PID
  if let Ok(content) = fs::read_to_string(runtime_dir.join(PID_FILE))
    && let Some(pid_str) = content.split(':').next()
    && let Ok(pid) = pid_str.parse::<u32>()
  {
    #[cfg(unix)]
    {
      unsafe {
        libc::kill(pid.cast_signed(), libc::SIGTERM);
      }
    }
    debug!("Sent SIGTERM to stale daemon pid {pid}");
  }

  // Remove runtime files
  let _ = fs::remove_file(runtime_dir.join(PID_FILE));
  let _ = fs::remove_file(dirs::daemon_socket_path(flake_id));
}

/// Read [develop] config from xi config.toml (best-effort).
fn read_develop_config() -> (u64, Vec<String>, String) {
  let config_path = std::env::var("XI_CONFIG").map_or_else(
    |_| {
      let config_home = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
          .map_or_else(|_| "/tmp".to_string(), |h| format!("{h}/.config"))
      });
      std::path::PathBuf::from(config_home).join("xi/config.toml")
    },
    std::path::PathBuf::from,
  );

  let Ok(content) = fs::read_to_string(&config_path) else {
    return (5, vec![], "lock".to_string());
  };

  // Minimal TOML parsing — just extract [develop] values
  let mut eval_interval = 5u64;
  let mut watch_extra = Vec::new();
  let mut eval_cache = "lock".to_string();
  let mut in_develop = false;

  for line in content.lines() {
    let trimmed = line.trim();
    if trimmed == "[develop]" {
      in_develop = true;
      continue;
    }
    if trimmed.starts_with('[') {
      in_develop = false;
      continue;
    }
    if !in_develop {
      continue;
    }
    if let Some(val) = trimmed.strip_prefix("eval_interval")
      && let Some(val) = val.trim().strip_prefix('=')
      && let Ok(n) = val.trim().parse::<u64>()
    {
      eval_interval = n.max(1);
    }
    if let Some(val) = trimmed.strip_prefix("watch_extra")
      && let Some(val) = val.trim().strip_prefix('=')
    {
      let val = val.trim();
      // Parse TOML array: ["*.yaml", "version.txt"]
      if val.starts_with('[') {
        let inner = val.trim_start_matches('[').trim_end_matches(']');
        for item in inner.split(',') {
          let item = item.trim().trim_matches('"').trim_matches('\'');
          if !item.is_empty() {
            watch_extra.push(item.to_string());
          }
        }
      }
    }
    if let Some(val) = trimmed.strip_prefix("eval_cache")
      && let Some(val) = val.trim().strip_prefix('=')
    {
      let val = val.trim().trim_matches('"').trim_matches('\'');
      if !val.is_empty() {
        eval_cache = val.to_string();
      }
    }
  }

  (eval_interval, watch_extra, eval_cache)
}

fn resolve_nh_bin() -> String {
  // Read from persisted xi-bin file
  let bin_path = dirs::state_base().join("xi-bin");
  fs::read_to_string(&bin_path).unwrap_or_else(|_| {
    // Fallback to current exe
    std::env::current_exe()
      .map_or_else(|_| "xi".to_string(), |p| p.display().to_string())
  })
}
