//! BDD: `xi deploy --build-strategy batched --build-only` builds every
//! selected target upfront and writes a manifest with nothing copied or
//! activated, and a later `xi deploy --from-manifest <path>` ships from
//! it without rebuilding - matching `xi push`'s own existing coverage
//! (`crates/xi-deploy/src/push/orchestrate.rs`'s
//! `build_only_builds_and_writes_a_manifest_but_ships_nothing` and
//! `a_batched_build_then_ship_round_trip_never_rebuilds_on_the_ship_leg`)
//! (xi-6mf6.3.3).
//!
//! `push_args()` (`crates/xi/src/deploy.rs`, xi-6mf6.2.1) already proves
//! `DeployArgs::build_strategy`/`build_only`/`from_manifest` carry
//! unchanged into their `PushArgs` counterparts; `xi push`'s own
//! coverage already proves those three fields drive the batched
//! build-then-ship round trip through `push::orchestrate::execute`.
//! What is untested is the deploy verb's own flags reaching that same
//! orchestrator unchanged: this file parses `DeployArgs` the way `xi
//! deploy --build-strategy batched --build-only` (and, in a second
//! invocation, `xi deploy --from-manifest <path>`) itself would, builds
//! the equivalent `PushArgs` the same way `push_args()` does, then
//! exercises the real `push::orchestrate::execute` against a fixture
//! `Nix`/`Transport` pair exactly as the push-level test does.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{collections::BTreeMap, path::Path, sync::Mutex};

use clap::Parser;
use color_eyre::{Result, eyre::bail};
use tempfile::TempDir;
use xi_core::sources::{Scope, SourcesContext, SourcesId, secrets::SecretSpec};
use xi_deploy::{
  push::{
    args::{
      BuildStrategy,
      CopyVia,
      PushArgs,
      PushBackend,
      PushMode,
      PushScope,
    },
    config::PushConfig,
    engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
    orchestrate::execute,
  },
  unified_args::DeployArgs,
};

/// Flattens `DeployArgs` behind the real `clap` parser, the same shape
/// `xi deploy` itself parses through, so `--build-strategy batched
/// --build-only`/`--from-manifest` resolve the way an operator would
/// actually see them.
#[derive(Debug, Parser)]
struct DeployArgsHarness {
  #[command(flatten)]
  args: DeployArgs,
}

fn parse(extra: &[&str]) -> DeployArgs {
  let mut argv = vec!["xi"];
  argv.extend_from_slice(extra);
  DeployArgsHarness::try_parse_from(argv)
    .expect("`xi deploy` with these flags parses")
    .args
}

/// Build the `PushArgs` a fleet rollout would dispatch to for `web1`,
/// the same mapping `push_args()` (`crates/xi/src/deploy.rs`) applies -
/// duplicated here because `push_args()` is private to that crate.
fn push_args_for(args: &DeployArgs) -> PushArgs {
  PushArgs {
    targets:           vec!["web1".to_owned()],
    on:                args.on.clone(),
    backend:           PushBackend::Auto,
    phase:             "activate".to_owned(),
    mode:              Some(PushMode::Closure),
    remote_build:      args.remote_build,
    sources_transport: args.sources_transport,
    retry:             args.retry,
    magic_rollback:    0,
    parallel:          1,
    interactive_sudo:  false,
    copy_via:          CopyVia::Local,
    fail_fast:         false,
    force_dirty:       false,
    build_strategy:    args.build_strategy,
    build_only:        args.build_only,
    from_manifest:     args.from_manifest.clone(),
    scope:             PushScope::System,
    reattach:          args.reattach.clone(),
    notify:            args.notify.clone(),
  }
}

/// Answers flake evaluation for a single NixOS host named `web1`, the
/// same fixture shape `push::orchestrate`'s own tests use.
struct FixtureNix {
  built: Mutex<Vec<String>>,
}

impl FixtureNix {
  fn single_nixos_host() -> Self {
    Self {
      built: Mutex::new(Vec::new()),
    }
  }
}

impl Nix for FixtureNix {
  fn eval_json(
    &self,
    installable: &str,
    _apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    if installable == ".#nixosConfigurations" {
      return Ok(Some(serde_json::json!(["web1"])));
    }
    Ok(None)
  }

  fn build(&self, installable: &str) -> Result<String> {
    self
      .built
      .lock()
      .expect("built lock")
      .push(installable.to_owned());
    Ok(format!("/nix/store/fake-{installable}"))
  }
}

/// Answers every SSH-family call a single-host push makes, the same
/// fixture shape `push::orchestrate`'s own tests use.
#[derive(Default)]
struct FixtureTransport {
  run_calls:  Mutex<Vec<RemoteCommand>>,
  copy_calls: Mutex<Vec<(String, String)>>,
}

impl Transport for FixtureTransport {
  fn run(&self, _target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
    self
      .run_calls
      .lock()
      .expect("run_calls lock")
      .push(cmd.clone());
    if cmd.script == "true" || cmd.script.starts_with("command -v") {
      return Ok(Outcome::ok(""));
    }
    if cmd.script.starts_with("df -kP") {
      return Ok(Outcome::ok(
        "/dev/sda1 100000000 10000000 90000000 11% /nix",
      ));
    }
    if cmd.script == "sudo -n true" {
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
    _local: &Path,
    _spec: &SecretSpec,
    _interactive_sudo: bool,
    _retry_attempts: u32,
  ) -> Result<()> {
    bail!("no secrets expected in this scenario")
  }

  fn sync_dir(
    &self,
    _target: &TargetAddr,
    _local: &Path,
    _remote: &str,
    _excludes: &[&str],
    _interactive_sudo: bool,
    _retry_attempts: u32,
  ) -> Result<()> {
    bail!("no sources sync expected in this scenario")
  }
}

fn fixture_context() -> (TempDir, SourcesContext) {
  let root = TempDir::new().expect("tempdir");
  let sources_dir = root.path().join("sources");
  std::fs::create_dir_all(&sources_dir).expect("sources dir");
  let context = SourcesContext::load(
    SourcesId::new(Scope::System, "https://example/repo"),
    sources_dir,
    root.path().join("state"),
  )
  .expect("load context");
  (root, context)
}

/// Where a batched build's manifest lands, inside the push's state dir -
/// `push::orchestrate::MANIFEST_FILE_NAME`'s value, duplicated here
/// because that constant is private to that crate.
const MANIFEST_FILE_NAME: &str = "push-manifest.json";

/// Scenario: `xi deploy --build-strategy batched --build-only` builds
/// `web1` upfront and writes a manifest, shipping nothing.
#[test]
fn deploy_build_strategy_batched_build_only_builds_and_writes_a_manifest_but_ships_nothing()
 {
  let args = parse(&["--build-strategy", "batched", "--build-only"]);
  assert_eq!(args.build_strategy, BuildStrategy::Batched);
  assert!(args.build_only);

  let push_args = push_args_for(&args);
  let nix = FixtureNix::single_nixos_host();
  let transport = FixtureTransport::default();
  let (root, context) = fixture_context();
  let secrets = BTreeMap::new();

  let report = execute(
    &push_args,
    &context,
    ".",
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &root.path().join(".xi.toml"),
    &nix,
    &transport,
  )
  .expect("build-only succeeds");

  assert!(report.ok(), "{:?}", report.results);
  assert_eq!(report.results.len(), 1);
  assert_eq!(report.results[0].name, "web1");
  assert_eq!(nix.built.lock().expect("built lock").len(), 1);
  assert!(
    transport.copy_calls.lock().expect("lock").is_empty(),
    "--build-only must not copy anything"
  );
  assert!(
    transport.run_calls.lock().expect("lock").is_empty(),
    "--build-only must not activate anything"
  );

  let manifest_path = context.state_dir.join(MANIFEST_FILE_NAME);
  let manifest: BTreeMap<String, String> = serde_json::from_str(
    &std::fs::read_to_string(&manifest_path).expect("manifest written"),
  )
  .expect("manifest is the {target: store-path} shape");
  assert_eq!(manifest.len(), 1);
  assert!(
    manifest["web1"].starts_with("/nix/store/fake-"),
    "got: {}",
    manifest["web1"]
  );
}

/// Scenario: a `--build-strategy batched --build-only` leg, then a
/// separate `--from-manifest <path>` leg - the second must ship `web1`
/// from the first leg's recorded store path without calling `nix.build`
/// again.
#[test]
fn deploy_from_manifest_ships_without_rebuilding_after_a_batched_build_only_leg()
 {
  let build_args = parse(&["--build-strategy", "batched", "--build-only"]);
  let nix = FixtureNix::single_nixos_host();
  let (root, context) = fixture_context();
  let secrets = BTreeMap::new();

  // Leg 1: `xi deploy --build-strategy batched --build-only` builds
  // web1 and writes the manifest, as a separate invocation that ships
  // nothing.
  let build_transport = FixtureTransport::default();
  let build_report = execute(
    &push_args_for(&build_args),
    &context,
    ".",
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &root.path().join(".xi.toml"),
    &nix,
    &build_transport,
  )
  .expect("build-only leg succeeds");
  assert!(build_report.ok(), "{:?}", build_report.results);
  assert_eq!(nix.built.lock().expect("built lock").len(), 1);

  let manifest_path = context.state_dir.join(MANIFEST_FILE_NAME);
  assert!(manifest_path.exists());

  // Leg 2: a separate invocation, `xi deploy --from-manifest <path>`,
  // ships web1 from the manifest's recorded store path - against the
  // *same* `FixtureNix`, so a second `nix.build` call here would show
  // up in its shared `built` log.
  let ship_args = parse(&[
    "--from-manifest",
    manifest_path.to_str().expect("utf8 path"),
  ]);
  assert_eq!(ship_args.from_manifest, Some(manifest_path.clone()));

  let mut ship_push_args = push_args_for(&ship_args);
  // `--from-manifest` requires `--build-strategy batched`
  // (`push::args::PushArgs::validate`); the harness's second `parse`
  // call does not re-pass `--build-strategy` since clap parses each
  // invocation independently, so this mirrors what an operator's own
  // `xi deploy --build-strategy batched --from-manifest <path>` would
  // resolve to.
  ship_push_args.build_strategy = BuildStrategy::Batched;

  let ship_transport = FixtureTransport::default();
  let ship_report = execute(
    &ship_push_args,
    &context,
    ".",
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &root.path().join(".xi.toml"),
    &nix,
    &ship_transport,
  )
  .expect("from-manifest leg succeeds");

  assert!(ship_report.ok(), "{:?}", ship_report.results);
  assert_eq!(
    nix.built.lock().expect("built lock").len(),
    1,
    "the ship leg must not call nix.build again"
  );
  assert_eq!(ship_transport.copy_calls.lock().expect("lock").len(), 1);
  let last = ship_transport
    .run_calls
    .lock()
    .expect("lock")
    .last()
    .expect("ran something")
    .script
    .clone();
  assert!(last.contains("XI_APPLIED_ORIGIN=push:"), "got: {last}");
  assert!(last.contains("xi system switch"), "got: {last}");
}
