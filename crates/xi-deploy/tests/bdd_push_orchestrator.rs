//! BDD: `xi push` end to end through the real orchestrator
//! (`crates/xi-deploy/src/push/orchestrate.rs`), against a real local git
//! fixture standing in for the operator's sources dir.
//!
//! `discover`/`preflight`/`engine`/`mode_sources` already have their own
//! unit and BDD coverage (round 1/2); what this file proves is that
//! [`xi_deploy::push::orchestrate::execute`] - the piece
//! `xi-sources-push: orchestrator + xi push CLI verb` added - actually
//! wires them together the way the spec describes, starting from a real
//! [`SourcesContext`] pointed at a real `git init`'d directory rather
//! than a hand-built struct.

#![expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! calls are unimplemented Transport \
            methods this scenario never exercises"
)]

use std::{collections::BTreeMap, process::Command, sync::Mutex};

use color_eyre::Result;
use tempfile::TempDir;
use xi_core::sources::{Scope, SourcesContext, SourcesId};
use xi_deploy::push::{
  args::{
    BuildStrategy, CopyVia, Phase, PushArgs, PushBackend, PushMode, PushScope,
  },
  config::PushConfig,
  engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
  orchestrate,
};

/// `git init` a real repo at `dir` with a `flake.nix`, so the fixture
/// this test deploys from is an actual sources dir, not a bare tempdir.
fn git_sources_repo(dir: &std::path::Path) {
  let git = |args: &[&str]| {
    Command::new("git")
      .arg("-C")
      .arg(dir)
      .args(args)
      .env("GIT_AUTHOR_NAME", "xi-push-test")
      .env("GIT_AUTHOR_EMAIL", "xi-push-test@example.invalid")
      .env("GIT_COMMITTER_NAME", "xi-push-test")
      .env("GIT_COMMITTER_EMAIL", "xi-push-test@example.invalid")
      .status()
      .expect("git invocation")
  };
  std::fs::create_dir_all(dir).expect("mkdir sources dir");
  assert!(git(&["init", "--quiet"]).success());
  std::fs::write(
    dir.join("flake.nix"),
    "{ outputs = _: { nixosConfigurations.web1 = { }; }; }\n",
  )
  .expect("write flake.nix");
  assert!(git(&["add", "."]).success());
  assert!(git(&["commit", "--quiet", "-m", "sources: initial"]).success());
}

/// A real [`SourcesContext`] over the git fixture, the same shape
/// `push::context::resolve_context` would hand the orchestrator.
fn fixture_context(root: &TempDir) -> SourcesContext {
  let sources_dir = root.path().join("sources");
  git_sources_repo(&sources_dir);
  SourcesContext::load(
    SourcesId::new(Scope::System, "https://example.invalid/fixture"),
    sources_dir,
    root.path().join("state"),
  )
  .expect("load context over the real git fixture")
}

/// Answers flake evaluation for the fixture's one `nixosConfigurations`
/// output, the same shape `bdd_push_builtin.rs` uses.
struct FixtureNix {
  built: Mutex<Vec<String>>,
}

impl Nix for FixtureNix {
  fn eval_json(
    &self,
    installable: &str,
    _apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    Ok(if installable.ends_with("#nixosConfigurations") {
      Some(serde_json::json!(["web1"]))
    } else {
      None
    })
  }

  fn build(&self, installable: &str) -> Result<String> {
    self
      .built
      .lock()
      .expect("built lock")
      .push(installable.to_owned());
    Ok("/nix/store/fake-web1-system".to_owned())
  }
}

/// Answers every SSH-family call a single-host `builtin` push makes.
#[derive(Default)]
#[expect(
  clippy::struct_field_names,
  reason = "each field names the Transport call it records; the shared \
            `_calls` suffix is the point, not an accident"
)]
struct FixtureTransport {
  copy_calls: Mutex<Vec<(String, String)>>,
  run_calls:  Mutex<Vec<RemoteCommand>>,
  sync_calls: Mutex<Vec<(String, String)>>,
}

impl Transport for FixtureTransport {
  fn run(&self, _target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
    self
      .run_calls
      .lock()
      .expect("run_calls lock")
      .push(cmd.clone());
    if cmd.script == "true" || cmd.script == "sudo -n true" {
      return Ok(Outcome::ok(""));
    }
    if cmd.script.starts_with("df -kP") {
      return Ok(Outcome::ok(
        "/dev/sda1 100000000 10000000 90000000 11% /nix",
      ));
    }
    if cmd.script.starts_with("command -v") {
      return Ok(Outcome::ok(""));
    }
    Ok(Outcome::ok(""))
  }

  fn copy_closure(
    &self,
    target: &TargetAddr,
    store_path: &str,
    _via: CopyVia,
    _retry_attempts: u32,
  ) -> Result<()> {
    self
      .copy_calls
      .lock()
      .expect("copy_calls lock")
      .push((target.name.clone(), store_path.to_owned()));
    Ok(())
  }

  fn upload_secret(
    &self,
    _target: &TargetAddr,
    _local: &std::path::Path,
    _spec: &xi_core::sources::secrets::SecretSpec,
    _interactive_sudo: bool,
    _retry_attempts: u32,
  ) -> Result<()> {
    unreachable!("this fixture declares no secrets")
  }

  fn sync_dir(
    &self,
    _target: &TargetAddr,
    local: &std::path::Path,
    remote: &str,
    _excludes: &[&str],
    _interactive_sudo: bool,
    _retry_attempts: u32,
  ) -> Result<()> {
    self
      .sync_calls
      .lock()
      .expect("sync_calls lock")
      .push((local.display().to_string(), remote.to_owned()));
    Ok(())
  }
}

fn push_args() -> PushArgs {
  PushArgs {
    targets: vec!["web1".to_owned()],
    on: None,
    backend: PushBackend::Auto,
    phase: "activate".to_owned(),
    mode: Some(PushMode::Closure),
    remote_build: None,
    sources_transport: None,
    retry: None,
    magic_rollback: 0,
    parallel: 1,
    interactive_sudo: false,
    copy_via: CopyVia::Local,
    fail_fast: false,
    force_dirty: false,
    build_strategy: BuildStrategy::Interleaved,
    build_only: false,
    from_manifest: None,
    scope: PushScope::System,
    reattach: None,
    notify: None,
  }
}

#[test]
fn xi_push_runs_end_to_end_against_a_real_git_sources_fixture() {
  let root = TempDir::new().expect("tempdir");
  let context = fixture_context(&root);
  let flake_ref = context.sources_dir.display().to_string();

  let nix = FixtureNix {
    built: Mutex::new(Vec::new()),
  };
  let transport = FixtureTransport::default();
  let secrets: BTreeMap<String, _> = BTreeMap::new();
  let args = push_args();

  assert_eq!(args.phases().expect("phases parse"), vec![Phase::Activate]);
  args.validate().expect("args are internally consistent");

  let xi_toml_path = root.path().join(".xi.toml");

  let report = orchestrate::execute(
    &args,
    &context,
    &flake_ref,
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &xi_toml_path,
    &nix,
    &transport,
  )
  .expect("push executes against the real git fixture");

  assert!(report.ok(), "report: {:?}", report.results);
  assert_eq!(report.results.len(), 1);
  assert_eq!(report.results[0].name, "web1");

  assert_eq!(
    nix.built.lock().expect("lock").len(),
    1,
    "the fixture's nixosConfigurations.web1 toplevel must have been built"
  );
  assert_eq!(transport.copy_calls.lock().expect("lock").len(), 1);
  let last_script = transport
    .run_calls
    .lock()
    .expect("lock")
    .last()
    .expect("activation ran")
    .script
    .clone();
  assert!(
    last_script.contains("XI_APPLIED_ORIGIN=push:"),
    "got: {last_script}"
  );
  assert!(
    last_script.contains("xi system switch"),
    "got: {last_script}"
  );
}

#[test]
fn xi_push_mode_sources_activates_via_the_remote_switch_entrypoint_not_the_old_raw_script()
 {
  // xi-jhh4.2.3: the end-to-end counterpart of
  // `xi_push_runs_end_to_end_against_a_real_git_sources_fixture` for
  // `--mode sources`, driven through the real orchestrator. Proves the
  // activation step it reaches is an `xi <scope> switch` invocation, not
  // the old bare `nixos-rebuild switch --flake` script `mode_sources`
  // ran before xi-jhh4.2.2.
  let root = TempDir::new().expect("tempdir");
  let context = fixture_context(&root);
  let flake_ref = context.sources_dir.display().to_string();

  let nix = FixtureNix {
    built: Mutex::new(Vec::new()),
  };
  let transport = FixtureTransport::default();
  let secrets: BTreeMap<String, _> = BTreeMap::new();
  let mut args = push_args();
  args.mode = Some(PushMode::Sources);

  let report = orchestrate::execute(
    &args,
    &context,
    &flake_ref,
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &root.path().join(".xi.toml"),
    &nix,
    &transport,
  )
  .expect("push executes against the real git fixture");

  assert!(report.ok(), "report: {:?}", report.results);
  assert!(
    nix.built.lock().expect("lock").is_empty(),
    "--mode sources never builds a closure locally"
  );
  assert!(transport.copy_calls.lock().expect("lock").is_empty());
  assert_eq!(transport.sync_calls.lock().expect("lock").len(), 1);

  let last_script = transport
    .run_calls
    .lock()
    .expect("lock")
    .last()
    .expect("activation ran")
    .script
    .clone();
  assert!(
    last_script.contains("XI_APPLIED_ORIGIN=push:"),
    "got: {last_script}"
  );
  assert!(
    last_script.contains("xi system switch"),
    "got: {last_script}"
  );
  assert!(
    !last_script.contains("nixos-rebuild switch --flake"),
    "the old raw rebuild script must not run: {last_script}"
  );
  assert!(
    last_script.starts_with("systemd-run --wait --pipe --collect --unit="),
    "got: {last_script}"
  );

  // A fully successful rollout caches `fleet-digest` next to the
  // operator's `.xi.toml`, not inside any scope's state dir.
  let cached = std::fs::read_to_string(root.path().join(".xi-fleet-digest"))
    .expect("fleet-digest cache file must exist after a successful rollout");
  let expected = xi_deploy::fleet_digest::FleetDigestInput {
    targets:        vec!["web1".to_owned()],
    backend:        report.backend.as_str().to_owned(),
    build_location: report.build_location.clone(),
  }
  .digest();
  assert_eq!(cached, expected);
}

/// Flattens [`PushArgs`] behind the real `clap` parser, the same shape
/// `xi push` itself parses through, so a CLI invocation with no
/// `--build-strategy` flag resolves its default the way an operator would
/// actually see it - not the way a hand-built [`PushArgs`] literal would.
#[derive(clap::Parser)]
struct PushArgsHarness {
  #[command(flatten)]
  push: PushArgs,
}

/// xi-z3or.3.4: `--build-strategy` is parsed (xi-z3or.3.1) and a batched
/// build phase exists (xi-z3or.3.2), but [`orchestrate::execute`] does not
/// consult `args.build_strategy` yet - 3.3 is what wires a `batched` branch
/// in. Until then, "the flag omitted" and "`--build-strategy interleaved`
/// given explicitly" are two different `PushArgs` values that must both
/// reach `execute` and dispatch identically to today's interleaved
/// build-copy-activate sequence. This locks that invariant in now, so 3.3
/// cannot silently change the default path while adding the batched one.
#[test]
fn omitting_build_strategy_dispatches_identically_to_interleaved_given_explicitly()
 {
  use clap::Parser as _;

  let omitted = PushArgsHarness::try_parse_from(["push", "web1"])
    .expect("bare `push web1` invocation should parse")
    .push;
  assert_eq!(
    omitted.build_strategy,
    BuildStrategy::Interleaved,
    "the flag omitted must resolve to the same default execute() already \
     assumes"
  );

  let explicit = PushArgsHarness::try_parse_from([
    "push",
    "web1",
    "--build-strategy",
    "interleaved",
  ])
  .expect("`--build-strategy interleaved` should parse")
  .push;

  for (label, args) in [("omitted", &omitted), ("explicit", &explicit)] {
    let root = TempDir::new().expect("tempdir");
    let context = fixture_context(&root);
    let flake_ref = context.sources_dir.display().to_string();
    let nix = FixtureNix {
      built: Mutex::new(Vec::new()),
    };
    let transport = FixtureTransport::default();
    let secrets: BTreeMap<String, _> = BTreeMap::new();

    let report = orchestrate::execute(
      args,
      &context,
      &flake_ref,
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .unwrap_or_else(|e| panic!("{label} push executes: {e}"));

    assert!(report.ok(), "{label}: {:?}", report.results);
    assert_eq!(report.results.len(), 1, "{label}");
    assert_eq!(report.results[0].name, "web1", "{label}");
    assert_eq!(
      nix.built.lock().expect("lock").len(),
      1,
      "{label}: must build exactly once, matching the interleaved \
       single-target path"
    );
    assert_eq!(
      transport.copy_calls.lock().expect("lock").len(),
      1,
      "{label}"
    );
    let last_script = transport
      .run_calls
      .lock()
      .expect("lock")
      .last()
      .expect("activation ran")
      .script
      .clone();
    assert!(
      last_script.contains("xi system switch"),
      "{label} got: {last_script}"
    );
  }
}

#[test]
fn an_unknown_target_name_refuses_before_any_mutation() {
  let root = TempDir::new().expect("tempdir");
  let context = fixture_context(&root);
  let flake_ref = context.sources_dir.display().to_string();

  let nix = FixtureNix {
    built: Mutex::new(Vec::new()),
  };
  let transport = FixtureTransport::default();
  let secrets: BTreeMap<String, _> = BTreeMap::new();
  let mut args = push_args();
  args.targets = vec!["does-not-exist".to_owned()];

  let xi_toml_path = root.path().join(".xi.toml");

  let err = orchestrate::execute(
    &args,
    &context,
    &flake_ref,
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &xi_toml_path,
    &nix,
    &transport,
  )
  .expect_err("no such target");
  assert!(format!("{err}").contains("web1"), "got: {err}");
  assert!(transport.copy_calls.lock().expect("lock").is_empty());
  assert!(
    !root.path().join(".xi-fleet-digest").exists(),
    "a refused push must not cache a fleet-digest"
  );
}
