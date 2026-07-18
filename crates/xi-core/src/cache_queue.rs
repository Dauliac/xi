//! Persistent cache push queue — survives failures and retries later.
//!
//! Queue file: `$XDG_STATE_HOME/xi/cache/queue.json`
//! Format: JSON array of [`QueueEntry`].

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::args::CacheTarget;

/// Configuration for the cache push queue.
#[derive(Debug, Clone)]
pub struct QueueConfig {
  /// Maximum entries in the queue (oldest dropped on overflow).
  pub max_size: usize,
  /// Entries older than this are expired (seconds).
  pub expiry_secs: u64,
  /// Seconds between automatic drain attempts (daemon only).
  pub drain_interval_secs: u64,
}

impl Default for QueueConfig {
  fn default() -> Self {
    Self {
      max_size: 100,
      expiry_secs: 7 * 24 * 3600, // 7 days
      drain_interval_secs: 300,   // 5 minutes
    }
  }
}

/// A pending cache push entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEntry {
  /// Nix store path to push.
  pub store_path: String,
  /// Cache target configuration.
  pub target: SerializableTarget,
  /// Unix timestamp when enqueued.
  pub enqueued_at: u64,
  /// Number of retry attempts so far.
  pub retry_count: u32,
  /// Last error message.
  pub last_error: String,
}

/// Serializable subset of `CacheTarget` for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableTarget {
  pub name: String,
  pub push_url: Option<String>,
  pub signing_key: Option<String>,
  pub push_command: Vec<String>,
}

impl From<&CacheTarget> for SerializableTarget {
  fn from(t: &CacheTarget) -> Self {
    Self {
      name: t.name.clone(),
      push_url: t.push_url.clone(),
      signing_key: t.signing_key.clone(),
      push_command: t.push_command.clone(),
    }
  }
}

impl From<&SerializableTarget> for CacheTarget {
  fn from(t: &SerializableTarget) -> Self {
    Self {
      name: t.name.clone(),
      push_url: t.push_url.clone(),
      signing_key: t.signing_key.clone(),
      push_command: t.push_command.clone(),
    }
  }
}

/// Resolve the queue file path.
fn queue_path() -> PathBuf {
  let state_home = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
    std::env::var("HOME")
      .map_or_else(|_| "/tmp".to_string(), |h| format!("{h}/.local/state"))
  });
  PathBuf::from(state_home).join("xi/cache/queue.json")
}

/// Current unix timestamp.
#[must_use]
pub fn now_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_secs())
}

/// Load the queue from disk. Returns empty vec if missing/corrupt.
#[must_use]
pub fn load() -> Vec<QueueEntry> {
  let path = queue_path();
  let Ok(content) = std::fs::read_to_string(&path) else {
    return vec![];
  };
  serde_json::from_str(&content).unwrap_or_else(|e| {
    debug!("[cache] queue parse error: {e}, resetting");
    vec![]
  })
}

/// Save the queue to disk.
fn save(entries: &[QueueEntry]) -> Result<()> {
  let path = queue_path();
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let json = serde_json::to_string_pretty(entries)?;
  std::fs::write(&path, json)?;
  Ok(())
}

/// Add a failed push to the persistent queue.
pub fn enqueue(
  store_path: &str,
  target: &CacheTarget,
  error: &str,
  config: &QueueConfig,
) {
  let mut entries = load();

  // Deduplicate: if same store_path + target name already queued, update it
  if let Some(existing) = entries
    .iter_mut()
    .find(|e| e.store_path == store_path && e.target.name == target.name)
  {
    existing.retry_count += 1;
    existing.last_error = error.to_string();
    if let Err(e) = save(&entries) {
      warn!("[cache] failed to save queue: {e}");
    }
    return;
  }

  entries.push(QueueEntry {
    store_path: store_path.to_string(),
    target: SerializableTarget::from(target),
    enqueued_at: now_secs(),
    retry_count: 0,
    last_error: error.to_string(),
  });

  // Enforce max size (drop oldest)
  if entries.len() > config.max_size {
    let overflow = entries.len() - config.max_size;
    entries.drain(..overflow);
  }

  if let Err(e) = save(&entries) {
    warn!("[cache] failed to save queue: {e}");
  }
}

/// Result of draining the queue.
pub struct DrainResult {
  pub succeeded: u32,
  pub failed: u32,
  pub expired: u32,
  pub missing: u32,
}

/// Drain the queue: retry all entries, remove succeeded/expired/missing.
///
/// Uses the provided `push_fn` to attempt the push, allowing both sync
/// and async callers to share the same drain logic.
pub fn drain(
  push_fn: &dyn Fn(&CacheTarget, &Path) -> Result<()>,
  config: &QueueConfig,
) -> DrainResult {
  let entries = load();
  if entries.is_empty() {
    return DrainResult {
      succeeded: 0,
      failed: 0,
      expired: 0,
      missing: 0,
    };
  }

  let now = now_secs();
  let mut remaining = Vec::new();
  let mut result = DrainResult {
    succeeded: 0,
    failed: 0,
    expired: 0,
    missing: 0,
  };

  for mut entry in entries {
    // Expire old entries
    if now.saturating_sub(entry.enqueued_at) > config.expiry_secs {
      debug!(
        "[cache] expiring queued push for {} ({})",
        entry.store_path, entry.target.name
      );
      result.expired += 1;
      continue;
    }

    // Check store path still exists
    let store_path = Path::new(&entry.store_path);
    if !store_path.exists() {
      debug!(
        "[cache] store path gone, removing from queue: {}",
        entry.store_path
      );
      result.missing += 1;
      continue;
    }

    // Try push
    let target = CacheTarget::from(&entry.target);
    match push_fn(&target, store_path) {
      Ok(()) => {
        info!(
          "[cache] queued push succeeded: {} → {}",
          entry.store_path, entry.target.name
        );
        result.succeeded += 1;
      },
      Err(e) => {
        debug!(
          "[cache] queued push still failing: {} → {}: {e}",
          entry.store_path, entry.target.name
        );
        entry.retry_count += 1;
        entry.last_error = format!("{e}");
        remaining.push(entry);
        result.failed += 1;
      },
    }
  }

  if let Err(e) = save(&remaining) {
    warn!("[cache] failed to save queue after drain: {e}");
  }

  result
}

/// Clear all entries from the queue.
pub fn clear() {
  if let Err(e) = save(&[]) {
    warn!("[cache] failed to clear queue: {e}");
  }
}

/// Return the number of pending entries (without loading full details).
#[must_use]
pub fn pending_count() -> usize {
  load().len()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn serializable_target_roundtrip() {
    let target = CacheTarget {
      name: "test".into(),
      push_url: Some("s3://bucket".into()),
      signing_key: None,
      push_command: vec![],
    };
    let ser = SerializableTarget::from(&target);
    let json = serde_json::to_string(&ser).unwrap();
    let deser: SerializableTarget = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.name, "test");
    assert_eq!(deser.push_url.as_deref(), Some("s3://bucket"));
  }

  #[test]
  fn queue_entry_roundtrip() {
    let entry = QueueEntry {
      store_path: "/nix/store/abc-pkg".into(),
      target: SerializableTarget {
        name: "mycache".into(),
        push_url: Some("s3://bucket".into()),
        signing_key: None,
        push_command: vec![],
      },
      enqueued_at: 1_700_000_000,
      retry_count: 2,
      last_error: "connection refused".into(),
    };
    let json = serde_json::to_string(&entry).unwrap();
    let deser: QueueEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.store_path, "/nix/store/abc-pkg");
    assert_eq!(deser.retry_count, 2);
  }

  #[test]
  fn default_config_has_sensible_values() {
    let config = QueueConfig::default();
    assert_eq!(config.max_size, 100);
    assert_eq!(config.expiry_secs, 7 * 24 * 3600);
    assert_eq!(config.drain_interval_secs, 300);
  }
}
