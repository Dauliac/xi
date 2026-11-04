//! Build, activate and register a generation after a successful advance.
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
//! [`run`] therefore does two things only: exec the switch, and - on
//! success - read back the `active-generation` that exec just wrote and
//! register it on the daemon's own named profile
//! ([`crate::profile::register_generation`]), the profile
//! `register_generation`'s own doc says is section 6.2's job.
//!
//! # Lock discipline
//!
//! The daemon's own `Singleton` gate ([`crate::singleton`]) and the
//! control socket's per-tick bookkeeping ([`crate::control`]) must both
//! have been released *before* [`run`] is called - it execs a process
//! that takes `SourcesLock` itself, and a caller that still held
//! anything the callee also takes would deadlock. [`run`] takes no lock
//! of its own and asserts nothing about the caller's state; the ordering
//! is enforced by where `main.rs` calls it (after `control.end_tick()`,
//! outside the `Singleton` permit), not by this module.
//!
//! # Pre-activation gates
//!
//! The `machineId` guard ([[xi-sources-gitops]] section 7.2) and the
//! reboot-window check (section 10.2) run *before* this module decides to
//! exec switch at all - they are what [`run`]'s caller consults first,
//! short-circuiting to [`crate::health::DegradedReason::MachineIdMismatch`]
//! or `TickOutcome::Deferred` without ever reaching this module. Fully
//! wired by the sibling `xi-bqf` / `xi-96k` work.

use std::path::Path;

use xi_core::sources::Scope;

use crate::{
  exec::{Exec, Invocation},
  health::DegradedReason,
  profile,
};

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

/// Everything [`run`] needs to exec the switch and register its result.
#[derive(Debug, Clone, Copy)]
pub struct ActivateInputs<'a> {
  /// Which concrete verb to exec.
  pub verb:         SwitchVerb,
  /// The scope's state dir - where `active-generation` lands after a
  /// successful switch, and where this module reads it back from.
  pub state_dir:    &'a Path,
  /// The daemon's own named profile to register the result on.
  pub profile_path: &'a Path,
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
  /// `xi <verb> switch` failed to spawn or exited non-zero.
  Failed(DegradedReason),
}

/// Exec `xi <verb> switch` and, on success, register what it activated.
///
/// No positional argument is passed: this is exactly the case
/// `xi_core::sources::switch::prepare`'s case 2 handles for an adopted
/// scope - resolve the conflict policy, take the sources lock, build,
/// activate, and write `active-generation`. The daemon's only remaining
/// job is to read that value back and hand it to
/// [`profile::register_generation`].
#[must_use]
pub fn run(exec: &dyn Exec, inputs: &ActivateInputs<'_>) -> ActivateOutcome {
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
  let active_generation =
    match xi_core::sources::read_state_file(inputs.state_dir, "active-generation")
    {
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
    _tmp:      TempDir,
    state_dir: std::path::PathBuf,
    profile:   std::path::PathBuf,
  }

  impl World {
    fn new() -> Self {
      let tmp = TempDir::new().expect("tempdir");
      let state_dir = tmp.path().join("state");
      let profile = tmp.path().join("profile");
      Self {
        _tmp: tmp,
        state_dir,
        profile,
      }
    }

    fn inputs(&self) -> ActivateInputs<'_> {
      ActivateInputs {
        verb:         SwitchVerb::Nixos,
        state_dir:    &self.state_dir,
        profile_path: &self.profile,
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
  }

  #[test]
  fn a_successful_switch_execs_the_bare_verb_switch_invocation() {
    let world = World::new();
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &world.inputs());

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec!["xi nixos switch".to_owned()]);
  }

  #[test]
  fn a_successful_switch_registers_the_recorded_generation() {
    let world = World::new();
    world.record_active_generation("/nix/store/abc-config");
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &world.inputs());

    assert_eq!(outcome, ActivateOutcome::Activated { hook: None });
    assert_eq!(exec.argv_log(), vec![
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
    assert_eq!(exec.argv_log(), vec!["xi nixos switch".to_owned()]);
  }

  #[test]
  fn a_failing_switch_is_reported_and_never_registers() {
    let world = World::new();
    world.record_active_generation("/nix/store/stale-config");
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
    // Only the switch invocation ran - no `nix-env` call followed it.
    assert_eq!(exec.argv_log(), vec!["xi nixos switch".to_owned()]);
  }

  #[test]
  fn a_registration_failure_does_not_undo_the_activation() {
    let world = World::new();
    world.record_active_generation("/nix/store/abc-config");
    let exec = RecordingExec::always_ok().with_reply(0, "", "").with_reply(
      1,
      "",
      "error: profile is locked",
    );

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

    assert_eq!(exec.argv_log(), vec!["xi home switch".to_owned()]);
  }
}
