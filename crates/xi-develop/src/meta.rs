use std::{fs, path::Path};

use color_eyre::Result;
use serde::{Deserialize, Serialize};

use crate::store_path::PackageInfo;

/// Persistent metadata for a devshell evaluation.
/// Stored in `$XDG_STATE_HOME/xi/develop/{flake_id}/meta.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DevShellMeta {
  /// Hash of the devshell environment (for change detection).
  pub env_hash: String,
  /// The devshell target attribute.
  pub target: String,
  /// Canonical flake root path.
  pub flake_root: String,
  /// Store path of the devshell output (for dix diff).
  pub store_path: Option<String>,
  /// Parsed packages from PATH (for compact diff).
  pub packages: Vec<PackageInfo>,
  /// Last successful eval timestamp (unix seconds).
  pub timestamp: u64,
  /// Eval duration in milliseconds.
  pub eval_duration_ms: u64,
  /// SHA256 hash of flake.lock content (cache key for skipping eval).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub lock_hash: Option<String>,
  /// Composite hash of eval inputs (for eval cache fast-path).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub input_hash: Option<String>,
}

const META_FILE: &str = "meta.json";

/// Load metadata from a state directory.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed.
pub fn load(state_dir: &Path) -> Result<DevShellMeta> {
  let path = state_dir.join(META_FILE);
  let content = fs::read_to_string(&path).map_err(|e| {
    color_eyre::eyre::eyre!("Cannot read {}: {}", path.display(), e)
  })?;
  let meta: DevShellMeta = serde_json::from_str(&content).map_err(|e| {
    color_eyre::eyre::eyre!("Cannot parse {}: {}", path.display(), e)
  })?;
  Ok(meta)
}

/// Save metadata to a state directory.
///
/// # Errors
///
/// Returns an error if file I/O fails.
pub fn save(state_dir: &Path, meta: &DevShellMeta) -> Result<()> {
  fs::create_dir_all(state_dir)?;
  let path = state_dir.join(META_FILE);
  let content = serde_json::to_string_pretty(meta)?;
  fs::write(&path, content)?;
  Ok(())
}

/// Get current unix timestamp in seconds.
pub fn now_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}
