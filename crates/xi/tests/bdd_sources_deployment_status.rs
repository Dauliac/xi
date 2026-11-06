//! BDD: `xi <scope> status` reports the `applied-origin` and
//! `inventory-role` state-dir markers, from
//! `openspec/changes/xi-deploy-unify-activation/specs/
//! xi-deploy-unify-activation/spec.md`.
//!
//! Drives the built `xi` binary, like its sibling
//! `bdd_sources_entrypoint.rs`, because the thing under test is the
//! command's JSON and human output, not the state-dir reader in
//! isolation.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{path::PathBuf, process::Command};

use xi_core::sources::write_state_file;

/// Path to the binary under test, as cargo built it for this run.
fn xi() -> PathBuf {
  let mut p = std::env::current_exe().expect("test exe path");
  p.pop(); // deps/
  p.pop(); // debug/
  p.push("xi");
  p
}

/// A user-scope machine rooted entirely inside a temp dir, with its
/// state dir seeded directly rather than through a real activation -
/// `record_activation_at`'s own writer is covered by
/// `xi-core::sources::switch`'s unit tests, so this only needs to
/// prove `DeploymentStatusArgs::run` reads what lands there.
struct Machine {
  #[expect(dead_code, reason = "held only to keep the tempdir alive")]
  tmp:   tempfile::TempDir,
  home:  PathBuf,
  state: PathBuf,
}

impl Machine {
  fn new() -> Self {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&home).expect("mkdir home");
    Self { tmp, home, state }
  }

  fn run(&self, args: &[&str]) -> String {
    let out = Command::new(xi())
      .args(args)
      .env("XI_NO_CHECKS", "1")
      .env("HOME", &self.home)
      .env("XDG_DATA_HOME", self.home.join(".local/share"))
      .env("XDG_STATE_HOME", self.home.join(".local/state"))
      .env("XDG_CONFIG_HOME", self.home.join(".config"))
      .env("XI_SOURCES_STATE_DIR", &self.state)
      .env_remove("XI_SOURCES_REMOTE")
      .current_dir(&self.home)
      .output()
      .expect("spawn xi");
    String::from_utf8_lossy(&out.stdout).into_owned()
  }

  fn status_json(&self) -> serde_json::Value {
    let stdout = self.run(&["home", "status", "--json"]);
    serde_json::from_str(&stdout).unwrap_or_else(|err| {
      panic!("status --json did not parse: {err}\n{stdout}")
    })
  }

  fn status_human(&self) -> String {
    self.run(&["home", "status"])
  }
}

/// A pushed target's envelope must report both markers.
#[test]
fn status_json_reports_applied_origin_and_inventory_role() {
  let m = Machine::new();
  write_state_file(&m.state, "applied-origin", "push:operator-host")
    .expect("seed applied-origin");
  write_state_file(&m.state, "inventory-role", "web1")
    .expect("seed inventory-role");

  let payload = m.status_json();

  assert_eq!(payload["applied_origin"], "push:operator-host");
  assert_eq!(payload["inventory_role"], "web1");
}

/// A daemon-ticked target carries no inventory role, since no push
/// named this host.
#[test]
fn status_json_reports_daemon_origin_with_no_inventory_role() {
  let m = Machine::new();
  write_state_file(&m.state, "applied-origin", "daemon")
    .expect("seed applied-origin");

  let payload = m.status_json();

  assert_eq!(payload["applied_origin"], "daemon");
  assert!(payload["inventory_role"].is_null());
}

/// Before any activation, both markers are absent and the envelope
/// says so rather than defaulting to a value nothing claimed.
#[test]
fn status_json_reports_null_markers_before_any_activation() {
  let m = Machine::new();

  let payload = m.status_json();

  assert!(payload["applied_origin"].is_null());
  assert!(payload["inventory_role"].is_null());
}

/// The human-readable report names both markers too, not only the
/// `--json` envelope.
///
/// Printed inside the non-`Missing` branch, so the sources dir has to
/// exist for the report to reach that branch at all - a bare
/// `mkdir`, with no generation recorded, is enough for `Pending`.
#[test]
fn status_human_reports_applied_origin_and_inventory_role() {
  let m = Machine::new();
  std::fs::create_dir_all(m.home.join(".local/share/xi/sources"))
    .expect("mkdir sources dir");
  write_state_file(&m.state, "applied-origin", "push:operator-host")
    .expect("seed applied-origin");
  write_state_file(&m.state, "inventory-role", "web1")
    .expect("seed inventory-role");

  let stdout = m.status_human();

  assert!(
    stdout.contains("applied_origin: push:operator-host"),
    "expected applied_origin in human output, got: {stdout}"
  );
  assert!(
    stdout.contains("inventory_role: web1"),
    "expected inventory_role in human output, got: {stdout}"
  );
}
