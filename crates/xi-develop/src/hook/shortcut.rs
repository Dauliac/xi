//! Local shortcut path — skip the daemon round-trip when we're already
//! sitting in a trusted devshell whose lock hash is unchanged.
//!
//! From design.md § "Local shortcut path when already in the trusted
//! subshell": the client skips the daemon round-trip when ALL of:
//!
//! 1. `__XI_IN_DEVSHELL=1`
//! 2. `pwd` starts with `$__XI_FLAKE_ROOT`
//! 3. `$__XI_LOCK_HASH` matches the last successful reply
//! 4. Age since last reply is under `shortcut_max_age_ms` (default 5 s)
//!
//! Violating any of (1-3) means we cannot answer from the local env; (1-3)
//! but not (4) means we should send only a lightweight `HeartBeat`. This
//! module returns a boolean; the caller decides between "answer locally"
//! and "send heartbeat" based on the age.

use std::{
  path::{Path, PathBuf},
  time::{Duration, SystemTime},
};

/// Freshness ceiling for the local shortcut path.
///
/// If the last successful daemon reply is older than this, callers must
/// still consult the daemon (`Query::HeartBeat`) even when env conditions
/// (1-3) all hold.
pub const SHORTCUT_MAX_AGE: Duration = Duration::from_secs(5);

/// Env var: set to `"1"` inside a xi-managed devshell.
pub const ENV_IN_DEVSHELL: &str = "__XI_IN_DEVSHELL";
/// Env var: absolute path of the flake root this devshell was entered from.
pub const ENV_FLAKE_ROOT: &str = "__XI_FLAKE_ROOT";
/// Env var: `flake.lock` hash captured at devshell entry.
pub const ENV_LOCK_HASH: &str = "__XI_LOCK_HASH";

/// Decide whether the prompt tick may answer locally without a daemon
/// round-trip.
///
/// Reads `__XI_IN_DEVSHELL`, `__XI_FLAKE_ROOT`, and `__XI_LOCK_HASH` from
/// the process env; combines with the caller-supplied `last_lock_hash` /
/// `last_reply_at` to test all four preconditions.
///
/// `last_lock_hash` and `last_reply_at` typically come from the persisted
/// offline cache (see [`crate::hook::offline`]).
#[must_use]
pub fn should_shortcut(
  cwd: &Path,
  last_lock_hash: Option<&str>,
  last_reply_at: Option<SystemTime>,
) -> bool {
  // (1) __XI_IN_DEVSHELL == "1"
  let in_devshell = std::env::var(ENV_IN_DEVSHELL).ok().as_deref() == Some("1");
  if !in_devshell {
    return false;
  }

  // (2) cwd starts with $__XI_FLAKE_ROOT
  let Ok(flake_root) = std::env::var(ENV_FLAKE_ROOT) else {
    return false;
  };
  let root = PathBuf::from(&flake_root);
  if !cwd.starts_with(&root) {
    return false;
  }

  // (3) $__XI_LOCK_HASH == last_lock_hash
  let Ok(env_lock) = std::env::var(ENV_LOCK_HASH) else {
    return false;
  };
  let Some(last) = last_lock_hash else {
    return false;
  };
  if env_lock != last {
    return false;
  }

  // (4) Age under SHORTCUT_MAX_AGE
  let Some(reply_at) = last_reply_at else {
    return false;
  };
  // `reply_at` in the future — treat as fresh (clock skew tolerance).
  SystemTime::now()
    .duration_since(reply_at)
    .map_or(true, |age| age < SHORTCUT_MAX_AGE)
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use super::*;

  // Env is process-global; serialise these tests to avoid cross-test races.
  static ENV_LOCK: Mutex<()> = Mutex::new(());

  fn clear_env() {
    // SAFETY: guarded by ENV_LOCK; no other test in this module touches
    // these vars without holding the guard.
    unsafe {
      std::env::remove_var(ENV_IN_DEVSHELL);
      std::env::remove_var(ENV_FLAKE_ROOT);
      std::env::remove_var(ENV_LOCK_HASH);
    }
  }

  #[test]
  fn denied_without_env() {
    let _g = ENV_LOCK.lock().expect("env lock");
    clear_env();
    assert!(!should_shortcut(
      Path::new("/tmp/proj"),
      Some("hash"),
      Some(SystemTime::now())
    ));
  }

  #[test]
  fn denied_when_cwd_outside_flake_root() {
    let _g = ENV_LOCK.lock().expect("env lock");
    clear_env();
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
      std::env::set_var(ENV_IN_DEVSHELL, "1");
      std::env::set_var(ENV_FLAKE_ROOT, "/tmp/proj");
      std::env::set_var(ENV_LOCK_HASH, "hash");
    }
    assert!(!should_shortcut(
      Path::new("/tmp/elsewhere"),
      Some("hash"),
      Some(SystemTime::now()),
    ));
    clear_env();
  }

  #[test]
  fn denied_when_lock_hash_mismatch() {
    let _g = ENV_LOCK.lock().expect("env lock");
    clear_env();
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
      std::env::set_var(ENV_IN_DEVSHELL, "1");
      std::env::set_var(ENV_FLAKE_ROOT, "/tmp/proj");
      std::env::set_var(ENV_LOCK_HASH, "hash-current");
    }
    assert!(!should_shortcut(
      Path::new("/tmp/proj/sub"),
      Some("hash-old"),
      Some(SystemTime::now()),
    ));
    clear_env();
  }

  #[test]
  fn denied_when_reply_is_stale() {
    let _g = ENV_LOCK.lock().expect("env lock");
    clear_env();
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
      std::env::set_var(ENV_IN_DEVSHELL, "1");
      std::env::set_var(ENV_FLAKE_ROOT, "/tmp/proj");
      std::env::set_var(ENV_LOCK_HASH, "hash");
    }
    let old = SystemTime::now() - (SHORTCUT_MAX_AGE + Duration::from_secs(1));
    assert!(!should_shortcut(
      Path::new("/tmp/proj/sub"),
      Some("hash"),
      Some(old),
    ));
    clear_env();
  }

  #[test]
  fn granted_when_all_four_preconditions_hold() {
    let _g = ENV_LOCK.lock().expect("env lock");
    clear_env();
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
      std::env::set_var(ENV_IN_DEVSHELL, "1");
      std::env::set_var(ENV_FLAKE_ROOT, "/tmp/proj");
      std::env::set_var(ENV_LOCK_HASH, "hash");
    }
    assert!(should_shortcut(
      Path::new("/tmp/proj/sub/dir"),
      Some("hash"),
      Some(SystemTime::now()),
    ));
    clear_env();
  }
}
