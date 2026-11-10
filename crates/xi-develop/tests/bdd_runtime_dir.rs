//! BDD: openspec/changes/devshell-e2e-harness
//!   specs/xi-devshell-test-harness/spec.md
//!
//! The runtime-dir override and the socket-path length guard. This is the one
//! production seam the e2e harness needs: without it the daemon socket and pid
//! file live in the process-global `/tmp/xi-{uid}`, which no test can sandbox
//! or clean up by dropping a `TempDir`.
//!
//! Every branch is exercised through `runtime_dir_from`, which takes the
//! override explicitly. Mutating `XI_RUNTIME_DIR` in-process would be `unsafe`
//! in edition 2024 and would race across the multithreaded test harness.

use std::path::{Path, PathBuf};

use xi_develop::dirs;

/// BDD: xi-devshell-test-harness#Override redirects socket and pid file
#[test]
fn runtime_dir_override_is_honoured() {
  let dir = dirs::runtime_dir_from(Some("/tmp/xi-harness-test"));
  assert_eq!(dir, Path::new("/tmp/xi-harness-test"));
}

/// BDD: xi-devshell-test-harness#Override redirects socket and pid file
///
/// The override must carry through to the derived socket path, not just the
/// base directory, or a test world still cannot sandbox the daemon.
#[test]
fn runtime_dir_override_carries_into_socket_path() {
  let base = dirs::runtime_dir_from(Some("/tmp/xi-harness-test"));
  let fid = dirs::flake_id(Path::new("/some/flake"));
  let socket = base.join(&fid).join("daemon.sock");

  assert!(socket.starts_with("/tmp/xi-harness-test"));
  assert!(socket.ends_with("daemon.sock"));
  assert_ne!(
    socket.parent().and_then(Path::parent),
    Some(dirs::platform_runtime_dir().as_path()),
    "override must not fall back to the platform uid path"
  );
}

/// BDD: xi-devshell-test-harness#Unset override preserves today's behaviour
#[test]
fn runtime_dir_empty_override_is_treated_as_unset() {
  assert_eq!(
    dirs::runtime_dir_from(Some("")),
    dirs::platform_runtime_dir(),
    "an empty override must not yield a relative or empty runtime dir"
  );
}

/// BDD: xi-devshell-test-harness#Unset override preserves today's behaviour
///
/// Pins the pre-change value byte for byte, so the override cannot silently
/// relocate the real daemon for users who never set the variable.
#[test]
fn runtime_dir_unset_matches_pre_change_value() {
  let expected = format!("/tmp/xi-{}", nix::unistd::getuid().as_raw());

  assert_eq!(dirs::runtime_dir_from(None), Path::new(&expected));
  assert_eq!(dirs::platform_runtime_dir(), Path::new(&expected));
}

/// BDD: xi-devshell-test-harness#Over-long socket path is diagnosed
#[test]
fn over_long_socket_path_is_diagnosed() {
  let long = PathBuf::from(format!("/tmp/{}/daemon.sock", "d".repeat(200)));

  let err = dirs::check_socket_path_len(&long)
    .expect_err("a 200 character path must be rejected");
  let msg = err.to_string();

  assert!(msg.contains("103"), "error must name the limit, got: {msg}");
  assert!(
    msg.contains(&long.display().to_string()),
    "error must name the offending path, got: {msg}"
  );
  assert!(
    msg.contains("XI_RUNTIME_DIR"),
    "error must point at the knob that fixes it, got: {msg}"
  );
}

/// BDD: xi-devshell-test-harness#Over-long socket path is diagnosed
///
/// The guard must not reject the paths real users get, or it would break the
/// default install rather than only the pathological override.
#[test]
fn ordinary_socket_path_passes_length_check() {
  let fid = dirs::flake_id(Path::new("/home/user/project"));
  let ok = dirs::platform_runtime_dir().join(fid).join("daemon.sock");

  assert!(
    dirs::check_socket_path_len(&ok).is_ok(),
    "the default socket path must not trip the length check: {}",
    ok.display()
  );
}

/// BDD: xi-devshell-test-harness#Over-long socket path is diagnosed
///
/// A path exactly at the limit is bindable; one byte over is not. Pins the
/// boundary so a future refactor cannot drift it by one.
#[test]
fn socket_path_length_boundary_is_exact() {
  let at_limit = PathBuf::from("x".repeat(103));
  let over_limit = PathBuf::from("x".repeat(104));

  assert!(
    dirs::check_socket_path_len(&at_limit).is_ok(),
    "103 bytes must be accepted"
  );
  assert!(
    dirs::check_socket_path_len(&over_limit).is_err(),
    "104 bytes must be rejected"
  );
}
