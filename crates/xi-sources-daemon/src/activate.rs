//! Build, gate, activate and register a generation after a successful
//! advance.
//!
//! `xi-sources-gitops` section 6.2: once a tick has fast-forwarded the
//! sources dir ([`crate::tick::run`] returning `Advanced`), *this* module
//! is what turns that into a running (or at least registered) generation.
//! It does not build anything itself - see the module doc on
//! [[xi-sources-gitops]]'s "Build and activate by delegating to `xi
//! switch`" decision: the daemon execs `xi <scope> switch` (no
//! positional) through the [`Exec`] seam, the same no-positional path the
//! interactive CLI already uses for an adopted scope
//! (`xi_core::sources::switch::prepare`, case 2). That subprocess takes
//! its own fresh `SourcesLock`, resolves the conflict policy, builds,
//! activates, and records `active-generation` in the state dir
//! (`xi_core::sources::switch::record_activation_at`) exactly as an
//! interactive switch would.
//!
//! # The build-only probe
//!
//! The `machineId` guard (section 7.2) and the reboot-window gate
//! (section 10.2) both need to inspect the generation *before* deciding
//! whether to activate it - a NixOS closure's `etc/machine-id` and
//! `kernel` symlink only exist once something has actually been built.
//! Building twice (once to probe, once for real inside `xi switch`)
//! sounds wasteful but is not: both invocations resolve the exact same
//! installable through the exact same `xi <verb> build`/`switch`
//! machinery (no second flake-attribute convention is invented here),
//! and Nix's own store cache makes the second invocation a lookup rather
//! than a rebuild. `probe_build` runs `xi <verb> build --out-link
//! <path>` - the existing, already-public `build` subcommand, which
//! resolves an adopted scope's installable exactly like `switch` does
//! but stops short of activating - and returns the realised store path.
//!
//! # The reboot-window gate
//!
//! "Would this activation require a reboot" only has an honest answer
//! once the pending generation exists on disk, for the same reason the
//! `machineId` guard needs the probe: a NixOS closure's kernel is a real
//! symlink inside it (`<store_path>/kernel`), not something evaluable
//! ahead of a build. `requires_reboot` compares that symlink's target
//! against the currently booted kernel (`/run/booted-system/kernel`);
//! platforms with no such path (darwin, system-manager, home-manager -
//! none of which this crate models a reboot concept for) always compare
//! as "no reboot needed", which is a reasoned limitation rather than a
//! blanket guess: only a NixOS kernel change is a reboot-relevant event
//! this codebase can detect at all today. A kernel path that cannot be
//! read on either side ("cannot determine") is treated the same way -
//! the gate exists to catch a *known* kernel change, not to block
//! indefinitely on missing data.
//!
//! [`run`] therefore: probes, gates (`machineId` then reboot-window),
//! execs the real switch, and - on success - reads back the
//! `active-generation` that exec just wrote and registers it on the
//! daemon's own named profile ([`crate::profile::register_generation`]),
//! the profile `register_generation`'s own doc says is section 6.2's
//! job.
//!
//! # Lock discipline
//!
//! The daemon's own `Singleton` gate ([`crate::singleton`]) and the
//! control socket's per-tick bookkeeping ([`crate::control`]) must both
//! have been released *before* [`run`] is called - it execs processes
//! that take `SourcesLock` themselves (both the probe build and the real
//! switch go through `xi <verb> build`/`switch`'s case-2 path), and a
//! caller that still held anything the callee also takes would deadlock.
//! [`run`] takes no lock of its own and asserts nothing about the
//! caller's state; the ordering is enforced by where `main.rs` calls it
//! (after `control.end_tick()`, outside the `Singleton` permit), not by
//! this module.

use std::path::{Path, PathBuf};

use xi_core::sources::{Scope, config::RebootWindow};

use crate::{
  exec::{Exec, Invocation},
  health::DegradedReason,
  machine_id,
  profile,
};

/// Where the currently booted system's kernel symlink lives on a NixOS
/// host. Absent on every other platform this daemon targets.
pub const BOOTED_KERNEL_PATH: &str = "/run/booted-system/kernel";

/// Which concrete `xi <verb> switch` this scope resolves to.
///
/// `Scope::System` is ambiguous on its own - NixOS, nix-darwin and
/// system-manager all manage "the system", and `xi switch` (bare) is
/// rejected for exactly this reason
/// ([`xi_core::sources::verbless_switch_error`]). The daemon runs on one
/// concrete host, though, so unlike the interactive CLI it does not need
/// the operator to say which: it probes the live host once at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchVerb {
  /// `xi nixos switch`.
  Nixos,
  /// `xi darwin switch`.
  Darwin,
  /// `xi system switch` (system-manager; non-NixOS Linux).
  System,
  /// `xi home switch`.
  Home,
}

impl SwitchVerb {
  /// The verb token `xi` itself accepts as a subcommand name.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Nixos => "nixos",
      Self::Darwin => "darwin",
      Self::System => "system",
      Self::Home => "home",
    }
  }
}

/// Marker file every NixOS installation carries. Absence means "some
/// other Linux", which is system-manager's territory.
const NIXOS_MARKER: &str = "/etc/NIXOS";

/// Resolve the switch verb for `scope` by probing the live host.
#[must_use]
pub fn resolve_switch_verb(scope: Scope) -> SwitchVerb {
  resolve_switch_verb_with(
    scope,
    cfg!(target_os = "macos"),
    Path::new(NIXOS_MARKER).exists(),
  )
}

/// [`resolve_switch_verb`] against injected host facts, for tests.
///
/// User scope is unambiguous (`home`) on every platform: home-manager has
/// no NixOS/darwin split the way system configuration does.
#[must_use]
pub const fn resolve_switch_verb_with(
  scope: Scope,
  is_macos: bool,
  is_nixos: bool,
) -> SwitchVerb {
  match scope {
    Scope::User => SwitchVerb::Home,
    Scope::System => {
      if is_macos {
        SwitchVerb::Darwin
      } else if is_nixos {
        SwitchVerb::Nixos
      } else {
        SwitchVerb::System
      }
    },
  }
}

/// Everything [`run`] needs to probe, gate, exec the switch, and register
/// its result.
#[derive(Debug, Clone, Copy)]
pub struct ActivateInputs<'a> {
  /// This host's identifying token ([`machine_id::host_token`]), resolved
  /// once by the caller. `None` means "could not be determined" - the
  /// guard's own "cannot decide" case, not "no guard configured".
  pub host_token:         Option<&'a str>,
  /// Which concrete verb to exec.
  pub verb:               SwitchVerb,
  /// The scope's state dir - where `active-generation` lands after a
  /// successful switch, and where this module reads it back from.
  pub state_dir:          &'a Path,
  /// The daemon's own named profile to register the result on.
  pub profile_path:       &'a Path,
  /// `--out-link` target for the build-only probe
  /// (`probe_build`) that [`run`] consults the pre-activation gates
  /// against. Owned by the caller so a test can pre-populate it with a
  /// fixture closure without a real `nix build` ever running.
  pub probe_out_link:     &'a Path,
  /// Operator override for the `machineId` guard. The daemon itself
  /// never sets this - it is CLI-only, surfaced on `xi-sources-daemon`
  /// as `--ignore-machine-id`, because an automated bypass of a safety
  /// check is not a safety check.
  pub ignore_machine_id:  bool,
  /// When a reboot-requiring activation may proceed. Defaults to
  /// [`RebootWindow::Always`] (no gate) in every caller that does not
  /// care.
  pub reboot_window:      RebootWindow,
  /// "Now", as minutes since local midnight - the caller's clock, not
  /// this module's, so a test can pick any instant without touching the
  /// real wall clock.
  pub minute_of_day:      u32,
  /// Where the currently booted system's kernel symlink lives.
  /// Production callers pass [`BOOTED_KERNEL_PATH`]; a test points this
  /// at a fixture so it never has to depend on what is actually booted
  /// on the machine running the test suite.
  pub booted_kernel_path: &'a str,
}

/// What activation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivateOutcome {
  /// `xi <verb> switch` completed; the generation may or may not have
  /// been registered on the named profile.
  Activated {
    /// Set when registration failed. The switch itself still stands -
    /// registration is bookkeeping for `nix-collect-garbage`, not a
    /// precondition the activation can be rolled back over.
    hook: Option<DegradedReason>,
  },
  /// The pending generation would require a reboot and
  /// `reboot_window` does not admit the current minute. Nothing was
  /// built or activated for real - the caller is expected to retry on
  /// a later tick, inside the window.
  Deferred {
    /// Minutes until `reboot_window` next admits an activation.
    minutes: u32,
  },
  /// The build-only probe failed, the `machineId` gate refused, or
  /// `xi <verb> switch` itself failed to spawn or exited non-zero. The
  /// sources dir already advanced (that happened in `tick::run`); this
  /// is purely "nothing got built/activated this time".
  Failed(DegradedReason),
}

/// Probe, gate, exec `xi <verb> switch`, and - on success - register
/// what it activated.
///
/// The probe (`probe_build`), the `machineId` guard, and the
/// reboot-window gate all run before any attempt to exec the real
/// switch: a `machineId` mismatch refuses, a reboot-requiring
/// generation outside `reboot_window` defers, and either way the real
/// switch - and the `SourcesLock` it takes - is never reached. When
/// both gates pass, the real switch is exactly the case
/// `xi_core::sources::switch::prepare`'s case 2 handles for an adopted
/// scope - resolve the conflict policy, take the sources lock, build,
/// activate, and write `active-generation`. The daemon's only remaining
/// job is to read that value back and hand it to
/// [`profile::register_generation`].
#[must_use]
pub fn run(exec: &dyn Exec, inputs: &ActivateInputs<'_>) -> ActivateOutcome {
  let store_path = match probe_build(exec, inputs) {
    Ok(store_path) => store_path,
    Err(reason) => return ActivateOutcome::Failed(reason),
  };

  if !inputs.ignore_machine_id
    && let Some(reason) =
      check_machine_id(inputs.host_token, store_path.as_deref())
  {
    return ActivateOutcome::Failed(reason);
  }

  if requires_reboot(
    booted_kernel_at(inputs.booted_kernel_path),
    generation_kernel(store_path.as_deref()),
  ) && !inputs.reboot_window.admits(inputs.minute_of_day)
  {
    return ActivateOutcome::Deferred {
      minutes: inputs
        .reboot_window
        .minutes_until_open(inputs.minute_of_day),
    };
  }

  let invocation = Invocation::new("xi", [inputs.verb.as_str(), "switch"]);

  let outcome = match exec.run(&invocation) {
    Ok(outcome) => outcome,
    Err(err) => {
      return ActivateOutcome::Failed(DegradedReason::ActivationFailed {
        detail: err.to_string(),
      });
    },
  };

  if !outcome.ok() {
    return ActivateOutcome::Failed(DegradedReason::ActivationFailed {
      detail: outcome.failure_text(),
    });
  }

  ActivateOutcome::Activated {
    hook: register(exec, inputs),
  }
}

/// Build (but do not activate) the pending generation, for the
/// pre-activation gates to inspect.
///
/// Runs `xi <verb> build --out-link <inputs.probe_out_link>` - the
/// existing `build` subcommand, which resolves an adopted scope's
/// installable through the same `prepare` case 2 path `switch` uses, but
/// returns after building rather than activating. `Ok(None)` means the
/// build reported success but left nothing resolvable at the out-link
/// (nothing this module can act on - the gates treat "no data" as
/// "nothing to enforce", not as a failure); a build that fails to spawn
/// or exits non-zero is the only case this returns `Err` for, since a
/// generation the daemon cannot even build is not one it should go on to
/// gate-check and activate.
fn probe_build(
  exec: &dyn Exec,
  inputs: &ActivateInputs<'_>,
) -> Result<Option<PathBuf>, DegradedReason> {
  let out_link = inputs.probe_out_link.to_string_lossy().into_owned();
  let invocation = Invocation::new("xi", [
    inputs.verb.as_str(),
    "build",
    "--out-link",
    &out_link,
  ]);

  let outcome = exec.run(&invocation).map_err(|err| {
    DegradedReason::ActivationFailed {
      detail: err.to_string(),
    }
  })?;

  if !outcome.ok() {
    return Err(DegradedReason::ActivationFailed {
      detail: outcome.failure_text(),
    });
  }

  Ok(std::fs::canonicalize(inputs.probe_out_link).ok())
}

/// The `machineId` guard ([[xi-sources-gitops]] section 7.2).
///
/// A generation that embeds no `etc/machine-id` at all (the common case:
/// most NixOS configurations never set `environment.etc."machine-
/// id"`) has nothing for this guard to compare, so it passes - the guard
/// exists for operators who explicitly pin a per-host id, not as a
/// blanket requirement every generation must satisfy. A probe that could
/// not resolve a store path at all ([`probe_build`] returning `None`)
/// gets the same treatment, for the same reason: no data is not evidence
/// of a mismatch.
fn check_machine_id(
  host_token: Option<&str>,
  store_path: Option<&Path>,
) -> Option<DegradedReason> {
  let generation_id = generation_machine_id(store_path?)?;
  machine_id::guard(host_token, &generation_id).err()
}

/// Read `<store_path>/etc/machine-id`, trimmed, treating absence or a
/// blank file as "this generation does not embed one".
fn generation_machine_id(store_path: &Path) -> Option<String> {
  std::fs::read_to_string(store_path.join("etc/machine-id"))
    .ok()
    .map(|raw| raw.trim().to_owned())
    .filter(|s| !s.is_empty())
}

/// The reboot-window gate ([[xi-sources-gitops]] section 10.2).
///
/// `true` only when both kernels are known and differ: "cannot
/// determine" (either symlink unreadable - not NixOS, or a probe that
/// resolved no store path) is treated as "no reboot needed", the same
/// "no data, nothing to enforce" stance the `machineId` guard takes.
/// This is a reasoned limitation, not a stub: a kernel comparison is the
/// one reboot-relevant signal this codebase can observe today, and
/// refusing to ever activate on a host where the comparison cannot be
/// made would be a worse failure mode than occasionally missing a
/// reboot recommendation on such a host.
#[must_use]
fn requires_reboot(booted: Option<PathBuf>, pending: Option<PathBuf>) -> bool {
  match (booted, pending) {
    (Some(booted), Some(pending)) => booted != pending,
    _ => false,
  }
}

/// The target of `<store_path>/kernel`, the symlink every NixOS system
/// closure carries pointing at its kernel package.
fn generation_kernel(store_path: Option<&Path>) -> Option<PathBuf> {
  std::fs::read_link(store_path?.join("kernel")).ok()
}

/// The target of the booted-kernel symlink at `path`
/// ([`BOOTED_KERNEL_PATH`] in production, a fixture in tests).
fn booted_kernel_at(path: &str) -> Option<PathBuf> {
  std::fs::read_link(path).ok()
}

/// Read back `active-generation` and register it on the named profile.
///
/// `Ok(None)` - no recorded generation - is not an error: a scope with no
/// recorded remote (case 3 of `prepare`) writes nothing, and `xi switch`
/// exiting zero on a tree that was not actually ours to adopt is not this
/// module's problem to flag.
fn register(
  exec: &dyn Exec,
  inputs: &ActivateInputs<'_>,
) -> Option<DegradedReason> {
  let active_generation = match xi_core::sources::read_state_file(
    inputs.state_dir,
    "active-generation",
  ) {
    Ok(value) => value,
    Err(err) => {
      return Some(DegradedReason::ProfileRegisterFailed {
        detail: format!("could not read active-generation: {err}"),
      });
    },
  };

  let store_path = active_generation?;

  profile::register_generation(exec, inputs.profile_path, &store_path)
    .err()
    .map(|err| {
      DegradedReason::ProfileRegisterFailed {
        detail: err.to_string(),
      }
    })
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]
mod tests {
  use tempfile::TempDir;

  use super::*;
  use crate::exec::recorder::RecordingExec;

  // -- resolve_switch_verb_with --------------------------------------

  #[test]
  fn user_scope_is_always_home() {
    assert_eq!(
      resolve_switch_verb_with(Scope::User, true, true),
      SwitchVerb::Home
    );
    assert_eq!(
      resolve_switch_verb_with(Scope::User, false, false),
      SwitchVerb::Home
    );
  }

  #[test]
  fn system_scope_on_macos_is_darwin() {
    assert_eq!(
      resolve_switch_verb_with(Scope::System, true, false),
      SwitchVerb::Darwin
    );
  }

  #[test]
  fn system_scope_with_the_nixos_marker_is_nixos() {
    assert_eq!(
      resolve_switch_verb_with(Scope::System, false, true),
      SwitchVerb::Nixos
    );
  }

  #[test]
  fn system_scope_with_neither_falls_back_to_system_manager() {
    assert_eq!(
      resolve_switch_verb_with(Scope::System, false, false),
      SwitchVerb::System
    );
  }

  // -- run --------------------------------------------------------------

  struct World {
    _tmp:               TempDir,
    state_dir:          std::path::PathBuf,
    profile:            std::path::PathBuf,
    probe_out_link:     std::path::PathBuf,
    booted_kernel_path: std::path::PathBuf,
  }

  impl World {
    fn new() -> Self {
      let tmp = TempDir::new().expect("tempdir");
      let state_dir = tmp.path().join("state");
      let profile = tmp.path().join("profile");
      // Deliberately not created: a probe whose out-link does not
      // resolve to anything is the common case in these tests, and
      // exercises "no data, nothing to enforce" the same way a probe
      // build that ran for real but produced nothing would. Same for
      // the booted-kernel path - not every test cares about the reboot
      // gate.
      let probe_out_link = tmp.path().join("probe-result");
      let booted_kernel_path = tmp.path().join("booted-kernel");
      Self {
        _tmp: tmp,
        state_dir,
        profile,
        probe_out_link,
        booted_kernel_path,
      }
    }

    fn inputs(&self) -> ActivateInputs<'_> {
      ActivateInputs {
        host_token:         None,
        verb:               SwitchVerb::Nixos,
        state_dir:          &self.state_dir,
        profile_path:       &self.profile,
        probe_out_link:     &self.probe_out_link,
        ignore_machine_id:  false,
        reboot_window:      RebootWindow::Always,
        minute_of_day:      0,
        booted_kernel_path: self
          .booted_kernel_path
          .to_str()
          .expect("utf8 path"),
      }
    }

    fn record_active_generation(&self, store_path: &str) {
      xi_core::sources::write_state_file(
        &self.state_dir,
        "active-generation",
        store_path,
      )
      .expect("write active-generation");
    }

    /// Simulate a probe build that resolved a real closure embedding
    /// `machine_id`.
    fn stage_probe_with_machine_id(&self, machine_id: &str) {
      let etc = self.probe_out_link.join("etc");
      std::fs::create_dir_all(&etc).expect("mkdir etc");
      std::fs::write(etc.join("machine-id"), machine_id)
        .expect("write machine-id");
    }

    /// Simulate a probe build whose closure's `kernel` symlink points at
    /// `target`. `target` need not exist: `fs::read_link` only reads the
    /// link's text.
    fn stage_probe_kernel(&self, target: &str) {
      std::fs::create_dir_all(&self.probe_out_link).expect("mkdir probe dir");
      std::os::unix::fs::symlink(target, self.probe_out_link.join("kernel"))
        .expect("symlink kernel");
    }

    /// Simulate the host currently having `target` booted.
    fn stage_booted_kernel(&self, target: &str) {
      std::os::unix::fs::symlink(target, &self.booted_kernel_path)
        .expect("symlink booted kernel");
    }
  }

  fn probe_invocation(world: &World) -> String {
    format!(
      "xi nixos build --out-link {}",
      world.probe_out_link.display()
    )
  }

  #[test]
  fn a_successful_switch_execs_the_probe_then_the_bare_switch_invocation() {
    let world = World::new();
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &world.inputs());

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec![
      probe_invocation(&world),
      "xi nixos switch".to_owned(),
    ]);
  }

  #[test]
  fn a_successful_switch_registers_the_recorded_generation() {
    let world = World::new();
    world.record_active_generation("/nix/store/abc-config");
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &world.inputs());

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec![
      probe_invocation(&world),
      "xi nixos switch".to_owned(),
      format!(
        "nix-env --profile {} --set /nix/store/abc-config",
        world.profile.display()
      ),
    ]);
  }

  #[test]
  fn a_switch_with_nothing_recorded_does_not_attempt_registration() {
    let world = World::new();
    // No `active-generation` written - e.g. `xi switch` ran against a
    // scope with no recorded remote.
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &world.inputs());

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec![
      probe_invocation(&world),
      "xi nixos switch".to_owned(),
    ]);
  }

  #[test]
  fn a_failing_probe_build_is_reported_and_never_reaches_switch() {
    let world = World::new();
    let exec = RecordingExec::always_ok().with_reply(
      1,
      "",
      "error: build of '/nix/store/abc-config.drv' failed",
    );

    let outcome = run(&exec, &world.inputs());

    match outcome {
      ActivateOutcome::Failed(reason) => {
        assert_eq!(reason.token(), "activation-failed");
        assert!(reason.to_string().contains("build of"));
      },
      other => panic!("expected Failed, got {other:?}"),
    }
    // Only the probe ran - the real switch never followed it.
    assert_eq!(exec.argv_log(), vec![probe_invocation(&world)]);
  }

  #[test]
  fn a_failing_switch_is_reported_and_never_registers() {
    let world = World::new();
    world.record_active_generation("/nix/store/stale-config");
    let exec = RecordingExec::always_ok()
      // Probe succeeds...
      .with_reply(0, "", "")
      // ...but the real switch fails.
      .with_reply(1, "", "error: activation script failed");

    let outcome = run(&exec, &world.inputs());

    match outcome {
      ActivateOutcome::Failed(reason) => {
        assert_eq!(reason.token(), "activation-failed");
        assert!(reason.to_string().contains("activation script"));
      },
      other => panic!("expected Failed, got {other:?}"),
    }
    // No `nix-env` call followed the failed switch.
    assert_eq!(exec.argv_log(), vec![
      probe_invocation(&world),
      "xi nixos switch".to_owned(),
    ]);
  }

  #[test]
  fn a_registration_failure_does_not_undo_the_activation() {
    let world = World::new();
    world.record_active_generation("/nix/store/abc-config");
    let exec = RecordingExec::always_ok()
      .with_reply(0, "", "") // probe
      .with_reply(0, "", "") // switch
      .with_reply(1, "", "error: profile is locked"); // nix-env

    let outcome = run(&exec, &world.inputs());

    match outcome {
      ActivateOutcome::Activated { hook: Some(reason) } => {
        assert_eq!(reason.token(), "profile-register-failed");
      },
      other => panic!("expected Activated with a degraded hook, got {other:?}"),
    }
  }

  #[test]
  fn other_verbs_spell_their_own_switch_invocation() {
    let world = World::new();
    let mut inputs = world.inputs();
    inputs.verb = SwitchVerb::Home;
    let exec = RecordingExec::always_ok();

    let _ = run(&exec, &inputs);

    assert_eq!(exec.argv_log(), vec![
      format!(
        "xi home build --out-link {}",
        world.probe_out_link.display()
      ),
      "xi home switch".to_owned(),
    ]);
  }

  // -- machineId gate -----------------------------------------------------

  #[test]
  fn a_blank_embedded_machine_id_file_activates_unconditionally() {
    let world = World::new();
    std::fs::create_dir_all(world.probe_out_link.join("etc"))
      .expect("mkdir etc");
    std::fs::write(
      world.probe_out_link.join("etc").join("machine-id"),
      "   \n",
    )
    .expect("write blank machine-id");
    let mut inputs = world.inputs();
    inputs.host_token = Some("AAAA");
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
  }

  #[test]
  fn matching_embedded_machine_id_activates() {
    let world = World::new();
    world.stage_probe_with_machine_id("AAAA");
    let mut inputs = world.inputs();
    inputs.host_token = Some("AAAA");
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec![
      probe_invocation(&world),
      "xi nixos switch".to_owned(),
    ]);
  }

  /// SC-G-007 / the spec's "Mismatched machine-id refuses" scenario,
  /// applied to the daemon's pre-activation gate: the real switch must
  /// never be reached.
  #[test]
  fn mismatched_embedded_machine_id_refuses_before_exec_ing_switch() {
    let world = World::new();
    world.stage_probe_with_machine_id("BBBB");
    let mut inputs = world.inputs();
    inputs.host_token = Some("AAAA");
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    match outcome {
      ActivateOutcome::Failed(reason) => {
        assert_eq!(reason.token(), "machine-id-mismatch");
        let text = reason.to_string();
        assert!(text.contains("AAAA"), "got: {text}");
        assert!(text.contains("BBBB"), "got: {text}");
      },
      other => panic!("expected Failed, got {other:?}"),
    }
    // Only the probe ran - the mismatch refused before the real switch.
    assert_eq!(exec.argv_log(), vec![probe_invocation(&world)]);
  }

  #[test]
  fn ignore_machine_id_bypasses_a_mismatch() {
    let world = World::new();
    world.stage_probe_with_machine_id("BBBB");
    let mut inputs = world.inputs();
    inputs.host_token = Some("AAAA");
    inputs.ignore_machine_id = true;
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec![
      probe_invocation(&world),
      "xi nixos switch".to_owned(),
    ]);
  }

  #[test]
  fn an_undecidable_host_token_refuses_a_generation_with_an_embedded_id() {
    let world = World::new();
    world.stage_probe_with_machine_id("BBBB");
    let inputs = world.inputs(); // host_token: None
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    match outcome {
      ActivateOutcome::Failed(reason) => {
        assert_eq!(reason.token(), "machine-id-mismatch");
      },
      other => panic!("expected Failed, got {other:?}"),
    }
  }

  // -- requires_reboot (pure) ----------------------------------------

  #[test]
  fn identical_kernels_never_require_a_reboot() {
    let kernel = PathBuf::from("/nix/store/same-kernel");
    assert!(!requires_reboot(Some(kernel.clone()), Some(kernel)));
  }

  #[test]
  fn differing_kernels_require_a_reboot() {
    assert!(requires_reboot(
      Some(PathBuf::from("/nix/store/old-kernel")),
      Some(PathBuf::from("/nix/store/new-kernel"))
    ));
  }

  #[test]
  fn an_undeterminable_kernel_on_either_side_never_requires_a_reboot() {
    assert!(!requires_reboot(
      None,
      Some(PathBuf::from("/nix/store/new"))
    ));
    assert!(!requires_reboot(
      Some(PathBuf::from("/nix/store/old")),
      None
    ));
    assert!(!requires_reboot(None, None));
  }

  // -- reboot-window gate (via run) -----------------------------------

  /// SC-G-009 / the spec's "Reboot-requiring activation deferred"
  /// scenario, applied to the daemon's pre-activation gate: the real
  /// switch must never be reached while the window is closed.
  #[test]
  fn a_reboot_requiring_generation_outside_the_window_defers() {
    let world = World::new();
    world.stage_booted_kernel("/nix/store/old-kernel");
    world.stage_probe_kernel("/nix/store/new-kernel");
    let mut inputs = world.inputs();
    inputs.reboot_window = RebootWindow::Daily {
      start: 120, // 02:00
      end:   240, // 04:00
    };
    inputs.minute_of_day = 14 * 60; // 14:00, outside the window
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Deferred { minutes: 12 * 60 });
    // Only the probe ran - the switch was never reached.
    assert_eq!(exec.argv_log(), vec![probe_invocation(&world)]);
  }

  #[test]
  fn a_reboot_requiring_generation_inside_the_window_activates() {
    let world = World::new();
    world.stage_booted_kernel("/nix/store/old-kernel");
    world.stage_probe_kernel("/nix/store/new-kernel");
    let mut inputs = world.inputs();
    inputs.reboot_window = RebootWindow::Daily {
      start: 120,
      end:   240,
    };
    inputs.minute_of_day = 3 * 60; // 03:00, inside the window
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec![
      probe_invocation(&world),
      "xi nixos switch".to_owned(),
    ]);
  }

  #[test]
  fn a_generation_not_requiring_a_reboot_ignores_the_window() {
    let world = World::new();
    world.stage_booted_kernel("/nix/store/same-kernel");
    world.stage_probe_kernel("/nix/store/same-kernel");
    let mut inputs = world.inputs();
    inputs.reboot_window = RebootWindow::Daily {
      start: 120,
      end:   240,
    };
    inputs.minute_of_day = 14 * 60; // outside the window, but no reboot needed
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
  }

  #[test]
  fn the_always_window_never_defers_even_when_a_reboot_is_needed() {
    let world = World::new();
    world.stage_booted_kernel("/nix/store/old-kernel");
    world.stage_probe_kernel("/nix/store/new-kernel");
    let mut inputs = world.inputs();
    inputs.reboot_window = RebootWindow::Always;
    inputs.minute_of_day = 14 * 60;
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
  }

  #[test]
  fn an_undeterminable_reboot_requirement_does_not_defer() {
    let world = World::new();
    // Neither kernel symlink staged - "cannot determine" - even with a
    // closed window, there is nothing for the gate to defer over.
    let mut inputs = world.inputs();
    inputs.reboot_window = RebootWindow::Daily {
      start: 120,
      end:   240,
    };
    inputs.minute_of_day = 14 * 60;
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &inputs);

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
  }
}
