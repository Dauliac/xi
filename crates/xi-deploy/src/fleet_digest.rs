//! The `fleet-digest` cache: a hash of the last resolved target set,
//! backend, and build-location decisions.
//!
//! Cached next to `.xi.toml` itself, deliberately NOT inside any
//! scope's state dir  -  it is an operator-side, repo-adjacent cache,
//! not per-scope daemon state. A repeat fleet rollout whose digest
//! still matches skips re-evaluation and reuses the cached decisions
//! directly (`Facts::fleet_digest_unchanged` in [`crate::decision`]).
//!
//! This module owns the cache file's shape and its hash input, plus
//! the two wiring points layered on top of it: task 4.2 writes it
//! after a successful rollout ([`crate::push::orchestrate`]), and task
//! 4.3 reads it back via [`resolve_fleet_digest_unchanged`] to decide
//! whether the decision tree can skip re-evaluation.

use std::{
  fs,
  io,
  path::{Path, PathBuf},
};

use xi_core::sources::cache_prestage::sha256_hex;

/// Filename of the cache file, sibling to `.xi.toml`.
pub const FLEET_DIGEST_FILENAME: &str = ".xi-fleet-digest";

/// The facts a fleet-digest hashes: the resolved target set, the
/// chosen backend, and the resolved build-location decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetDigestInput {
  /// The resolved target set for the rollout. Order doesn't matter  -
  /// [`FleetDigestInput::digest`] canonicalizes it.
  pub targets:        Vec<String>,
  /// The chosen deployment backend's name (e.g. `"deploy-rs"`,
  /// `"colmena"`, `"builtin"`).
  pub backend:        String,
  /// The resolved build-location decision (e.g. the build-location
  /// ladder's verdict: local vs. remote build).
  pub build_location: String,
}

impl FleetDigestInput {
  /// Hash this input into a stable digest string.
  ///
  /// Targets are sorted before hashing: a fleet-digest is a hash of a
  /// resolved target *set*, so two inputs naming the same targets in a
  /// different order must hash identically.
  #[must_use]
  pub fn digest(&self) -> String {
    let mut targets = self.targets.clone();
    targets.sort();
    let canonical = format!(
      "targets={}\nbackend={}\nbuild_location={}",
      targets.join(","),
      self.backend,
      self.build_location
    );
    sha256_hex(&canonical)
  }
}

/// Resolve the fleet-digest cache path sibling to a given `.xi.toml`
/// path (the same path shape `SourcesConfig::from_toml_file` and
/// `PushConfig::from_toml_file` already take).
#[must_use]
pub fn cache_path_for(xi_toml_path: &Path) -> PathBuf {
  xi_toml_path
    .parent()
    .unwrap_or_else(|| Path::new(""))
    .join(FLEET_DIGEST_FILENAME)
}

/// Load the cached digest string, if a cache file exists next to
/// `xi_toml_path`. A missing cache file is not an error: it is simply
/// the cache-miss case callers check for with [`is_unchanged`].
///
/// # Errors
///
/// Returns an error if the cache file exists but cannot be read.
pub fn load(xi_toml_path: &Path) -> io::Result<Option<String>> {
  match fs::read_to_string(cache_path_for(xi_toml_path)) {
    Ok(contents) => Ok(Some(contents.trim().to_owned())),
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(e),
  }
}

/// Persist `input`'s digest to the cache file next to `xi_toml_path`,
/// overwriting whatever was cached before.
///
/// # Errors
///
/// Returns an error if the cache file cannot be written.
pub fn store(xi_toml_path: &Path, input: &FleetDigestInput) -> io::Result<()> {
  fs::write(cache_path_for(xi_toml_path), input.digest())
}

/// Whether `input`'s digest matches what's cached next to
/// `xi_toml_path`  -  a cache hit. Any mismatch, or no cache file at
/// all, is a cache miss.
#[must_use]
pub fn is_unchanged(xi_toml_path: &Path, input: &FleetDigestInput) -> bool {
  matches!(load(xi_toml_path), Ok(Some(cached)) if cached == input.digest())
}

/// Resolve `decision::Facts::fleet_digest_unchanged` (task 4.3): whether
/// the `fleet-digest` cached next to `xi_toml_path` still matches the
/// target set, backend, and build-location a fleet rollout would
/// otherwise have to rediscover from scratch.
///
/// This is the one fact in [`crate::decision::Facts`] that requires
/// reading the filesystem, so it is wired here  -  alongside [`load`]
/// and [`store`]  -  rather than inside `decision` itself, which stays
/// filesystem-free. A caller recomputes `targets`/`backend`/
/// `build_location` the same way a rollout would (e.g. from the
/// current inventory) and passes them in; any difference from what was
/// cached at the last successful rollout is a miss, which is exactly
/// what forces the tree to re-evaluate instead of reusing the cache.
#[must_use]
pub fn resolve_fleet_digest_unchanged(
  xi_toml_path: &Path,
  targets: &[String],
  backend: &str,
  build_location: &str,
) -> bool {
  is_unchanged(xi_toml_path, &FleetDigestInput {
    targets:        targets.to_vec(),
    backend:        backend.to_owned(),
    build_location: build_location.to_owned(),
  })
}

#[cfg(test)]
mod tests {
  use tempfile::tempdir;

  use super::*;

  fn input(
    targets: &[&str],
    backend: &str,
    build_location: &str,
  ) -> FleetDigestInput {
    FleetDigestInput {
      targets:        targets.iter().map(|t| (*t).to_owned()).collect(),
      backend:        backend.to_owned(),
      build_location: build_location.to_owned(),
    }
  }

  #[test]
  fn cache_path_is_sibling_to_xi_toml() {
    let path = Path::new("/home/me/dotfiles/.xi.toml");
    assert_eq!(
      cache_path_for(path),
      Path::new("/home/me/dotfiles/.xi-fleet-digest")
    );
  }

  #[test]
  fn digest_is_order_independent_over_targets() {
    let a = input(&["web1", "web2"], "deploy-rs", "remote");
    let b = input(&["web2", "web1"], "deploy-rs", "remote");
    assert_eq!(a.digest(), b.digest());
  }

  #[test]
  fn digest_changes_when_backend_differs() {
    let a = input(&["web1"], "deploy-rs", "remote");
    let b = input(&["web1"], "colmena", "remote");
    assert_ne!(a.digest(), b.digest());
  }

  #[test]
  fn digest_changes_when_build_location_differs() {
    let a = input(&["web1"], "deploy-rs", "remote");
    let b = input(&["web1"], "deploy-rs", "local");
    assert_ne!(a.digest(), b.digest());
  }

  #[test]
  fn digest_changes_when_target_set_differs() {
    let a = input(&["web1"], "deploy-rs", "remote");
    let b = input(&["web1", "web2"], "deploy-rs", "remote");
    assert_ne!(a.digest(), b.digest());
  }

  #[test]
  fn no_cache_file_is_a_miss() {
    let dir = tempdir().expect("tempdir");
    let xi_toml = dir.path().join(".xi.toml");
    let facts = input(&["web1"], "deploy-rs", "remote");
    assert!(load(&xi_toml).expect("read").is_none());
    assert!(!is_unchanged(&xi_toml, &facts));
  }

  #[test]
  fn storing_then_checking_the_same_input_is_a_hit() {
    let dir = tempdir().expect("tempdir");
    let xi_toml = dir.path().join(".xi.toml");
    let facts = input(&["web1", "web2"], "deploy-rs", "remote");

    store(&xi_toml, &facts).expect("store");

    assert!(is_unchanged(&xi_toml, &facts));
    assert!(
      dir.path().join(FLEET_DIGEST_FILENAME).exists(),
      "cache file must live next to .xi.toml, not in a scope state dir"
    );
  }

  #[test]
  fn a_changed_input_after_a_cached_store_is_a_miss() {
    let dir = tempdir().expect("tempdir");
    let xi_toml = dir.path().join(".xi.toml");
    let cached = input(&["web1"], "deploy-rs", "remote");
    let changed = input(&["web1", "web2"], "deploy-rs", "remote");

    store(&xi_toml, &cached).expect("store");

    assert!(!is_unchanged(&xi_toml, &changed));
  }

  #[test]
  fn storing_again_overwrites_the_previous_digest() {
    let dir = tempdir().expect("tempdir");
    let xi_toml = dir.path().join(".xi.toml");
    let first = input(&["web1"], "deploy-rs", "remote");
    let second = input(&["web1", "web2"], "colmena", "local");

    store(&xi_toml, &first).expect("store first");
    store(&xi_toml, &second).expect("store second");

    assert!(!is_unchanged(&xi_toml, &first));
    assert!(is_unchanged(&xi_toml, &second));
  }

  // -- resolve_fleet_digest_unchanged (xi-fxgx.4.3) -----------------------

  #[test]
  fn resolve_fleet_digest_unchanged_is_a_hit_on_the_same_inventory() {
    let dir = tempdir().expect("tempdir");
    let xi_toml = dir.path().join(".xi.toml");
    store(&xi_toml, &input(&["web1", "web2"], "deploy-rs", "remote"))
      .expect("store");

    assert!(resolve_fleet_digest_unchanged(
      &xi_toml,
      &["web1".to_owned(), "web2".to_owned()],
      "deploy-rs",
      "remote",
    ));
  }

  #[test]
  fn resolve_fleet_digest_unchanged_is_a_miss_on_a_changed_inventory() {
    let dir = tempdir().expect("tempdir");
    let xi_toml = dir.path().join(".xi.toml");
    store(&xi_toml, &input(&["web1"], "deploy-rs", "remote")).expect("store");

    // A target added to the inventory since the last cached rollout.
    assert!(!resolve_fleet_digest_unchanged(
      &xi_toml,
      &["web1".to_owned(), "web2".to_owned()],
      "deploy-rs",
      "remote",
    ));
  }

  #[test]
  fn resolve_fleet_digest_unchanged_is_a_miss_with_no_cache_file() {
    let dir = tempdir().expect("tempdir");
    let xi_toml = dir.path().join(".xi.toml");

    assert!(!resolve_fleet_digest_unchanged(
      &xi_toml,
      &["web1".to_owned()],
      "deploy-rs",
      "remote",
    ));
  }
}
