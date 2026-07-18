//! BDD: tests/features/09_trust_untrust.feature (flake ID)
//! BDD: tests/features/15_version_upgrade.feature (cache version)
//! BDD: tests/features/11_daemon_lifecycle.feature (state dirs)
//!
//! Tests for directory layout, flake ID, A/B slot switching, and cache versioning.

use std::fs;
use std::path::Path;

use xi_develop::dirs;

/// BDD: 09_trust_untrust.feature#Trust marker is per-flake-path
#[test]
fn flake_id_is_deterministic() {
  let id1 = dirs::flake_id(Path::new("/home/user/project"));
  let id2 = dirs::flake_id(Path::new("/home/user/project"));
  assert_eq!(id1, id2);
  assert_eq!(id1.len(), 16); // 8 bytes = 16 hex chars
}

/// BDD: 09_trust_untrust.feature#Trust marker is per-flake-path
#[test]
fn flake_id_differs_for_different_paths() {
  let id1 = dirs::flake_id(Path::new("/home/user/project-a"));
  let id2 = dirs::flake_id(Path::new("/home/user/project-b"));
  assert_ne!(id1, id2);
}

/// BDD: 15_version_upgrade.feature#Cache version mismatch nukes state
#[test]
fn cache_version_mismatch_nukes_state() {
  let tmp = tempfile::tempdir().unwrap();
  let state_dir = tmp.path().join("state");
  fs::create_dir_all(&state_dir).unwrap();

  // Write an old version
  fs::write(state_dir.join("VERSION"), "0").unwrap();
  // Write a file that should be nuked
  fs::write(state_dir.join("old-data.json"), "{}").unwrap();

  // Ensure cache version (current is 1, stored is 0)
  dirs::ensure_cache_version(&state_dir).unwrap();

  // Old data should be gone
  assert!(!state_dir.join("old-data.json").exists());
  // VERSION file should have the new version
  let version = fs::read_to_string(state_dir.join("VERSION")).unwrap();
  assert_eq!(version.trim(), dirs::CACHE_VERSION.to_string());
}

/// BDD: 15_version_upgrade.feature#Cache version match preserves state
#[test]
fn cache_version_match_preserves_state() {
  let tmp = tempfile::tempdir().unwrap();
  let state_dir = tmp.path().join("state");
  fs::create_dir_all(&state_dir).unwrap();

  // Write the current version
  fs::write(state_dir.join("VERSION"), dirs::CACHE_VERSION.to_string())
    .unwrap();
  // Write a file that should be preserved
  fs::write(state_dir.join("meta.json"), "{\"test\":true}").unwrap();

  dirs::ensure_cache_version(&state_dir).unwrap();

  // Data should still be there
  assert!(state_dir.join("meta.json").exists());
}

/// BDD: 03_live_update_package.feature (A/B slot switching)
#[test]
fn ab_slot_atomic_switch() {
  let tmp = tempfile::tempdir().unwrap();
  let state_dir = tmp.path();

  // Write first version
  dirs::write_and_switch(state_dir, "env-default", "sh", "# version 1\n")
    .unwrap();

  let link = dirs::current_link(state_dir, "env-default", "sh");
  assert!(link.exists() || fs::read_link(&link).is_ok());
  let content = fs::read_to_string(&link).unwrap();
  assert_eq!(content, "# version 1\n");

  // Write second version — should switch to other slot
  dirs::write_and_switch(state_dir, "env-default", "sh", "# version 2\n")
    .unwrap();

  let content = fs::read_to_string(&link).unwrap();
  assert_eq!(content, "# version 2\n");

  // Both slot files should exist
  assert!(state_dir.join("env-default.a.sh").exists());
  assert!(state_dir.join("env-default.b.sh").exists());
}

/// BDD: 03_live_update_package.feature (generation counter)
#[test]
fn generation_counter_increments() {
  let tmp = tempfile::tempdir().unwrap();
  let state_dir = tmp.path();

  // No file yet
  let gen_path = state_dir.join("env-generation");
  assert!(!gen_path.exists());

  // Bump once
  dirs::bump_env_generation(state_dir);
  let val: u64 = fs::read_to_string(&gen_path)
    .unwrap()
    .trim()
    .parse()
    .unwrap();
  assert_eq!(val, 1);

  // Bump again
  dirs::bump_env_generation(state_dir);
  let val: u64 = fs::read_to_string(&gen_path)
    .unwrap()
    .trim()
    .parse()
    .unwrap();
  assert_eq!(val, 2);
}

/// BDD: 04_live_update_hook.feature (separate hook generation)
#[test]
fn hook_generation_independent_of_env() {
  let tmp = tempfile::tempdir().unwrap();
  let state_dir = tmp.path();

  dirs::bump_env_generation(state_dir);
  dirs::bump_env_generation(state_dir);
  dirs::bump_hook_generation(state_dir);

  let env_gen: u64 = fs::read_to_string(state_dir.join("env-generation"))
    .unwrap()
    .trim()
    .parse()
    .unwrap();
  let hook_gen: u64 = fs::read_to_string(state_dir.join("hook-generation"))
    .unwrap()
    .trim()
    .parse()
    .unwrap();

  assert_eq!(env_gen, 2);
  assert_eq!(hook_gen, 1);
}
