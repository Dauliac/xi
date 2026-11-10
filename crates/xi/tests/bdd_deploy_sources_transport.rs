//! BDD: `xi deploy --sources-transport target-pull` against a fixture
//! fleet resolves `PushMode::TargetPull` for every target, matching `xi
//! push --sources-transport target-pull`'s own existing BDD coverage for
//! the same fixture shape
//! (`crates/xi-deploy/tests/bdd_push_orchestrator.rs`), extended to a
//! multi-host fleet so "every target" is actually exercised, not just
//! one (xi-6mf6.3.1).
//!
//! `DeployArgs::sources_transport` reaches
//! `xi_deploy::push::args::PushArgs::sources_transport` the same way
//! `crates/xi/src/deploy.rs::push_args()` carries it through - already
//! locked in by that function's own
//! `push_args_carries_the_seven_push_only_flags_unchanged` unit test.
//! This file proves the next step: once there, it actually drives
//! `xi_deploy::push::orchestrate::execute` to resolve every discovered
//! target to `PushMode::TargetPull`, with no closure build, no closure
//! copy, and no sources-dir sync - the same invariant
//! `bdd_push_orchestrator.rs` already proves for `xi push` plain.

#![expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! call is an unimplemented \
            Transport method this scenario never exercises"
)]

use std::{collections::BTreeMap, process::Command, sync::Mutex};

use clap::Parser;
use color_eyre::Result;
use tempfile::TempDir;
use xi_core::sources::{Scope, SourcesContext, SourcesId};
use xi_deploy::{
  push::{
    args::{CopyVia, PushArgs, PushBackend, PushScope},
    config::PushConfig,
    engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
    orchestrate,
  },
  unified_args::DeployArgs,
};

/// Flattens [`DeployArgs`] behind the real `clap` parser, the same
/// shape `xi deploy` itself parses through - mirroring
/// `crates/xi/src/deploy.rs`'s own `TestCli` harness.
#[derive(Debug, Parser)]
struct DeployCli {
  #[command(flatten)]
  args: DeployArgs,
}

/// `git init` a real repo at `dir` with a `flake.nix` declaring three
/// `nixosConfigurations` hosts, so discovery finds a fleet rather than a
/// single host - the one shape difference from
/// `bdd_push_orchestrator.rs`'s own single-host fixture, needed to prove
/// "every target" rather than just one.
fn git_sources_repo(dir: &std::path::Path) {
  let git = |args: &[&str]| {
    Command::new("git")
      .arg("-C")
      .arg(dir)
      .args(args)
      .env("GIT_AUTHOR_NAME", "xi-deploy-sources-transport-test")
      .env(
        "GIT_AUTHOR_EMAIL",
        "xi-deploy-sources-transport-test@example.invalid",
      )
      .env("GIT_COMMITTER_NAME", "xi-deploy-sources-transport-test")
      .env(
        "GIT_COMMITTER_EMAIL",
        "xi-deploy-sources-transport-test@example.invalid",
      )
      .status()
      .expect("git invocation")
  };
  std::fs::create_dir_all(dir).expect("mkdir sources dir");
  assert!(git(&["init", "--quiet"]).success());
  std::fs::write(
    dir.join("flake.nix"),
    "{ outputs = _: { nixosConfigurations.web1 = { }; \
     nixosConfigurations.web2 = { }; nixosConfigurations.web3 = { }; }; } \n",
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

/// Answers flake evaluation for the fixture's three `nixosConfigurations`
/// hosts, the same shape `bdd_push_orchestrator.rs`'s own `FixtureNix`
/// uses.
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
      Some(serde_json::json!(["web1", "web2", "web3"]))
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
    Ok(format!("/nix/store/fake-{installable}"))
  }
}

/// Answers every SSH-family call a push makes, the same shape
/// `bdd_push_orchestrator.rs`'s own `FixtureTransport` uses. Leaves
/// `advance_now` at [`Transport`]'s own default (always errors, naming
/// the target): `--sources-transport target-pull` with no daemon
/// fixture to answer it is exactly what proves every target actually
/// reached `PushMode::TargetPull` dispatch - `PushMode::Closure` would
/// build and copy instead, `PushMode::Sources` would sync instead, and
/// neither calls `advance_now` at all.
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

/// Build the [`PushArgs`] `crates/xi/src/deploy.rs::push_args()` would
/// build from `deploy_args` for a fleet rollout, restricted to the
/// fields this scenario exercises - mirrors that function's own
/// field-for-field pass-through.
fn push_args_from(deploy_args: &DeployArgs) -> PushArgs {
  PushArgs {
    targets:           Vec::new(),
    on:                None,
    backend:           PushBackend::Auto,
    phase:             "activate".to_owned(),
    mode:              deploy_args.mode,
    remote_build:      deploy_args.remote_build,
    sources_transport: deploy_args.sources_transport,
    retry:             deploy_args.retry,
    magic_rollback:    0,
    parallel:          1,
    interactive_sudo:  false,
    copy_via:          CopyVia::Local,
    fail_fast:         false,
    force_dirty:       false,
    build_strategy:    deploy_args.build_strategy,
    build_only:        deploy_args.build_only,
    from_manifest:     deploy_args.from_manifest.clone(),
    scope:             PushScope::System,
    reattach:          deploy_args.reattach.clone(),
    notify:            deploy_args.notify.clone(),
  }
}

#[test]
fn deploy_sources_transport_target_pull_resolves_target_pull_for_every_target()
{
  let cli =
    DeployCli::try_parse_from(["xi", "--sources-transport", "target-pull"])
      .expect("`xi deploy --sources-transport target-pull` parses");

  let root = TempDir::new().expect("tempdir");
  let context = fixture_context(&root);
  let flake_ref = context.sources_dir.display().to_string();

  let nix = FixtureNix {
    built: Mutex::new(Vec::new()),
  };
  let transport = FixtureTransport::default();
  let secrets: BTreeMap<String, _> = BTreeMap::new();
  let args = push_args_from(&cli.args);

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
  .expect("execute reports a per-target failure rather than erroring itself");

  let mut names: Vec<&str> = report
    .results
    .iter()
    .map(|result| result.name.as_str())
    .collect();
  names.sort_unstable();
  assert_eq!(names, ["web1", "web2", "web3"]);

  assert!(
    !report.ok(),
    "no daemon fixture answers advance_now, so every target must fail there \
     rather than silently succeeding via another mode: {:?}",
    report.results
  );
  for result in &report.results {
    let message = result
      .outcome
      .as_ref()
      .expect_err(
        "every target resolves TargetPull, which this fixture cannot answer",
      )
      .to_string();
    assert!(
      message.contains("does not support --mode target-pull's advance_now"),
      "target {} must have dispatched through PushMode::TargetPull, got: \
       {message}",
      result.name
    );
  }

  assert!(
    nix.built.lock().expect("lock").is_empty(),
    "--sources-transport target-pull must never build a closure locally"
  );
  assert!(
    transport.copy_calls.lock().expect("lock").is_empty(),
    "--sources-transport target-pull must never copy a closure"
  );
  assert!(
    transport.sync_calls.lock().expect("lock").is_empty(),
    "--sources-transport target-pull must never sync the sources dir"
  );
}
