//! BDD: `xi deploy --notify <command>` against a fixture fleet fires
//! `<command>` exactly once per target on that target's terminal outcome,
//! matching `xi push --notify <command>`'s own existing coverage
//! (`crates/xi-deploy/src/push/orchestrate.rs`'s
//! `a_successful_mode_sources_push_runs_notify_exactly_once`).
//!
//! `push_args()` (`crates/xi/src/deploy.rs`, xi-6mf6.2.1) already proves
//! `DeployArgs::notify` carries unchanged into `PushArgs::notify`; `xi
//! push --notify`'s own coverage already proves that value fires through
//! `receipt::mark_notified_and_notify`/`remove_and_notify` exactly once
//! per push id. What is untested is the deploy verb's own flag reaching
//! that same mechanism unchanged across a fleet of more than one target:
//! this file parses `DeployArgs` the way `xi deploy --notify <command>`
//! itself would, then exercises the real `push::orchestrate::execute`
//! against a fixture fleet of two hosts under `--mode sources` (the only
//! mode `--notify`'s receipt mechanism wires through), proving the
//! command fires once per host rather than zero, twice, or only for one
//! of them.

#![expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! calls are unimplemented Transport \
            methods this scenario never exercises"
)]

use std::collections::BTreeMap;

use clap::Parser;
use color_eyre::Result;
use tempfile::TempDir;
use xi_core::sources::{Scope, SourcesContext, SourcesId};
use xi_deploy::{
  push::{
    args::{CopyVia, PushArgs, PushMode, PushScope},
    config::PushConfig,
    engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
    orchestrate::execute,
  },
  unified_args::DeployArgs,
};

/// Flattens `DeployArgs` behind the real `clap` parser, the same shape
/// `xi deploy` itself parses through, so `--notify <command>` resolves
/// the way an operator would actually see it.
#[derive(Debug, Parser)]
struct DeployArgsHarness {
  #[command(flatten)]
  args: DeployArgs,
}

/// The fixture fleet: every host a `RolloutFleet` decision would dispatch
/// `xi deploy --notify <command>` to.
const FIXTURE_FLEET: [&str; 2] = ["web1", "web2"];

/// Answers flake evaluation with a two-host `nixosConfigurations` set and
/// nothing else, the same shape `orchestrate.rs`'s own `FixtureNix` uses.
struct FixtureNix;

impl Nix for FixtureNix {
  fn eval_json(
    &self,
    installable: &str,
    _apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    Ok(if installable == ".#nixosConfigurations" {
      Some(serde_json::json!(FIXTURE_FLEET))
    } else {
      None
    })
  }

  fn build(&self, installable: &str) -> Result<String> {
    Ok(format!("/nix/store/fake-{installable}"))
  }
}

/// Answers every SSH-family call a `--mode sources` push makes: the
/// preflight probes, the `rsync`-standing-in `sync_dir`, and the
/// `systemd-run`-wrapped remote switch - all succeeding, so every target
/// reaches a confirmed terminal (success) outcome and its receipt is
/// removed-and-notified exactly once.
#[derive(Default)]
struct FixtureTransport;

impl Transport for FixtureTransport {
  fn run(&self, _target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
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
    // The `systemd-run`-wrapped remote switch itself.
    Ok(Outcome::ok(""))
  }

  fn copy_closure(
    &self,
    _target: &TargetAddr,
    _store_path: &str,
    _via: CopyVia,
    _retry_attempts: u32,
  ) -> Result<()> {
    unreachable!("--mode sources never copies a closure")
  }

  fn upload_secret(
    &self,
    _target: &TargetAddr,
    _local: &std::path::Path,
    _spec: &xi_core::sources::secrets::SecretSpec,
    _interactive_sudo: bool,
    _retry_attempts: u32,
  ) -> Result<()> {
    unreachable!("this fixture fleet declares no secrets")
  }

  fn sync_dir(
    &self,
    _target: &TargetAddr,
    _local: &std::path::Path,
    _remote: &str,
    _excludes: &[&str],
    _interactive_sudo: bool,
    _retry_attempts: u32,
  ) -> Result<()> {
    Ok(())
  }
}

/// A scratch [`SourcesContext`] pointing at a real tempdir, the same
/// shape `orchestrate.rs`'s own `fixture_context` uses.
fn fixture_context() -> (TempDir, SourcesContext) {
  let root = TempDir::new().expect("tempdir");
  let sources_dir = root.path().join("sources");
  std::fs::create_dir_all(&sources_dir).expect("mkdir sources dir");
  let context = SourcesContext::load(
    SourcesId::new(Scope::System, "https://example.invalid/fixture"),
    sources_dir,
    root.path().join("state"),
  )
  .expect("load context");
  (root, context)
}

/// Mirrors `crate::push_args`'s conversion (`crates/xi/src/deploy.rs`,
/// proven unchanged by `push_args_carries_the_seven_push_only_flags_unchanged`)
/// for the one field this test exercises - the function itself is private
/// to the `xi` crate and not reachable from this external test.
fn push_args(deploy_args: &DeployArgs, targets: Vec<String>) -> PushArgs {
  PushArgs {
    targets,
    on: deploy_args.on.clone(),
    backend: deploy_args.backend,
    phase: deploy_args.phase.clone(),
    mode: Some(PushMode::Sources),
    remote_build: deploy_args.remote_build,
    sources_transport: deploy_args.sources_transport,
    retry: deploy_args.retry,
    magic_rollback: deploy_args.magic_rollback,
    parallel: deploy_args.parallel,
    interactive_sudo: deploy_args.interactive_sudo,
    copy_via: deploy_args.copy_via,
    fail_fast: deploy_args.fail_fast,
    force_dirty: deploy_args.sources.force_dirty,
    build_strategy: deploy_args.build_strategy,
    build_only: deploy_args.build_only,
    from_manifest: deploy_args.from_manifest.clone(),
    scope: PushScope::System,
    reattach: deploy_args.reattach.clone(),
    notify: deploy_args.notify.clone(),
  }
}

#[test]
fn deploy_notify_fires_the_command_exactly_once_per_fleet_host_on_success() {
  let log = tempfile::NamedTempFile::new().expect("tempfile for the notify log");
  let log_path = log.path().to_owned();

  let parsed = DeployArgsHarness::try_parse_from([
    "xi",
    "--notify",
    &format!("printf 'fired\\n' >> \"{}\"", log_path.display()),
  ])
  .expect("`xi deploy --notify <command>` parses")
  .args;
  assert_eq!(
    parsed.notify.as_deref(),
    Some(format!("printf 'fired\\n' >> \"{}\"", log_path.display()).as_str()),
    "the deploy verb's own --notify flag parses to the given command"
  );

  let (_root, context) = fixture_context();
  let nix = FixtureNix;
  let transport = FixtureTransport;
  let secrets: BTreeMap<_, _> = BTreeMap::new();
  let args = push_args(&parsed, FIXTURE_FLEET.map(str::to_owned).to_vec());

  let report = execute(
    &args,
    &context,
    ".",
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &context.state_dir.join(".xi.toml"),
    &nix,
    &transport,
  )
  .expect("executes against the fixture fleet");

  assert!(report.ok(), "report: {:?}", report.results);
  assert_eq!(
    report.results.len(),
    FIXTURE_FLEET.len(),
    "every fixture-fleet host must have been dispatched"
  );

  let fired_count = std::fs::read_to_string(&log_path)
    .expect("read notify log")
    .lines()
    .filter(|line| *line == "fired")
    .count();
  assert_eq!(
    fired_count,
    FIXTURE_FLEET.len(),
    "--notify must fire exactly once per fleet host's terminal outcome, \
     not zero, twice, or skipped for any host"
  );

  let receipts_dir = context.state_dir.join("push-receipts");
  let leftover: Vec<_> = std::fs::read_dir(&receipts_dir)
    .map(|entries| entries.filter_map(Result::ok).collect())
    .unwrap_or_default();
  assert!(
    leftover.is_empty(),
    "a successful push's receipt is removed once --notify has fired: {leftover:?}"
  );
}
