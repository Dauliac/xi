//! Per-flake offline cache + reconnect backoff state.
//!
//! Design.md § "Offline cache validated primarily by `flake.lock` hash":
//! validity is `lock_hash == sha256(flake.lock) AND now - last_seen_at <
//! offline_cache_ttl`. Default TTL is 24 h — a floor on freshness, not a
//! hard expiry.
//!
//! Also holds the per-flake reconnect backoff counter (design.md § "Client
//! reconnect backoff — per-flake exponential"): first failure retries on
//! next prompt, subsequent failures wait `state_query × 2^n` capped at
//! `reconnect_backoff_cap`.
//!
//! On-disk layout: `$XDG_STATE_HOME/xi/hook-cache/<flake_id>.json`.
//! Written atomically via `.tmp` + rename.

use std::{
  fs,
  io::Write,
  path::PathBuf,
  time::{Duration, SystemTime},
};

use color_eyre::{Result, eyre::eyre};
use serde::{Deserialize, Serialize};

use crate::dirs::xi_state_root;

/// Backoff bookkeeping for a per-flake reconnect loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackoffState {
  /// Wall-clock at which the last daemon-facing operation failed.
  pub last_failed_at: SystemTime,
  /// Consecutive failure count; success resets to zero.
  pub attempt_count:  u32,
}

/// Persisted per-flake hook cache.
///
/// One JSON blob per flake. Contains just enough to answer a prompt tick
/// during a daemon outage plus the backoff state so a restart storm never
/// produces per-prompt reconnect spam.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineCache {
  /// Hex `sha256(flake.lock)` at the time the cache was written.
  pub lock_hash:       String,
  /// Wall-clock at which the daemon last replied successfully.
  pub last_seen_at:    SystemTime,
  /// Optional stable hash of the sourced env (used by consumers to detect
  /// re-source needs cheaply).
  pub env_hash:        Option<String>,
  /// Number of packages in the last observed devshell (for cheap status).
  pub package_count:   u32,
  /// Reconnect backoff — `None` when the daemon is healthy.
  pub reconnect_state: Option<BackoffState>,
}

impl OfflineCache {
  /// Load the cache for `flake_id`. Returns `None` when the file is absent
  /// or malformed (a malformed cache is silently discarded — the design
  /// treats the offline cache as best-effort, not authoritative).
  #[must_use]
  pub fn load(flake_id: &str) -> Option<Self> {
    let path = cache_path(flake_id);
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
  }

  /// Save the cache for `flake_id` atomically (`.tmp` + rename).
  ///
  /// # Errors
  ///
  /// Returns an error if the parent directory cannot be created or the
  /// write/rename fails.
  pub fn save(&self, flake_id: &str) -> Result<()> {
    let path = cache_path(flake_id);
    let parent = path
      .parent()
      .ok_or_else(|| eyre!("cache path has no parent"))?;
    fs::create_dir_all(parent)?;

    let mut tmp = path.clone();
    let name = path
      .file_name()
      .ok_or_else(|| eyre!("cache path has no file name"))?
      .to_string_lossy()
      .into_owned();
    tmp.set_file_name(format!("{name}.tmp"));

    let body = serde_json::to_string(self)?;
    {
      let mut f = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)?;
      f.write_all(body.as_bytes())?;
      // No `sync_all`: the offline cache is a hot-path write that fires on
      // every daemon-facing prompt tick. `sync_all` costs up to ~30 ms on
      // rotational disks — enough to blow the 100 ms tick budget. The
      // atomic-rename provides observable-write ordering, which is what
      // the design demands; a lost write across a hard reset is acceptable
      // because the flake.lock hash re-validates the cache on read.
    }
    fs::rename(&tmp, &path)?;
    Ok(())
  }

  /// Is this cache still trustworthy?
  ///
  /// True iff the lock hash matches the current flake's lock AND the cache
  /// is younger than `ttl`. Both conditions must hold — the TTL is a floor
  /// on freshness (see spec `Client — offline cache freshness`).
  #[must_use]
  pub fn is_valid(&self, current_lock_hash: &str, ttl: Duration) -> bool {
    if self.lock_hash != current_lock_hash {
      return false;
    }
    // `last_seen_at` in the future — clock skew, treat as fresh.
    SystemTime::now()
      .duration_since(self.last_seen_at)
      .map_or(true, |age| age < ttl)
  }

  /// Next reconnect delay for this flake's backoff loop.
  ///
  /// `state_query * 2^attempt_count`, clamped to `cap`. Attempts saturate
  /// at 62 to avoid overflow in the shift.
  #[must_use]
  pub fn next_backoff_delay(
    &self,
    state_query: Duration,
    cap: Duration,
  ) -> Duration {
    let attempts = self
      .reconnect_state
      .map_or(0u32, |s| s.attempt_count.min(62));
    // Compute in nanos (u128) so we don't lose precision or overflow.
    let base_nanos = state_query.as_nanos().min(u128::from(u64::MAX));
    let scaled = base_nanos.saturating_mul(1u128 << attempts);
    let cap_nanos = cap.as_nanos().min(u128::from(u64::MAX));
    let bounded = scaled.min(cap_nanos);
    // Cap value to u64::MAX nanos before converting back — Duration can
    // represent that without further loss.
    let secs = u64::try_from(bounded / 1_000_000_000).unwrap_or(u64::MAX);
    let nanos = u32::try_from(bounded % 1_000_000_000).unwrap_or(0);
    Duration::new(secs, nanos)
  }
}

/// Directory containing per-flake cache files.
fn cache_dir() -> PathBuf {
  xi_state_root().join("hook-cache")
}

fn cache_path(flake_id: &str) -> PathBuf {
  cache_dir().join(format!("{flake_id}.json"))
}

#[cfg(test)]
mod tests {
  use std::sync::MutexGuard;

  use tempfile::TempDir;

  use super::*;

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
    StateHomeGuard {
      _td:   td,
      _lock: lock,
    }
  }

  fn sample_cache() -> OfflineCache {
    OfflineCache {
      lock_hash:       "abc123".to_owned(),
      last_seen_at:    SystemTime::now(),
      env_hash:        Some("envhash".to_owned()),
      package_count:   42,
      reconnect_state: None,
    }
  }

  #[test]
  fn save_load_roundtrip() {
    let _td = with_state_home();
    let c = sample_cache();
    c.save("fid").expect("save");
    let loaded = OfflineCache::load("fid").expect("load");
    assert_eq!(loaded.lock_hash, c.lock_hash);
    assert_eq!(loaded.env_hash, c.env_hash);
    assert_eq!(loaded.package_count, 42);
  }

  #[test]
  fn is_valid_only_when_hash_and_ttl_hold() {
    let mut c = sample_cache();
    let long_ttl = Duration::from_secs(3600);
    // Same hash + fresh -> valid.
    assert!(c.is_valid("abc123", long_ttl));
    // Different hash -> invalid.
    assert!(!c.is_valid("otherhash", long_ttl));
    // Same hash but ancient -> invalid.
    c.last_seen_at = SystemTime::now() - Duration::from_secs(2 * 3600);
    assert!(!c.is_valid("abc123", long_ttl));
  }

  #[test]
  fn backoff_is_monotonic_and_capped() {
    let base = Duration::from_millis(20);
    let cap = Duration::from_secs(60);

    let mut c = sample_cache();

    // Attempt 0 -> exactly base.
    c.reconnect_state = Some(BackoffState {
      last_failed_at: SystemTime::now(),
      attempt_count:  0,
    });
    let d0 = c.next_backoff_delay(base, cap);
    assert_eq!(d0, base);

    // Monotonic across a few attempts.
    let mut prev = d0;
    for n in 1..8 {
      c.reconnect_state = Some(BackoffState {
        last_failed_at: SystemTime::now(),
        attempt_count:  n,
      });
      let cur = c.next_backoff_delay(base, cap);
      assert!(cur >= prev, "attempt {n}: {cur:?} !>= {prev:?}");
      prev = cur;
    }

    // Deep attempt saturates at cap.
    c.reconnect_state = Some(BackoffState {
      last_failed_at: SystemTime::now(),
      attempt_count:  40,
    });
    assert_eq!(c.next_backoff_delay(base, cap), cap);

    // Extreme attempt still clamps, no overflow.
    c.reconnect_state = Some(BackoffState {
      last_failed_at: SystemTime::now(),
      attempt_count:  u32::MAX,
    });
    assert_eq!(c.next_backoff_delay(base, cap), cap);
  }

  #[test]
  fn atomic_save_leaves_no_tmp_behind() {
    let _td = with_state_home();
    let c = sample_cache();
    c.save("fid2").expect("save");
    let dir = cache_dir();
    let tmp_files: Vec<_> = fs::read_dir(&dir)
      .expect("readdir")
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
      .collect();
    assert!(tmp_files.is_empty(), "leftover tmp: {tmp_files:?}");
  }
}
