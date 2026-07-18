//! File watcher — watches nix-relevant git-tracked files + user-configured extras.
//!
//! Default: watches staged `*.nix`, `flake.lock`, `flake.nix` files.
//! Users can extend with `[develop.watch]` in `xi.toml`:
//!
//! ```toml
//! [develop.watch]
//! extra = ["*.yaml", "version.txt", "Cargo.lock"]
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use color_eyre::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, info};

/// Default patterns — only the flake entry points.
/// Users extend via [develop] `watch_extra` in config.toml.
const DEFAULT_PATTERNS: &[&str] = &["flake.nix", "flake.lock"];

/// Event sent when watched files change.
#[derive(Debug)]
pub struct FileChangeEvent {
  pub paths: Vec<PathBuf>,
}

/// Git-aware file watcher.
pub struct GitWatcher {
  _watcher: RecommendedWatcher,
  pub rx: mpsc::Receiver<FileChangeEvent>,
}

impl GitWatcher {
  /// Create a new watcher for a flake root.
  ///
  /// By default watches staged files matching `*.nix` and `flake.lock`.
  /// Extra patterns can be provided (from xi.toml config).
  ///
  /// # Errors
  ///
  /// Returns an error if git2 or notify setup fails.
  pub fn new(flake_root: &Path, extra_patterns: &[String]) -> Result<Self> {
    let (event_tx, event_rx) = mpsc::channel();

    // Collect dirs containing nix-relevant git-tracked files
    let watched_dirs = git_tracked_nix_dirs(flake_root, extra_patterns)?;

    info!(
      "Watching {} directories in {}",
      watched_dirs.len(),
      flake_root.display()
    );

    // Set up notify watcher
    let mut watcher = notify::recommended_watcher(
      move |res: std::result::Result<notify::Event, notify::Error>| {
        if let Ok(event) = res
          && (event.kind.is_modify()
            || event.kind.is_create()
            || event.kind.is_remove())
        {
          let _ = event_tx.send(FileChangeEvent { paths: event.paths });
        }
      },
    )?;

    // Watch each unique directory
    for dir in &watched_dirs {
      if dir.exists()
        && let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive)
      {
        debug!("Cannot watch {}: {e}", dir.display());
      }
    }

    // Watch .git/index for staging changes (git add/rm)
    let git_index = flake_root.join(".git/index");
    if git_index.exists()
      && let Err(e) = watcher.watch(&git_index, RecursiveMode::NonRecursive)
    {
      debug!("Cannot watch .git/index: {e}");
    }

    Ok(Self {
      _watcher: watcher,
      rx: event_rx,
    })
  }

  /// Try to receive a change event (non-blocking).
  #[must_use]
  pub fn try_recv(&self, timeout: Duration) -> Option<FileChangeEvent> {
    self.rx.recv_timeout(timeout).ok()
  }
}

/// Check if a file path matches the watch patterns.
fn matches_patterns(path: &str, extra_patterns: &[String]) -> bool {
  // Extract filename from path
  let filename = path.rsplit('/').next().unwrap_or(path);

  // Default: only flake.nix and flake.lock (exact filename match)
  for pat in DEFAULT_PATTERNS {
    if filename == *pat {
      return true;
    }
  }

  // User extras from config.toml [develop] watch_extra
  for pat in extra_patterns {
    if pat.starts_with("*.") {
      // Extension match: "*.yaml" matches "foo.yaml"
      let ext = &pat[1..]; // ".yaml"
      if path.ends_with(ext) {
        return true;
      }
    } else if filename == pat.as_str() || path.ends_with(pat.as_str()) {
      // Exact filename match: "version.txt", "Cargo.lock"
      return true;
    }
  }

  false
}

/// Collect unique parent directories of nix-relevant git-tracked files.
fn git_tracked_nix_dirs(
  flake_root: &Path,
  extra_patterns: &[String],
) -> Result<Vec<PathBuf>> {
  let repo = git2::Repository::open(flake_root).map_err(|e| {
    color_eyre::eyre::eyre!(
      "Cannot open git repo at {}: {e}",
      flake_root.display()
    )
  })?;

  let index = repo
    .index()
    .map_err(|e| color_eyre::eyre::eyre!("Cannot read git index: {e}"))?;

  let mut dirs: HashSet<PathBuf> = HashSet::new();

  for entry in index.iter() {
    let path_str = String::from_utf8_lossy(&entry.path);
    if matches_patterns(&path_str, extra_patterns) {
      let full_path = flake_root.join(path_str.as_ref());
      if let Some(parent) = full_path.parent() {
        dirs.insert(parent.to_path_buf());
      }
    }
  }

  // Always include flake root (for flake.nix, flake.lock)
  dirs.insert(flake_root.to_path_buf());

  Ok(dirs.into_iter().collect())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_patterns() {
    // Only flake.nix and flake.lock match by default
    assert!(matches_patterns("flake.nix", &[]));
    assert!(matches_patterns("flake.lock", &[]));
    assert!(matches_patterns("subdir/flake.nix", &[]));
    // Other .nix files do NOT match by default
    assert!(!matches_patterns("shell.nix", &[]));
    assert!(!matches_patterns("modules/foo.nix", &[]));
    assert!(!matches_patterns("src/main.rs", &[]));
    assert!(!matches_patterns("Cargo.toml", &[]));
  }

  #[test]
  fn extra_extension_pattern() {
    let extras = vec!["*.yaml".to_string()];
    assert!(matches_patterns("config.yaml", &extras));
    assert!(matches_patterns("deep/dir/foo.yaml", &extras));
    assert!(!matches_patterns("config.json", &extras));
  }

  #[test]
  fn extra_exact_filename() {
    let extras = vec!["version.txt".to_string(), "Cargo.lock".to_string()];
    assert!(matches_patterns("version.txt", &extras));
    assert!(matches_patterns("some/dir/version.txt", &extras));
    assert!(matches_patterns("Cargo.lock", &extras));
    assert!(!matches_patterns("README.md", &extras));
  }

  #[test]
  fn nix_files_always_match() {
    let extras = vec!["*.yaml".to_string()];
    assert!(matches_patterns("flake.nix", &extras));
    assert!(matches_patterns("flake.lock", &extras));
  }
}
