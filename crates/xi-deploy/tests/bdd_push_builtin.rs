//! BDD: single-host push via the `builtin` backend
//! (`openspec/changes/xi-sources-push/specs/xi-sources-push/spec.md`,
//! SC-P-001 and the "Preflight" requirement).
//!
//! `xi push` has no orchestrator or CLI wiring yet (see
//! `crates/xi-deploy/src/push/mod.rs`'s module doc), so this file plays
//! the part of the orchestrator: it drives the real
//! [`discover`]/[`preflight`]/[`engine`]/[`rollback`] primitives in the
//! same sequence a single-host `builtin` push would, against the two
//! fakes every one of those modules already takes an edge for
//! ([`Nix`], [`Transport`]). A failure here means one of the pieces the
//! future orchestrator will call stopped composing the way the spec
//! says a push should.

#![expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! calls are unimplemented Transport \
            methods this scenario never exercises"
)]

use std::sync::Mutex;

use color_eyre::Result;
use xi_core::sources::Scope;
use xi_deploy::push::{
  args::{CopyVia, PushBackend},
  discover::{self, PushTarget, ResolvedBackend},
  engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
  mode_sources,
  preflight,
  rollback,
};

const FLAKE: &str = ".";

/// Answers flake evaluation from a fixture table keyed by attr. Absence
/// from the table reads as "no such output", which is how discovery
/// learns a flake has no `deploy`/`colmenaHive` output without that
/// being an error.
struct FixtureNix {
  outputs: Vec<(&'static str, serde_json::Value)>,
  built:   Mutex<Vec<String>>,
}

impl FixtureNix {
  fn single_nixos_host(name: &'static str) -> Self {
    Self {
      outputs: vec![(".#nixosConfigurations", serde_json::json!([name]))],
      built:   Mutex::new(Vec::new()),
    }
  }
}

impl Nix for FixtureNix {
  fn eval_json(
    &self,
    installable: &str,
    _apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    Ok(
      self
        .outputs
        .iter()
        .find(|(attr, _)| *attr == installable)
        .map(|(_, value)| value.clone()),
    )
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

/// Answers every SSH-family call a single-host `builtin` push makes:
/// the four preflight probes (matched by script content, since that is
/// what preflight actually sends) and whatever activation script the
/// test hands it afterwards.
#[derive(Default)]
struct FixtureTransport {
  copy_closure_calls: Mutex<Vec<(String, String, CopyVia)>>,
  run_calls:          Mutex<Vec<RemoteCommand>>,
  /// When set, the SSH probe step (`"true"`) fails - simulating an
  /// unreachable host - instead of the default all-green preflight.
  ssh_unreachable:    bool,
}

impl Transport for FixtureTransport {
  fn run(&self, _target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
    self
      .run_calls
      .lock()
      .expect("run_calls lock")
      .push(cmd.clone());

    if cmd.script == "true" {
      return Ok(if self.ssh_unreachable {
        Outcome::failed("connection refused")
      } else {
        Outcome::ok("")
      });
    }
    if cmd.script.starts_with("df -kP") {
      // Comfortably over preflight::MIN_FREE_NIX_KIB.
      return Ok(Outcome::ok(
        "/dev/sda1 100000000 10000000 90000000 11% /nix",
      ));
    }
    if cmd.script == "sudo -n true" {
      return Ok(Outcome::ok(""));
    }
    if cmd.script.starts_with("command -v") {
      return Ok(Outcome::ok(""));
    }
    // Anything else is the activation (or magic-rollback) script the
    // test itself constructed; the fixture just has to say it worked.
    Ok(Outcome::ok(""))
  }

  fn copy_closure(
    &self,
    target: &TargetAddr,
    store_path: &str,
    via: CopyVia,
    _retry_attempts: u32,
  ) -> Result<()> {
    self
      .copy_closure_calls
      .lock()
      .expect("copy_closure_calls lock")
      .push((target.name.clone(), store_path.to_owned(), via));
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

/// The activation script `xi-deploy-unify-activation` gives the
/// `builtin` backend's closure-mode push: a remote `xi <scope> switch
/// <store_path>` invocation, tagged with
/// `XI_APPLIED_ORIGIN=push:<origin_host>` so the target's
/// `record_activation_at` can tell this activation apart from a local
/// interactive switch or a gitops-daemon tick
/// (`crates/xi-deploy/src/push/orchestrate.rs::activation_script`).
fn activation_script(
  _target: &PushTarget,
  store_path: &str,
  scope: Scope,
  origin_host: &str,
) -> String {
  format!(
    "XI_APPLIED_ORIGIN=push:{origin_host} xi {} switch {store_path}",
    scope.sources_verb()
  )
}

#[test]
fn discovery_finds_the_single_host_and_resolves_the_builtin_backend() {
  let nix = FixtureNix::single_nixos_host("web1");
  let targets = discover::discover(&nix, FLAKE).expect("discovers");
  assert_eq!(targets.len(), 1);
  assert_eq!(targets[0].addr.name, "web1");

  let selected =
    discover::select(&targets, &["web1".to_owned()], None).expect("selects");
  assert_eq!(selected.len(), 1);

  let filtered: Vec<PushTarget> =
    selected.iter().map(|target| (*target).clone()).collect();
  let backend = discover::detect_backend(PushBackend::Auto, &filtered, false);
  assert_eq!(
    backend,
    ResolvedBackend::Builtin,
    "no deploy.* or colmenaHive output exists, so builtin is the fallback"
  );
}

#[test]
fn a_healthy_target_passes_every_preflight_check() {
  let nix = FixtureNix::single_nixos_host("web1");
  let targets = discover::discover(&nix, FLAKE).expect("discovers");
  let transport = FixtureTransport::default();

  let report =
    preflight::run(&transport, &targets[0], false).expect("preflight runs");
  assert!(report.ok(), "report: {report:?}");
  assert_eq!(report.results.len(), 4, "ssh, disk, sudo, rebuild-tool");
}

#[test]
fn a_clean_target_builds_copies_and_activates_in_one_pass() {
  // This is the end-to-end shape of SC-P-001 for a single host: the
  // operator asks for one target, nothing about the flake selects a
  // non-default backend, so `builtin` builds locally, copies the
  // closure, and activates - in that order, with preflight gating the
  // mutation and nothing else touched along the way.
  let nix = FixtureNix::single_nixos_host("web1");
  let transport = FixtureTransport::default();

  let targets = discover::discover(&nix, FLAKE).expect("discovers");
  let selected =
    discover::select(&targets, &["web1".to_owned()], None).expect("selects");
  let filtered: Vec<PushTarget> =
    selected.iter().map(|target| (*target).clone()).collect();
  assert_eq!(
    discover::detect_backend(PushBackend::Auto, &filtered, false),
    ResolvedBackend::Builtin
  );
  let target = selected[0];

  let report =
    preflight::run(&transport, target, false).expect("preflight runs");
  assert!(report.ok(), "preflight must pass before any mutation");

  let store_path = nix.build(&target.installable).expect("builds");
  assert!(
    nix
      .built
      .lock()
      .expect("built lock")
      .contains(&target.installable)
  );

  transport
    .copy_closure(&target.addr, &store_path, CopyVia::Local, 3)
    .expect("copies the closure");

  let activation =
    activation_script(target, &store_path, Scope::System, "operator-host");
  let outcome = transport
    .run(
      &target.addr,
      &RemoteCommand::elevated(activation.clone(), false, 3),
    )
    .expect("activates");
  assert!(outcome.success);

  let copies = transport.copy_closure_calls.lock().expect("lock").clone();
  assert_eq!(copies.len(), 1);
  assert_eq!(copies[0], ("web1".to_owned(), store_path, CopyVia::Local));

  let last_script = transport
    .run_calls
    .lock()
    .expect("lock")
    .last()
    .expect("ran")
    .script
    .clone();
  assert_eq!(last_script, activation);
}

#[test]
fn a_sources_mode_push_activates_via_the_remote_switch_entrypoint_not_the_old_raw_script()
 {
  // xi-jhh4.2.3: the end-to-end counterpart of
  // `a_clean_target_builds_copies_and_activates_in_one_pass` for `--mode
  // sources`. No closure is built or copied under this mode, so this
  // drives the real `mode_sources::switch` (not a hand-rolled copy)
  // against the fixture `Transport` and asserts the recorded command is
  // an `xi <scope> switch` invocation, not the old bare
  // `nixos-rebuild switch --flake` script.
  let nix = FixtureNix::single_nixos_host("web1");
  let transport = FixtureTransport::default();

  let targets = discover::discover(&nix, FLAKE).expect("discovers");
  let selected =
    discover::select(&targets, &["web1".to_owned()], None).expect("selects");
  let target = selected[0];

  mode_sources::switch(&transport, target, Scope::System, "operator-host", false, 3)
    .expect("activates via the remote switch entrypoint");

  let last_script = transport
    .run_calls
    .lock()
    .expect("lock")
    .last()
    .expect("ran")
    .script
    .clone();
  assert_eq!(
    last_script,
    mode_sources::wrap_in_systemd_run(&mode_sources::switch_script(
      target,
      Scope::System,
      "operator-host"
    ))
  );
  assert!(
    last_script.contains("xi deploy --local"),
    "got: {last_script}"
  );
  assert!(
    !last_script.contains("nixos-rebuild switch --flake"),
    "the old raw rebuild script must not run: {last_script}"
  );
  assert!(
    transport
      .copy_closure_calls
      .lock()
      .expect("lock")
      .is_empty(),
    "--mode sources never copies a closure"
  );
}

#[test]
fn an_unreachable_target_fails_preflight_and_the_push_never_mutates() {
  let nix = FixtureNix::single_nixos_host("web1");
  let transport = FixtureTransport {
    ssh_unreachable: true,
    ..FixtureTransport::default()
  };

  let targets = discover::discover(&nix, FLAKE).expect("discovers");
  let report =
    preflight::run(&transport, &targets[0], false).expect("preflight runs");

  assert!(!report.ok());
  assert_eq!(
    report.results.len(),
    1,
    "an unreachable ssh short-circuits the other three checks"
  );
  assert!(
    transport
      .copy_closure_calls
      .lock()
      .expect("lock")
      .is_empty()
  );
}

#[test]
fn magic_rollback_confirms_a_healthy_activation_without_rolling_back() {
  let transport = FixtureTransport::default();
  let target = TargetAddr {
    name:     "web1".to_owned(),
    ssh_host: "web1".to_owned(),
    ssh_opts: Vec::new(),
  };

  let outcome = rollback::confirm(
    &transport,
    &target,
    std::time::Duration::from_millis(10),
    std::time::Duration::from_millis(2),
    false,
    3,
  )
  .expect("confirms");
  assert_eq!(outcome, rollback::RollbackOutcome::Confirmed);

  // The sentinel plant and probes must not be mistaken for the
  // activation itself.
  let runs = transport.run_calls.lock().expect("lock").clone();
  assert!(
    runs
      .iter()
      .any(|cmd| cmd.script.contains(rollback::SENTINEL_PATH))
  );
}
