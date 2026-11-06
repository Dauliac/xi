//! BDD: `xi push` against a flake declaring `systemConfigs.web3`
//! (xi-slej.3.3, SC-MD-006), end to end through the real orchestrator
//! (`crates/xi-deploy/src/push/orchestrate.rs`), against a real local git
//! fixture standing in for the operator's sources dir.
//!
//! `TargetSource::SystemConfigs` discovery (xi-slej.3.1) and its
//! `ProfileKind::Custom` `activate_command` template (xi-slej.3.2) each
//! have their own unit coverage in `discover.rs`; what this file proves
//! is that a real push - discovery, build, copy, and activation - run
//! that sequence end to end for a system-manager target, rather than a
//! `nixos-rebuild`- or `darwin-rebuild`-shaped command, mirroring
//! `bdd_push_orchestrator.rs`'s coverage of the `nixosConfigurations`
//! path.

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
  args::{CopyVia, Phase, PushArgs, PushBackend, PushMode, PushScope},
  config::PushConfig,
  engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
  orchestrate,
};

/// `git init` a real repo at `dir` with a `flake.nix` declaring only
/// `systemConfigs.web3`, so the fixture this test deploys from is an
/// actual sources dir, not a bare tempdir.
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
    "{ outputs = _: { systemConfigs.web3 = { }; }; }\n",
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

/// Answers flake evaluation for the fixture's one `systemConfigs`
/// output, mirroring `discover.rs`'s own `SystemConfigsFixtureNix`.
struct FixtureNix {
  built: Mutex<Vec<String>>,
}

impl Nix for FixtureNix {
  fn eval_json(
    &self,
    installable: &str,
    _apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    Ok(if installable.ends_with("#systemConfigs") {
      Some(serde_json::json!(["web3"]))
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
    Ok("/nix/store/fake-web3-system-manager".to_owned())
  }
}

/// Answers every SSH-family call a single-host `builtin` push makes.
#[derive(Default)]
struct FixtureTransport {
  copy_calls: Mutex<Vec<(String, String)>>,
  run_calls:  Mutex<Vec<RemoteCommand>>,
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
  ) -> Result<()> {
    unreachable!("this fixture declares no secrets")
  }

  fn sync_dir(
    &self,
    _target: &TargetAddr,
    _local: &std::path::Path,
    _remote: &str,
    _excludes: &[&str],
    _interactive_sudo: bool,
  ) -> Result<()> {
    unreachable!("--mode closure never syncs a directory")
  }
}

/// No explicit targets: SC-MD-006 pushes "with no explicit targets" and
/// relies on discovery to find `web3` from the `systemConfigs` output.
fn push_args() -> PushArgs {
  PushArgs {
    targets:          Vec::new(),
    on:               None,
    backend:          PushBackend::Auto,
    phase:            "activate".to_owned(),
    mode:             Some(PushMode::Closure),
    remote_build:     None,
    magic_rollback:   0,
    parallel:         1,
    interactive_sudo: false,
    copy_via:         CopyVia::Local,
    fail_fast:        false,
    force_dirty:      false,
    scope:            PushScope::System,
  }
}

#[test]
fn xi_push_discovers_and_activates_a_system_configs_target() {
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

  let report = orchestrate::execute(
    &args,
    &context,
    &flake_ref,
    &secrets,
    &PushConfig::default(),
    None,
    &nix,
    &transport,
  )
  .expect("push executes against the real git fixture");

  assert!(report.ok(), "report: {:?}", report.results);
  assert_eq!(report.results.len(), 1);
  assert_eq!(report.results[0].name, "web3");

  assert_eq!(
    nix.built.lock().expect("lock").len(),
    1,
    "the fixture's systemConfigs.web3 toplevel must have been built"
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
    last_script.contains("nix build --no-link --profile"),
    "got: {last_script}"
  );
  assert!(last_script.contains("/bin/activate"), "got: {last_script}");
  assert!(
    !last_script.contains("switch-to-configuration"),
    "got: {last_script}"
  );
  assert!(
    !last_script.contains("nixos-rebuild")
      && !last_script.contains("darwin-rebuild"),
    "got: {last_script}"
  );
}
