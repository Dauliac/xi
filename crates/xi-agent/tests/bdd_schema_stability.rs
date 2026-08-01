//! BDD: schema envelope stability across serde round-trips.
//!
//! Contract: every envelope emitted by xi-agent must round-trip through
//! serde without loss of shape. Snapshot-quality — this suite catches
//! accidental field renames or enum tag drift before release.

use std::path::PathBuf;

use chrono::TimeZone as _;
use xi_agent::schema::*;

fn fixed_time() -> chrono::DateTime<chrono::Utc> {
  chrono::Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap()
}

/// Envelope carries a schema and a payload; must round-trip.
#[test]
fn envelope_roundtrip_context() {
  let ctx = AgentContext {
    workspace:  WorkspaceInfo {
      root:           PathBuf::from("/tmp/proj"),
      current_system: "x86_64-linux".into(),
      xi_version:     "4.4.0".into(),
    },
    flake:      Some(FlakeSummary {
      path:      PathBuf::from("/tmp/proj/flake.nix"),
      lock_hash: Some("abc".into()),
      systems:   vec!["x86_64-linux".into()],
    }),
    devshell:   DevshellPayload {
      state:               DevshellState::Ready,
      target:              Some(".#devShells.x86_64-linux.default".into()),
      package_count:       42,
      daemon_state:        Some("Ready".into()),
      active_cache_pushes: vec![],
      entered_command:     "xi develop".into(),
    },
    git:        GitState {
      head:            Some("deadbeef".into()),
      branch:          Some("master".into()),
      dirty:           true,
      untracked_count: 3,
      ahead_behind:    Some((1, 0)),
    },
    validation: ValidationPlan {
      steps: vec![ValidationStep {
        id:         "fmt-check".into(),
        command:    vec!["xi".into(), "fmt".into(), "--check".into()],
        purpose:    "verify formatting is stable".into(),
        blocking:   true,
        depends_on: vec![],
      }],
    },
    xi_config:  None,
  };
  let mut env = Envelope::ok("context", ctx, 42);
  env.generated = fixed_time();
  let json = serde_json::to_string(&env).unwrap();
  let back: Envelope<AgentContext> = serde_json::from_str(&json).unwrap();
  assert_eq!(back.schema, SCHEMA_V1);
  assert_eq!(back.command, "context");
  assert_eq!(back.duration_ms, 42);
  assert!(back.data.is_some());
}

/// Devshell payload emits kebab-case for its enum + fields.
#[test]
fn devshell_payload_kebab_case() {
  let payload = DevshellPayload {
    state:               DevshellState::NotRunning,
    target:              None,
    package_count:       0,
    daemon_state:        None,
    active_cache_pushes: vec![],
    entered_command:     "xi develop".into(),
  };
  let json = serde_json::to_string(&payload).unwrap();
  assert!(json.contains("\"state\":\"not-running\""));
  assert!(json.contains("\"entered-command\":\"xi develop\""));
  assert!(!json.contains("\"entered_command\""));
}

/// Validation event is a tagged enum keyed on `event`.
#[test]
fn validation_event_is_tagged() {
  let ev = ValidationEvent::Finished {
    id:          "cargo-test".into(),
    status:      ValidationStatus::Passed,
    duration_ms: 1234,
    error:       None,
  };
  let json = serde_json::to_string(&ev).unwrap();
  assert!(json.contains("\"event\":\"finished\""));
  assert!(json.contains("\"status\":\"passed\""));
}

/// A failing envelope has empty data but non-empty errors.
#[test]
fn failing_envelope_has_errors() {
  let diag = Diagnostic::new("flake.eval.failed", "boom")
    .with_source("nix flake show")
    .with_hint("re-run with --show-trace");
  let env: Envelope<AgentContext> = Envelope::fail("context", diag, 7);
  let json = serde_json::to_string(&env).unwrap();
  assert!(json.contains("\"flake.eval.failed\""));
  assert!(json.contains("\"data\":null"));
  assert!(json.contains("\"re-run with --show-trace\""));
}

/// Schema string is stable and versioned.
#[test]
fn schema_string_versioned() {
  assert!(SCHEMA_V1.starts_with("xi.agent/v"));
  assert!(SCHEMA_V1.ends_with("1"));
}

/// Install action is a tagged enum on `action`.
#[test]
fn install_action_tagged() {
  let a = InstallAction::UpToDate;
  let json = serde_json::to_string(&a).unwrap();
  assert!(json.contains("\"action\":\"up-to-date\""));
  let b = InstallAction::Skipped {
    reason: "permission-denied".into(),
  };
  let json = serde_json::to_string(&b).unwrap();
  assert!(json.contains("\"action\":\"skipped\""));
  assert!(json.contains("\"reason\":\"permission-denied\""));
}
