//! Rotating JSONL diagnostic sink at `$XDG_STATE_HOME/xi/hook.log`.
//!
//! Design.md § "Diagnostics to rotating log file only": every hook-side
//! diagnostic (transient outage, backoff, protocol renegotiation) goes here
//! — never to interactive stderr, which is reserved for user-actionable
//! trust prompts. This keeps the "invisible when working" contract intact.
//!
//! Rotation: single file, 1 MiB cap. On overrun the current file is renamed
//! to `hook.log.1` (overwriting any previous `.1`) and a fresh main file is
//! opened.
//!
//! Every write is best-effort: I/O errors are dropped with a `tracing::debug!`
//! so a broken disk never crashes the prompt tick.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::Result;
use serde_json::json;
use tracing::debug;

use crate::dirs::xi_state_root;

/// Rotation ceiling — 1 MiB per the design decision.
pub const ROTATE_AT_BYTES: u64 = 1024 * 1024;

/// Handle to the rotating diagnostic log.
///
/// Cheap to construct; owns a `Mutex<()>` used to serialise rotation on the
/// (unlikely) shared-handle path. Individual writes still open the file
/// fresh so concurrent shells don't need to coordinate.
#[derive(Debug)]
pub struct DiagLog {
  path:   PathBuf,
  rotate: Mutex<()>,
}

impl DiagLog {
  /// Open (or lazily create) the diagnostic log.
  ///
  /// The parent directory is created eagerly; the file itself is opened on
  /// first write.
  ///
  /// # Errors
  ///
  /// Returns an error only when the log directory cannot be created.
  pub fn open() -> Result<Self> {
    let path = log_path();
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }
    Ok(Self { path, rotate: Mutex::new(()) })
  }

  /// Path of the current log file.
  #[must_use]
  pub fn path(&self) -> &std::path::Path {
    &self.path
  }

  /// Emit one JSON-line diagnostic.
  ///
  /// Fields: `{ ts, level, event, flake_id, msg }`. `ts` is epoch
  /// milliseconds. On rotation the previous file is moved to
  /// `<path>.1` (overwriting) and a fresh main file is created.
  ///
  /// Errors are swallowed with a `debug!` — the diagnostic sink must never
  /// take the prompt down.
  pub fn write(&self, level: &str, event: &str, flake_id: &str, msg: &str) {
    let ts = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map(|d| d.as_millis())
      .unwrap_or(0);
    let line = json!({
      "ts":       ts,
      "level":    level,
      "event":    event,
      "flake_id": flake_id,
      "msg":      msg,
    })
    .to_string();

    if let Err(e) = self.write_line(&line) {
      debug!(error = %e, "hook diag_log write failed (swallowed)");
    }
  }

  fn write_line(&self, line: &str) -> Result<()> {
    // Rotate first if the current file is over the cap; then append.
    self.maybe_rotate()?;
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    let mut f = OpenOptions::new()
      .create(true)
      .append(true)
      .open(&self.path)?;
    f.write_all(buf.as_bytes())?;
    Ok(())
  }

  fn maybe_rotate(&self) -> Result<()> {
    let size = match fs::metadata(&self.path) {
      Ok(m) => m.len(),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
      Err(e) => return Err(e.into()),
    };
    if size < ROTATE_AT_BYTES {
      return Ok(());
    }

    // Serialise rotation on the (rare) contended path so two concurrent
    // writers don't both truncate.
    let _g = self.rotate.lock().ok();
    // Re-check under the lock — another writer may have just rotated.
    let size = match fs::metadata(&self.path) {
      Ok(m) => m.len(),
      Err(_) => return Ok(()),
    };
    if size < ROTATE_AT_BYTES {
      return Ok(());
    }

    let mut rotated = self.path.clone();
    let name = self
      .path
      .file_name()
      .map(|n| n.to_string_lossy().into_owned())
      .unwrap_or_else(|| String::from("hook.log"));
    rotated.set_file_name(format!("{name}.1"));
    // Overwrite any prior .1.
    let _ = fs::remove_file(&rotated);
    fs::rename(&self.path, &rotated)?;
    Ok(())
  }
}

/// Absolute path of the diagnostic log.
fn log_path() -> PathBuf {
  xi_state_root().join("hook.log")
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::MutexGuard;
  use tempfile::TempDir;

  struct StateHomeGuard {
    _td:   TempDir,
    _lock: MutexGuard<'static, ()>,
  }

  fn with_state_home() -> StateHomeGuard {
    let lock = crate::hook::test_env::XDG_LOCK
      .lock()
      .unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().expect("mktemp");
    // SAFETY: guarded by hook::test_env::XDG_LOCK for the duration.
    unsafe {
      std::env::set_var("XDG_STATE_HOME", td.path());
    }
    StateHomeGuard { _td: td, _lock: lock }
  }

  #[test]
  fn writes_then_rotates_at_threshold() {
    let td = with_state_home();
    let log = DiagLog::open().expect("open");

    // Pre-seed the file to just over the threshold.
    let path = log.path().to_path_buf();
    if let Some(p) = path.parent() {
      fs::create_dir_all(p).expect("mkdir");
    }
    let big = vec![b'x'; ROTATE_AT_BYTES as usize + 16];
    fs::write(&path, &big).expect("seed");

    // One more write must rotate the seed to .1 and start fresh.
    log.write("info", "test", "fid", "hello");

    let mut rotated = path.clone();
    let name = path.file_name().expect("name").to_string_lossy().into_owned();
    rotated.set_file_name(format!("{name}.1"));

    assert!(rotated.exists(), "rotated file should exist");
    let fresh = fs::read_to_string(&path).expect("read fresh");
    assert!(
      fresh.lines().count() == 1,
      "fresh log should have exactly one line, got {fresh:?}"
    );
    assert!(fresh.contains("\"event\":\"test\""));
    drop(td);
  }

  #[test]
  fn tolerates_missing_dir() {
    // Point XDG_STATE_HOME at a nonexistent subdirectory; open() must
    // create parents.
    let _lock = crate::hook::test_env::XDG_LOCK
      .lock()
      .unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().expect("mktemp");
    let nested = td.path().join("nested/deep");
    // SAFETY: guarded by XDG_LOCK.
    unsafe {
      std::env::set_var("XDG_STATE_HOME", &nested);
    }
    let log = DiagLog::open().expect("open on missing dir");
    log.write("warn", "created", "fid", "ok");
    assert!(log.path().exists());
  }

  #[test]
  fn write_survives_missing_parent_race() {
    // Even after the dir is nuked mid-run, write() must not panic.
    let _td = with_state_home();
    let log = DiagLog::open().expect("open");
    log.write("info", "first", "fid", "one");

    if let Some(p) = log.path().parent() {
      let _ = fs::remove_dir_all(p);
    }
    // Best-effort: should not crash even if the write itself fails.
    log.write("info", "second", "fid", "two");
  }
}
