//! BDD: an inbound `xi push` activates its target end-to-end against a
//! fixture transport, and the target's own state dir carries
//! `applied-origin: push:<host>` afterwards
//! (`openspec/changes/xi-deploy-unify-activation/specs/
//! xi-deploy-unify-activation/spec.md`, SC-UA-001 and SC-UA-003).
//!
//! `xi-jhh4.3.1`-`4.3.3` proved `record_activation_at` writes the marker
//! when `XI_APPLIED_ORIGIN` is set, and that `xi <scope> status` reads it
//! back - both against a hand-seeded state dir, deliberately bypassing the
//! writer's real caller. `bdd_push_builtin.rs` and `orchestrate.rs`'s own
//! tests prove the activation script `push::orchestrate::execute` builds
//! has the right shape, by asserting on the string the fixture transport
//! recorded. Neither proves the two ends actually meet: that *running*
//! the script the orchestrator hands a real target lands the marker the
//! status reader expects.
//!
//! This file closes that gap. [`FixtureTransport::run`] plays the SSH hop
//! exactly as `bdd_push_builtin.rs`'s fixture does for every preflight
//! probe, but for the one script that is an `xi <scope> switch`
//! activation it also executes the target-side half: it calls the real
//! [`record_activation_at`] with the env var the script carries, standing
//! in for what a real target's own `xi` binary does once that script
//! reaches it over SSH.

#![expect(
  clippy::expect_used,
  clippy::unreachable,
  clippy::unused_self,
  reason = "Fine in tests; the unreachable! calls are unimplemented Transport \
            methods this scenario never exercises, and OriginEnvGuard::set \
            mirrors xi_core::sources::switch's own test helper of the same \
            shape"
)]

use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};

use color_eyre::Result;
use serial_test::serial;
use tempfile::TempDir;
use xi_core::sources::{
  Scope,
  SourcesContext,
  SourcesId,
  config::ScopePaths,
  read_state_file,
  switch::record_activation_at,
};
use xi_deploy::push::{
  args::{
    BuildStrategy, CopyVia, PushArgs, PushBackend, PushMode, PushScope,
  },
  config::PushConfig,
  engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
  orchestrate::execute,
};

const FLAKE: &str = ".";

/// Answers flake evaluation with a single `nixosConfigurations` entry,
/// nothing else - the same shape `orchestrate.rs`'s own tests and
/// `bdd_push_builtin.rs` use.
struct FixtureNix;

impl Nix for FixtureNix {
  fn eval_json(
    &self,
    installable: &str,
    _apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    if installable == ".#nixosConfigurations" {
      Ok(Some(serde_json::json!(["web1"])))
    } else {
      Ok(None)
    }
  }

  fn build(&self, _installable: &str) -> Result<String> {
    Ok("/nix/store/fake-web1-system".to_owned())
  }
}

/// Saves, clears, and restores `XI_APPLIED_ORIGIN`, mirroring
/// `xi_core::sources::switch`'s own `OriginEnvGuard`:
/// [`record_activation_at`] reads this from the process environment, so a
/// test driving it for real must not race another test in the same
/// process over the same var. `#[serial]` on the test below is this
/// guard's other half.
struct OriginEnvGuard {
  saved: Option<String>,
}

impl OriginEnvGuard {
  fn clear() -> Self {
    let saved = std::env::var("XI_APPLIED_ORIGIN").ok();
    unsafe {
      std::env::remove_var("XI_APPLIED_ORIGIN");
    }
    Self { saved }
  }

  fn set(&self, value: &str) {
    unsafe {
      std::env::set_var("XI_APPLIED_ORIGIN", value);
    }
  }
}

impl Drop for OriginEnvGuard {
  fn drop(&mut self) {
    unsafe {
      match &self.saved {
        Some(v) => std::env::set_var("XI_APPLIED_ORIGIN", v),
        None => std::env::remove_var("XI_APPLIED_ORIGIN"),
      }
    }
  }
}

/// Split `"XI_APPLIED_ORIGIN=push:host xi system switch /nix/store/..."`
/// into the origin tag and the store path - the exact shape
/// `push::orchestrate::activation_script` emits for `--mode closure`.
fn parse_activation_script(script: &str) -> (String, String) {
  let (env_assignment, rest) = script
    .split_once(' ')
    .expect("an activation script carries an env assignment then a command");
  let origin = env_assignment
    .strip_prefix("XI_APPLIED_ORIGIN=")
    .expect("the first token is the applied-origin tag")
    .to_owned();
  let store_path = rest
    .rsplit(' ')
    .next()
    .expect("the script's last token is the store path")
    .to_owned();
  (origin, store_path)
}

/// Answers every SSH-family call a single-host `builtin` push makes, the
/// same preflight shape `bdd_push_builtin.rs` uses - except the
/// activation script, which this transport actually executes against
/// `target_state_dir` instead of only recording.
struct FixtureTransport {
  run_calls:        Mutex<Vec<RemoteCommand>>,
  target_state_dir: PathBuf,
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

    // The activation script itself: simulate the target's own `xi system
    // switch` reaching its real success path, which ends in a call to
    // `record_activation_at` reading `XI_APPLIED_ORIGIN` from its own
    // process environment - exactly what this script sets.
    let (origin, store_path) = parse_activation_script(&cmd.script);
    let guard = OriginEnvGuard::clear();
    guard.set(&origin);
    let paths = ScopePaths {
      sources_dir: self.target_state_dir.join("sources"),
      state_dir:   self.target_state_dir.clone(),
      config_file: self.target_state_dir.join("sources.toml"),
    };
    record_activation_at(&paths, std::path::Path::new(&store_path), false)
      .expect("the target's own record_activation_at must succeed");
    drop(guard);

    Ok(Outcome::ok(""))
  }

  fn copy_closure(
    &self,
    _target: &TargetAddr,
    _store_path: &str,
    _via: CopyVia,
    _retry_attempts: u32,
  ) -> Result<()> {
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
    unreachable!("a single-host activate push has no secrets in this scenario")
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
    unreachable!("--mode closure never syncs a directory")
  }
}

/// A scratch [`SourcesContext`] pointing at a real tempdir, the same
/// shape `orchestrate.rs`'s own `fixture_context` uses - this is the
/// *operator's* context, distinct from `target_state_dir`, which stands
/// in for the target machine on the other end of the SSH hop.
fn operator_context() -> (TempDir, SourcesContext) {
  let root = TempDir::new().expect("tempdir");
  let sources_dir = root.path().join("sources");
  std::fs::create_dir_all(&sources_dir).expect("mkdir sources dir");
  let context = SourcesContext::load(
    SourcesId::new(Scope::System, "https://example/repo"),
    sources_dir,
    root.path().join("state"),
  )
  .expect("load context");
  (root, context)
}

#[test]
#[serial]
fn a_closure_mode_push_leaves_the_targets_applied_origin_marker_behind() {
  let nix = FixtureNix;
  let target_root = TempDir::new().expect("target tempdir");
  let transport = FixtureTransport {
    run_calls:        Mutex::new(Vec::new()),
    target_state_dir: target_root.path().join("state"),
  };
  let (operator_root, context) = operator_context();
  let secrets = BTreeMap::new();

  let args = PushArgs {
    targets:          vec!["web1".to_owned()],
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
    build_strategy:   BuildStrategy::Interleaved,
    build_only:       false,
    from_manifest:    None,
    scope:            PushScope::System,
    retry:            None,
    reattach:         None,
    notify:           None,
  };

  let report = execute(
    &args,
    &context,
    FLAKE,
    &secrets,
    &PushConfig::default(),
    None,
    None,
    &operator_root.path().join(".xi.toml"),
    &nix,
    &transport,
  )
  .expect("push executes");

  assert!(report.ok(), "push must succeed: {:?}", report.results);

  let origin_host =
    xi_core::util::get_hostname(None).expect("this machine's hostname");
  let applied_origin =
    read_state_file(&transport.target_state_dir, "applied-origin")
      .expect("read applied-origin")
      .expect("applied-origin must be written by the activation script");

  assert_eq!(applied_origin, format!("push:{origin_host}"));
}
