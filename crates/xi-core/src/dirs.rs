//! Shared XDG Base Directory helpers for xi.
//!
//! Provides consistent path resolution for cache, state, and config
//! directories across all xi crates.

use std::path::PathBuf;

/// Resolve the xi cache directory (`$XDG_CACHE_HOME/xi` or `~/.cache/xi`).
#[must_use]
pub fn xdg_cache_dir() -> PathBuf {
  xdg_dir("XDG_CACHE_HOME", ".cache")
}

/// Resolve the xi state directory (`$XDG_STATE_HOME/xi` or
/// `~/.local/state/xi`).
#[must_use]
pub fn xdg_state_dir() -> PathBuf {
  xdg_dir("XDG_STATE_HOME", ".local/state")
}

fn xdg_dir(env_key: &str, home_fallback: &str) -> PathBuf {
  if let Ok(dir) = std::env::var(env_key)
    && !dir.is_empty()
  {
    return PathBuf::from(dir).join("xi");
  }

  if let Ok(home) = std::env::var("HOME")
    && !home.is_empty()
  {
    return PathBuf::from(home).join(home_fallback).join("xi");
  }

  PathBuf::from("/tmp").join("xi")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cache_dir_contains_xi() {
    let dir = xdg_cache_dir();
    assert!(
      dir.ends_with("xi"),
      "cache dir should end with 'xi': {}",
      dir.display()
    );
  }

  #[test]
  fn state_dir_contains_xi() {
    let dir = xdg_state_dir();
    assert!(
      dir.ends_with("xi"),
      "state dir should end with 'xi': {}",
      dir.display()
    );
  }
}
