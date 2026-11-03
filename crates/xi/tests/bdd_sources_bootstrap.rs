//! BDD: `xi system uninstall`, the CLI wiring for the
//! `xi-sources-bootstrap` capability's reversal path
//! (`openspec/changes/xi-sources-bootstrap/specs/xi-sources-bootstrap/spec.md`,
//! requirement "Receipt file").
//!
//! The planner, preflight, and reversal engine are unit-tested against
//! [`RecordingRunner`] in `xi_core::sources::bootstrap`; what is
//! specific to the binary crate, and therefore worth driving through
//! the compiled `xi` here instead, is the CLI wiring itself: that
//! `xi system uninstall` exists, resolves the receipt through the same
//! `XI_SOURCES_STATE_DIR` override the rest of the sources family
//! honours, refuses cleanly with no receipt, and actually reverses a
//! real one on disk end to end.
//!
//! [`RecordingRunner`]: xi_core::sources::bootstrap::RecordingRunner

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{fs, path::PathBuf, process::Command};

use tempfile::TempDir;

/// Path to the binary under test, as cargo built it for this run.
fn xi() -> PathBuf {
  let mut p = std::env::current_exe().expect("test exe path");
  p.pop(); // deps/
  p.pop(); // debug/
  p.push("xi");
  p
}

fn run(args: &[&str], state_dir: &std::path::Path) -> (String, bool) {
  let out = Command::new(xi())
    .args(args)
    // Keep the probe off the real machine's state.
    .env("XI_NO_CHECKS", "1")
    .env("XI_SOURCES_STATE_DIR", state_dir)
    .output()
    .expect("spawn xi");
  let merged = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  (merged, out.status.success())
}

/// The receipt path for the system scope: one level above the state
/// dir, per `xi_core::sources::bootstrap::receipt_dir`.
fn receipt_path(state_dir: &std::path::Path) -> PathBuf {
  state_dir
    .parent()
    .expect("state dir has a parent")
    .join("bootstrap-receipt.json")
}

// -- Requirement: `xi system uninstall` exists -----------------------------

/// Scenario: the subcommand is discoverable and documents its contract.
#[test]
fn uninstall_is_a_system_subcommand() {
  let tmp = TempDir::new().expect("tempdir");
  let (out, ok) = run(&["system", "--help"], &tmp.path().join("state"));
  assert!(ok, "`xi system --help` failed: {out}");
  assert!(
    out.contains("uninstall"),
    "`xi system --help` should list uninstall, got: {out}"
  );
}

// -- Requirement: Receipt file ----------------------------------------------

/// Scenario: uninstalling an unmanaged host says so.
#[test]
fn uninstall_without_a_receipt_refuses_clearly() {
  let tmp = TempDir::new().expect("tempdir");
  let state_dir = tmp.path().join("state");
  fs::create_dir_all(&state_dir).expect("mkdir state");

  let (out, ok) = run(&["system", "uninstall"], &state_dir);
  assert!(!ok, "uninstall with no receipt must fail, got: {out}");
  assert!(
    out.contains("no bootstrap receipt") && out.contains("uninstall"),
    "expected the NoReceipt message, got: {out}"
  );
}

/// Scenario: Full round trip is reversible, driven through the real
/// binary rather than the library.
///
/// Builds a receipt naming three phases - a no-op preflight, a
/// `clone-sources` that owns a scratch directory, and the closing
/// `write-receipt` - and checks that running the compiled `xi system
/// uninstall` against it removes exactly those two paths and reports
/// all three phases reversed, in `uninstall`'s documented
/// last-applied-first order.
#[test]
fn uninstall_reverses_a_real_receipt_end_to_end() {
  let tmp = TempDir::new().expect("tempdir");
  let state_dir = tmp.path().join("state");
  fs::create_dir_all(&state_dir).expect("mkdir state");

  let cloned = tmp.path().join("was-cloned");
  fs::create_dir_all(cloned.join(".git")).expect("mkdir cloned");
  fs::write(cloned.join("marker"), b"adopted").expect("write marker");

  let receipt = receipt_path(&state_dir);
  fs::write(
    &receipt,
    format!(
      r#"{{
  "schema_version": 1,
  "scope": "system",
  "remote": "https://git.example/cfg",
  "entries": [
    {{
      "action": "check-preflight",
      "applied_at": "2026-01-01T00:00:00Z",
      "reversal": "nothing",
      "note": "preflight only read the host"
    }},
    {{
      "action": "clone-sources",
      "applied_at": "2026-01-01T00:00:01Z",
      "reversal": "remove-path",
      "path": {cloned:?}
    }},
    {{
      "action": "write-receipt",
      "applied_at": "2026-01-01T00:00:02Z",
      "reversal": "remove-path",
      "path": {receipt:?}
    }}
  ]
}}
"#,
      cloned = cloned.display().to_string(),
      receipt = receipt.display().to_string(),
    ),
  )
  .expect("write receipt fixture");

  let (out, ok) = run(&["system", "uninstall"], &state_dir);
  assert!(ok, "uninstall must succeed on a reversible receipt: {out}");
  assert!(
    out.contains("reversed 3 bootstrap action"),
    "expected a count of 3 reversed actions, got: {out}"
  );

  assert!(!cloned.exists(), "the cloned sources dir must be gone");
  assert!(!receipt.exists(), "the receipt itself must be gone last");
}

/// Scenario: a reversal that fails leaves the receipt in place so a
/// retry can pick up where it stopped.
#[test]
fn a_failing_reversal_leaves_the_receipt_for_a_retry() {
  let tmp = TempDir::new().expect("tempdir");
  let state_dir = tmp.path().join("state");
  fs::create_dir_all(&state_dir).expect("mkdir state");

  let receipt = receipt_path(&state_dir);
  fs::write(
    &receipt,
    r#"{
  "schema_version": 1,
  "scope": "system",
  "entries": [
    {
      "action": "install-nix",
      "applied_at": "2026-01-01T00:00:00Z",
      "reversal": "run",
      "argv": ["sh", "-c", "exit 1"]
    }
  ]
}
"#,
  )
  .expect("write receipt fixture");

  let (out, ok) = run(&["system", "uninstall"], &state_dir);
  assert!(!ok, "a failing reversal must surface as a failure: {out}");
  assert!(receipt.is_file(), "the receipt must survive a failed reversal");
}
