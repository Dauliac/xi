//! BDD: contract for `xi agent devshell`
//! (specs/001-xi-agent/contracts/schema.md#Contract-3).
//!
//! The daemon-present + daemon-evaluating paths require a live
//! xi-develop socket and are covered by xi-develop's own bdd_* suite.
//! Here we lock the "socket absent" arm and the diagnostic factory —
//! both hermetic, both required by the contract.

use xi_agent::{
  args::DevshellArgs,
  devshell,
  schema::{DevshellState, Envelope, SCHEMA_V1},
};

fn args_default() -> DevshellArgs {
  DevshellArgs { timeout_ms: 500 }
}

/// Contract 3: "If the socket does not exist -> DevshellState::NotRunning
/// with a hint pointing at `xi develop`." A tempdir has no daemon by
/// construction, so this is the deterministic empty case.
#[test]
fn socket_absent_reports_not_running() {
  let tmp = tempfile::tempdir().unwrap();
  let payload = devshell::collect_at(&args_default(), tmp.path());
  assert!(matches!(payload.state, DevshellState::NotRunning));
  assert_eq!(payload.package_count, 0);
  assert!(payload.target.is_none());
  assert_eq!(payload.entered_command, "xi develop");
}

/// The `hint_not_running` diagnostic is the shape callers embed when
/// they want to surface the socket-absent condition inside another
/// envelope's `errors[]`. Consumers key on `code`, so it must not drift.
#[test]
fn hint_not_running_carries_stable_code_and_hint() {
  let d = devshell::hint_not_running();
  assert_eq!(d.code, "daemon.not-running");
  assert_eq!(d.hint.as_deref(), Some("run `xi develop` to start it"));
}

/// Envelope shape roundtrip for the devshell payload — anchors the
/// on-wire kebab-case rendering that agents parse.
#[test]
fn devshell_envelope_roundtrips() {
  let tmp = tempfile::tempdir().unwrap();
  let payload = devshell::collect_at(&args_default(), tmp.path());
  let env = Envelope::ok("devshell", payload, 0);
  let json = serde_json::to_string(&env).unwrap();
  assert!(json.contains("\"schema\":\"xi.agent/v1\""));
  assert!(json.contains("\"command\":\"devshell\""));
  assert!(json.contains("\"state\":\"not-running\""));
  let back: Envelope<xi_agent::schema::DevshellPayload> =
    serde_json::from_str(&json).unwrap();
  assert_eq!(back.schema, SCHEMA_V1);
}
