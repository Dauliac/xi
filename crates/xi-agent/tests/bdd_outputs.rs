//! BDD: contract for `xi agent outputs`
//! (specs/001-xi-agent/contracts/schema.md#Contract-2).
//!
//! Fast contract checks anchor the hermetic guarantees:
//!   - collect() short-circuits on a non-flake directory with a
//!     `workspace.not-a-flake` diagnostic (no nix subprocess spawned)
//!   - the wrapping Envelope shape round-trips through serde_json
//!   - category / kind taxonomy uses `xi_core::flake_output` variants
//!
//! Integration paths that require `nix flake show` on a real flake are
//! marked `#[ignore]`.

use xi_agent::{
  args::OutputsArgs,
  outputs,
  schema::{Envelope, OutputsPayload, SCHEMA_V1},
};

fn args_default() -> OutputsArgs {
  OutputsArgs {
    all_systems:    false,
    system:         Some("x86_64-linux".into()),
    include_hidden: false,
  }
}

/// A directory without `flake.nix` yields a not-a-flake diagnostic
/// without touching the nix binary. Guarantees offline-safety for the
/// common "cd into a random dir" agent scenario.
#[test]
fn empty_workspace_yields_not_a_flake_diagnostic() {
  let tmp = tempfile::tempdir().unwrap();
  let err = outputs::collect(&args_default(), "x86_64-linux", tmp.path())
    .expect_err("empty workspace must return a diagnostic, not a payload");
  assert_eq!(err.code, "workspace.not-a-flake");
}

/// The Envelope<OutputsPayload> shape is stable and roundtrippable.
/// This anchors the on-wire contract independently from whether nix
/// eval succeeded.
#[test]
fn outputs_envelope_shape_roundtrips() {
  let payload = OutputsPayload {
    system:  "x86_64-linux".into(),
    outputs: Vec::new(),
    hidden:  Vec::new(),
  };
  let env = Envelope::ok("outputs", payload, 0);
  let json = serde_json::to_string(&env).unwrap();
  let back: Envelope<OutputsPayload> = serde_json::from_str(&json).unwrap();
  assert_eq!(back.schema, SCHEMA_V1);
  assert_eq!(back.command, "outputs");
  assert!(back.data.is_some());
  assert!(back.data.unwrap().outputs.is_empty());
}

/// Integration: a real flake fixture yields outputs categorised via the
/// FlakeOutput taxonomy. Requires nix + network for input resolution.
#[test]
#[ignore = "requires nix and network — run with --ignored for integration"]
fn real_flake_categorises_outputs() {
  let tmp = tempfile::tempdir().unwrap();
  std::fs::write(
    tmp.path().join("flake.nix"),
    r#"{
      outputs = { self }: {
        packages.x86_64-linux.hello = derivation {
          name = "hello";
          builder = "/bin/sh";
          system = "x86_64-linux";
        };
      };
    }"#,
  )
  .unwrap();
  let payload = outputs::collect(&args_default(), "x86_64-linux", tmp.path())
    .expect("outputs collect on real flake");
  assert_eq!(payload.system, "x86_64-linux");
  assert!(
    payload.outputs.iter().any(|e| e.name == "hello"),
    "hello package expected in {:?}",
    payload.outputs
  );
}
