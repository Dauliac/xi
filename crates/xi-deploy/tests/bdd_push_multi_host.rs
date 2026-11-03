//! BDD: multi-host push with `--parallel 2`
//! (`openspec/changes/xi-sources-push/tasks.md` §8.2, the
//! `xi-sources-push` spec's SC-P-005: "With `--parallel 4` and five
//! targets, four activations run concurrently; the fifth queues.").
//!
//! Like `bdd_push_builtin.rs`, `xi push` has no orchestrator or CLI
//! wiring yet (see `crates/xi-deploy/src/push/mod.rs`'s module doc), so
//! this plays the orchestrator's part: discover several hosts, then run
//! each host's eval-then-apply pipeline on its own thread, gated
//! through the real [`limits::PhaseLimits`] the `--parallel` flag is
//! meant to configure. A failure here means the split eval/apply
//! semaphore pair (`xi-sources-push` tasks.md §6.1) does not actually
//! cap concurrent *activations* the way `--parallel N` promises, even
//! though each piece passes its own unit tests in isolation.

#![expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! calls are unimplemented \
            Transport methods this scenario never exercises"
)]

use std::{
  sync::{
    Arc,
    Mutex,
    atomic::{AtomicUsize, Ordering},
  },
  thread,
  time::Duration,
};

use color_eyre::Result;
use xi_deploy::push::{
  args::CopyVia,
  discover::{self, PushTarget},
  engine::{Nix, Outcome, RemoteCommand, TargetAddr, Transport},
  limits::PhaseLimits,
  preflight,
};

const FLAKE: &str = ".";
const HOSTS: &[&str] = &["web1", "web2", "web3", "web4", "web5"];

/// Five `nixosConfigurations` entries, nothing else - enough targets
/// that a cap of 2 concurrent activations is actually distinguishable
/// from "everything just happened to run one at a time".
struct FixtureNix;

impl Nix for FixtureNix {
  fn eval_json(
    &self,
    installable: &str,
    _apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    if installable == ".#nixosConfigurations" {
      Ok(Some(serde_json::json!(HOSTS)))
    } else {
      Ok(None)
    }
  }

  fn build(&self, installable: &str) -> Result<String> {
    Ok(format!("/nix/store/fake-{installable}"))
  }
}

/// Tracks how many callers were inside the gate at the same time, the
/// same shape `push::limits`'s own test uses.
#[derive(Debug, Default)]
struct Peak {
  current: AtomicUsize,
  peak:    AtomicUsize,
}

impl Peak {
  fn enter(&self) {
    let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
    self.peak.fetch_max(now, Ordering::SeqCst);
  }

  fn leave(&self) {
    self.current.fetch_sub(1, Ordering::SeqCst);
  }
}

/// Answers every SSH-family call a multi-host `builtin` push makes.
/// Preflight probes resolve instantly; the activation script (anything
/// else) holds [`Peak`] open for a few milliseconds so concurrent
/// activations actually overlap in wall-clock time, the same way real
/// `switch-to-configuration` calls would.
#[derive(Default)]
struct FixtureTransport {
  activated:  Mutex<Vec<String>>,
  apply_peak: Peak,
}

impl Transport for FixtureTransport {
  fn run(&self, target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
    if cmd.script == "true" || cmd.script == "sudo -n true" {
      return Ok(Outcome::ok(""));
    }
    if cmd.script.starts_with("df -kP") {
      // Comfortably over preflight::MIN_FREE_NIX_KIB.
      return Ok(Outcome::ok(
        "/dev/sda1 100000000 10000000 90000000 11% /nix",
      ));
    }
    if cmd.script.starts_with("command -v") {
      return Ok(Outcome::ok(""));
    }

    self.apply_peak.enter();
    thread::sleep(Duration::from_millis(30));
    self.apply_peak.leave();
    self.activated.lock().expect("activated lock").push(target.name.clone());
    Ok(Outcome::ok(""))
  }

  fn copy_closure(
    &self,
    _target: &TargetAddr,
    _store_path: &str,
    _via: CopyVia,
  ) -> Result<()> {
    Ok(())
  }

  fn upload_secret(
    &self,
    _target: &TargetAddr,
    _local: &std::path::Path,
    _spec: &xi_core::sources::secrets::SecretSpec,
    _interactive_sudo: bool,
  ) -> Result<()> {
    unreachable!("a multi-host activate push has no secrets in this scenario")
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

/// The `builtin` backend's activation script, same shape as
/// `bdd_push_builtin.rs`'s.
fn activation_script(target: &PushTarget, store_path: &str) -> String {
  format!(
    "nix-env --profile {} --set {store_path} && \
     {store_path}/bin/switch-to-configuration switch",
    target.profile_path
  )
}

/// Run one target's eval-then-apply pipeline, gated by `limits`.
fn push_one(
  nix: &FixtureNix,
  transport: &FixtureTransport,
  limits: &PhaseLimits,
  target: &PushTarget,
) {
  let store_path = {
    // Eval and apply are independent gates (tasks.md §6.1): holding the
    // eval permit must not also hold back a different target's apply.
    let _eval_permit = limits.eval.acquire();
    nix.build(&target.installable).expect("builds")
  };

  let report =
    preflight::run(transport, target, false).expect("preflight runs");
  assert!(report.ok(), "preflight must pass before any mutation");

  let _apply_permit = limits.apply.acquire();
  transport
    .copy_closure(&target.addr, &store_path, CopyVia::Local)
    .expect("copies the closure");
  let script = activation_script(target, &store_path);
  let outcome = transport
    .run(&target.addr, &RemoteCommand::elevated(script, false))
    .expect("activates");
  assert!(outcome.success);
}

#[test]
fn parallel_two_caps_concurrent_activations_across_five_hosts() {
  let nix = FixtureNix;
  let transport = Arc::new(FixtureTransport::default());
  let limits = Arc::new(PhaseLimits::new(2));

  let targets = discover::discover(&nix, FLAKE).expect("discovers");
  assert_eq!(targets.len(), HOSTS.len(), "all five hosts must be found");

  let handles: Vec<_> = targets
    .into_iter()
    .map(|target| {
      let transport = Arc::clone(&transport);
      let limits = Arc::clone(&limits);
      // `FixtureNix` holds no state, so each thread gets its own; the
      // shared mutable state under test is entirely in `transport` and
      // `limits`.
      thread::spawn(move || {
        push_one(&FixtureNix, &transport, &limits, &target);
      })
    })
    .collect();

  for handle in handles {
    handle.join().expect("a push worker panicked");
  }

  assert_eq!(
    transport.activated.lock().expect("activated lock").len(),
    HOSTS.len(),
    "every host must have activated exactly once"
  );

  let peak = transport.apply_peak.peak.load(Ordering::SeqCst);
  assert!(peak <= 2, "peak concurrent activations {peak} exceeded --parallel 2");
  // With five targets, a 30ms hold per activation, and a cap of 2, the
  // cap must actually have been saturated at least once - otherwise
  // this assertion would pass even if `--parallel` were silently
  // ignored and every target just happened to run alone.
  assert_eq!(
    peak, 2,
    "the cap was never reached; this scenario does not actually exercise \
     concurrency"
  );
}

#[test]
fn a_single_target_never_needs_more_than_one_permit_of_either_kind() {
  // The degenerate end of SC-P-005: `--parallel 2` with one target must
  // not deadlock or require two concurrent holders of a semaphore that
  // only ever sees one caller.
  let nix = FixtureNix;
  let transport = FixtureTransport::default();
  let limits = PhaseLimits::new(2);

  let targets = discover::discover(&nix, FLAKE).expect("discovers");
  let selected =
    discover::select(&targets, &["web1".to_owned()], None).expect("selects");
  let target = (*selected[0]).clone();

  push_one(&nix, &transport, &limits, &target);

  assert_eq!(transport.activated.lock().expect("lock").as_slice(), ["web1"]);
}
