use std::path::{Path, PathBuf};

/// Walk from `start` up to the filesystem root, collecting every directory
/// that contains a `flake.nix`.  The returned vector is ordered outermost-first
/// (i.e. the root-most flake comes first, the nearest/innermost comes last).
#[must_use]
pub fn find_flake_stack(start: &Path) -> Vec<PathBuf> {
  let mut flakes = Vec::new();
  let mut dir = start.to_path_buf();
  loop {
    if dir.join("flake.nix").exists() {
      flakes.push(dir.clone());
    }
    if !dir.pop() {
      break;
    }
  }
  // Collected innermost-first, reverse to get outermost-first
  flakes.reverse();
  flakes
}

/// Convenience wrapper: returns the nearest (innermost) flake root, if any.
#[must_use]
pub fn find_flake_root(start: &Path) -> Option<PathBuf> {
  find_flake_stack(start).last().cloned()
}
