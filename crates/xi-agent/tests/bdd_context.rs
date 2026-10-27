//! BDD: contract for `xi agent context`
//! (specs/001-xi-agent/contracts/schema.md#Contract-1).
//!
//! Every assertion here comes directly from the contract:
//!   - exactly one Envelope<AgentContext> is produced
//!   - the envelope is valid JSON round-trippable through serde_json
//!   - schema == "xi.agent/v1", command == "context"
//!   - `--no-daemon` forces `DevshellState::NotRunning` and does not touch the
//!     daemon socket
//!   - a parseable envelope is always emitted (even under errors[]), which —
//!     together with emit() writing only to stdout — is the library-level
//!     guarantee behind the "exit 0 on any parseable response" contract.

use std::fs;

use xi_agent::{
  args::ContextArgs,
  context,
  schema::{AgentContext, DevshellState, Envelope, SCHEMA_V1},
};

fn no_daemon_args() -> ContextArgs {
  ContextArgs {
    system:    Some("x86_64-linux".into()),
    no_daemon: true,
  }
}

/// Fresh fixture directory: no flake, no git — the "empty workspace" edge.
///
/// Failure-mode contract (quickstart.md): "Not in a flake" surfaces a
/// `workspace.not-a-flake` diagnostic and still emits a well-formed
/// envelope. This test anchors that guarantee.
#[test]
fn context_envelope_is_well_formed_on_empty_workspace() {
  let tmp = tempfile::tempdir().unwrap();
  let env = context::build(tmp.path(), &no_daemon_args());

  assert_eq!(env.schema, SCHEMA_V1);
  assert_eq!(env.command, "context");
  assert!(env.data.is_some(), "data present even on empty workspace");
  let data = env.data.as_ref().unwrap();
  assert_eq!(data.workspace.root, tmp.path());
  assert_eq!(data.workspace.current_system, "x86_64-linux");
  // No flake.nix in the fixture — flake summary must be absent, not error.
  assert!(data.flake.is_none());
  assert!(
    env.errors.iter().any(|d| d.code == "workspace.not-a-flake"),
    "expected workspace.not-a-flake diagnostic, got {:?}",
    env.errors
  );
}

/// `--no-daemon` short-circuits to NotRunning without hitting the socket.
///
/// If this test ever hangs, the flag was ignored — the daemon lookup is
/// synchronous and would block on socket connect.
#[test]
fn no_daemon_forces_not_running() {
  let tmp = tempfile::tempdir().unwrap();
  let env = context::build(tmp.path(), &no_daemon_args());
  let data = env.data.as_ref().unwrap();
  assert!(matches!(data.devshell.state, DevshellState::NotRunning));
  assert_eq!(data.devshell.package_count, 0);
  assert_eq!(data.devshell.entered_command, "xi develop");
}

/// A flake.nix fixture makes flake summary populate.
///
/// Marked #[ignore] because `context::build` also runs
/// `nix flake show` on the fixture and any fixture we can write
/// deterministically triggers a network-backed evaluation. The other
/// tests already cover the "flake is None" arm — this one is here for
/// documentation of the pairing between fixture and `flake` payload.
#[test]
#[ignore = "requires nix and network — run with --ignored for integration"]
fn flake_summary_populates_when_flake_present() {
  let tmp = tempfile::tempdir().unwrap();
  fs::write(tmp.path().join("flake.nix"), "{ outputs = { self }: { }; }")
    .unwrap();
  let env = context::build(tmp.path(), &no_daemon_args());
  let data = env.data.as_ref().unwrap();
  let flake = data.flake.as_ref().expect("flake summary present");
  assert_eq!(flake.path, tmp.path().join("flake.nix"));
}

/// Envelope round-trips through serde_json — the on-wire shape.
///
/// Also verifies kebab-case rendering of the two enum-heavy fields
/// consumers rely on for parsing (`devshell.state`, `install.action`
/// covered separately in bdd_schema_stability.rs — here we just anchor
/// the context path).
#[test]
fn context_envelope_roundtrips_as_json() {
  let tmp = tempfile::tempdir().unwrap();
  let env = context::build(tmp.path(), &no_daemon_args());
  let json = serde_json::to_string(&env).expect("envelope serializes");
  assert!(json.contains("\"schema\":\"xi.agent/v1\""));
  assert!(json.contains("\"command\":\"context\""));
  assert!(json.contains("\"state\":\"not-running\""));
  let back: Envelope<AgentContext> =
    serde_json::from_str(&json).expect("envelope parses");
  assert_eq!(back.schema, env.schema);
  assert_eq!(back.command, env.command);
  assert_eq!(back.data.is_some(), env.data.is_some());
}
