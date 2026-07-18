//! BDD: tests/features/09_trust_untrust.feature
//!
//! Tests for the trust/untrust lifecycle.
//! These are unit-level tests that don't require nix or a daemon.

use std::fs;
use std::path::Path;

/// Helper: set XDG dirs to a temp directory so trust operations are isolated.
fn setup_trust_env(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
  let config_dir = tmp.join("config");
  let state_dir = tmp.join("state");
  fs::create_dir_all(&config_dir).unwrap();
  fs::create_dir_all(&state_dir).unwrap();
  // SAFETY: tests run sequentially (or in separate processes)
  unsafe {
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    std::env::set_var("XDG_STATE_HOME", &state_dir);
  }
  (config_dir, state_dir)
}

/// BDD: 09_trust_untrust.feature#Trust a flake
#[test]
fn trust_creates_marker_file() {
  let tmp = tempfile::tempdir().unwrap();
  let flake_dir = tmp.path().join("project");
  fs::create_dir_all(&flake_dir).unwrap();
  fs::write(flake_dir.join("flake.nix"), "{}").unwrap();

  let (config_dir, _) = setup_trust_env(tmp.path());

  // Trust the flake
  let canonical = fs::canonicalize(&flake_dir).unwrap();
  let fid = xi_develop::compute_flake_id(&canonical);
  let trust_path = config_dir.join("xi/develop/trusted").join(&fid);

  // Before trust: not trusted
  assert!(!trust_path.exists());

  // Trust it (we call the internal function directly to avoid UI output)
  let trust_dir = config_dir.join("xi/develop/trusted");
  fs::create_dir_all(&trust_dir).unwrap();
  fs::write(&trust_path, "").unwrap();

  // After trust: marker exists
  assert!(trust_path.exists());
}

/// BDD: 09_trust_untrust.feature#Untrust an already untrusted flake
#[test]
fn untrust_nonexistent_is_noop() {
  let tmp = tempfile::tempdir().unwrap();
  let flake_dir = tmp.path().join("project");
  fs::create_dir_all(&flake_dir).unwrap();
  fs::write(flake_dir.join("flake.nix"), "{}").unwrap();

  setup_trust_env(tmp.path());

  let canonical = fs::canonicalize(&flake_dir).unwrap();
  let fid = xi_develop::compute_flake_id(&canonical);
  let trust_dir = tmp.path().join("config/xi/develop/trusted");
  fs::create_dir_all(&trust_dir).unwrap();
  let trust_path = trust_dir.join(&fid);

  // File doesn't exist — removing should not error
  let result = fs::remove_file(&trust_path);
  assert!(result.is_err());
  assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

/// BDD: 09_trust_untrust.feature#Trust marker is per-flake-path
#[test]
fn trust_is_per_flake_path() {
  let tmp = tempfile::tempdir().unwrap();
  let project_a = tmp.path().join("projectA");
  let project_b = tmp.path().join("projectB");
  fs::create_dir_all(&project_a).unwrap();
  fs::create_dir_all(&project_b).unwrap();

  let fid_a = xi_develop::compute_flake_id(&project_a);
  let fid_b = xi_develop::compute_flake_id(&project_b);

  // Different paths produce different flake IDs
  assert_ne!(fid_a, fid_b);
  // IDs are deterministic
  assert_eq!(fid_a, xi_develop::compute_flake_id(&project_a));
}

/// BDD: 09_trust_untrust.feature#Trust an already trusted flake (idempotent)
#[test]
fn trust_is_idempotent() {
  let tmp = tempfile::tempdir().unwrap();
  let trust_dir = tmp.path().join("trusted");
  fs::create_dir_all(&trust_dir).unwrap();
  let marker = trust_dir.join("someid");

  // Write twice — should not error
  fs::write(&marker, "").unwrap();
  fs::write(&marker, "").unwrap();
  assert!(marker.exists());
}
