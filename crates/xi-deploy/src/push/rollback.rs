//! Magic rollback: the operator-side canary loop the `xi-sources-push`
//! spec's "Magic rollback" requirement describes.
//!
//! deploy-rs's `danger_zone` (research `push-deploy.md` §"Per-tool
//! extraction") puts the timer and the revert decision on the *target*:
//! the operator deletes a sentinel to confirm, and the target reverts
//! itself if nobody does. Our spec inverts that: the *operator* polls a
//! sentinel over SSH and is the one who decides to roll back, which is
//! why every step below goes through [`Transport`] rather than assuming
//! anything about what the target does on its own. That also makes the
//! whole dance unit-testable against a fake transport, with no real
//! target, clock, or sleep longer than a test can afford.

use std::{
  thread,
  time::{Duration, Instant},
};

use color_eyre::{Result, eyre::bail};
use tracing::{info, warn};

use super::engine::{RemoteCommand, TargetAddr, Transport, shell_quote};

/// Well-known path the sentinel lives at on every target.
///
/// `/run` because it is tmpfs on every systemd target this capability
/// cares about: a sentinel that is not cleared explicitly still cannot
/// outlive a reboot, which matches "absence means the target rebooted or
/// lost the ssh config" from the spec's step 3.
pub const SENTINEL_PATH: &str = "/run/xi-push-magic-rollback.sentinel";

/// Well-known path of the pre-planted rollback escape hatch.
pub const ESCAPE_HATCH_PATH: &str = "/run/xi-push-magic-rollback.sh";

/// What a confirmation window ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackOutcome {
  /// The sentinel stayed readable for the whole window: the push stands.
  Confirmed,
  /// The sentinel went missing or unreadable; rollback was triggered.
  RolledBack,
}

/// Plant the sentinel and the escape-hatch script on `target`.
///
/// Called once, immediately after a successful activation and before the
/// confirmation window opens. The two are planted together in one SSH
/// round trip: a sentinel with no escape hatch yet, or vice versa, is a
/// window where this capability's own two steps race each other for no
/// benefit.
///
/// # Errors
///
/// Returns an error if the local `ssh` could not be spawned, or if the
/// remote planting command exits non-zero.
pub fn plant(
  transport: &dyn Transport,
  target: &TargetAddr,
  interactive_sudo: bool,
  retry_attempts: u32,
) -> Result<()> {
  let script = format!(
    "set -e; mkdir -p $(dirname {sentinel}); date +%s > {sentinel}; printf \
     '%s\\n' '#!/bin/sh' 'exec switch-to-configuration --rollback' > {hatch}; \
     chmod 0700 {hatch}",
    sentinel = shell_quote(SENTINEL_PATH),
    hatch = shell_quote(ESCAPE_HATCH_PATH),
  );

  let outcome = transport.run(
    target,
    &RemoteCommand::elevated(script, interactive_sudo, retry_attempts),
  )?;
  if !outcome.success {
    bail!(
      "planting the magic-rollback sentinel on {} failed: {}",
      target.name,
      outcome.message()
    );
  }
  Ok(())
}

/// Probe whether the sentinel is still readable on `target`.
///
/// `Ok(false)` covers both documented causes of absence equally: the
/// file is gone, or the SSH connection itself failed. The spec's step 3
/// treats a lost connection the same as a lost file ("absence means the
/// target rebooted or lost the ssh config"), so this does not
/// distinguish them either.
///
/// # Errors
///
/// Returns an error only if the local `ssh` could not be spawned at all.
pub fn probe(transport: &dyn Transport, target: &TargetAddr) -> Result<bool> {
  let outcome = transport.run(
    target,
    &RemoteCommand::probe(format!("test -r {}", shell_quote(SENTINEL_PATH))),
  )?;
  Ok(outcome.success)
}

/// Run the pre-planted escape hatch, rolling the target back.
///
/// # Errors
///
/// Returns an error if the local `ssh` could not be spawned, or if the
/// escape hatch exits non-zero.
pub fn trigger(
  transport: &dyn Transport,
  target: &TargetAddr,
  interactive_sudo: bool,
  retry_attempts: u32,
) -> Result<()> {
  let outcome = transport.run(
    target,
    &RemoteCommand::elevated(
      format!("sh {}", shell_quote(ESCAPE_HATCH_PATH)),
      interactive_sudo,
      retry_attempts,
    ),
  )?;
  if !outcome.success {
    bail!(
      "magic-rollback escape hatch failed on {}: {}",
      target.name,
      outcome.message()
    );
  }
  Ok(())
}

/// Run the full confirmation window on `target`: plant, poll until
/// `window` elapses or the sentinel goes missing, and roll back in the
/// latter case.
///
/// `poll_interval` is a parameter rather than a fixed constant so a test
/// can shrink both it and `window` to keep the real
/// [`thread::sleep`] calls well under a second;
/// production callers are expected to pass a few seconds.
///
/// # Errors
///
/// Propagates any [`Transport`] error from planting, probing, or
/// triggering the rollback.
pub fn confirm(
  transport: &dyn Transport,
  target: &TargetAddr,
  window: Duration,
  poll_interval: Duration,
  interactive_sudo: bool,
  retry_attempts: u32,
) -> Result<RollbackOutcome> {
  plant(transport, target, interactive_sudo, retry_attempts)?;
  let deadline = Instant::now() + window;

  loop {
    if !probe(transport, target)? {
      warn!(
        target = %target.name,
        "magic-rollback sentinel unreadable; triggering rollback"
      );
      trigger(transport, target, interactive_sudo, retry_attempts)?;
      return Ok(RollbackOutcome::RolledBack);
    }

    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
      info!(
        target = %target.name,
        "magic-rollback window elapsed; push confirmed"
      );
      return Ok(RollbackOutcome::Confirmed);
    }

    thread::sleep(poll_interval.min(remaining));
  }
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! calls are unimplemented Transport \
            methods this module's tests never exercise"
)]
mod tests {
  use std::sync::Mutex;

  use super::*;
  use crate::push::{args::CopyVia, engine::Elevate};

  /// Records every command run against it and answers from a scripted
  /// per-call outcome list, falling back to success once the script is
  /// exhausted - the common case where a test only cares about the
  /// first few calls.
  struct ScriptedTransport {
    /// Per-call `(unused label, outcome)`, consumed in order; the label
    /// is unused but keeps each script entry self-documenting at the
    /// call site.
    script:   Mutex<Vec<(&'static str, bool)>>,
    /// Every [`RemoteCommand`] run against this transport, in order -
    /// kept whole (not just its script text) so a test can assert on
    /// [`Elevate`] and `pty` too, which is this module's own output and
    /// not [`super::super::engine::SshTransport`]'s wrapping.
    commands: Mutex<Vec<RemoteCommand>>,
  }

  impl ScriptedTransport {
    fn new(script: Vec<(&'static str, bool)>) -> Self {
      Self {
        script:   Mutex::new(script),
        commands: Mutex::new(Vec::new()),
      }
    }

    fn commands(&self) -> Vec<RemoteCommand> {
      self.commands.lock().expect("commands lock").clone()
    }
  }

  impl Transport for ScriptedTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      cmd: &RemoteCommand,
    ) -> Result<crate::push::engine::Outcome> {
      self
        .commands
        .lock()
        .expect("commands lock")
        .push(cmd.clone());
      let mut script = self.script.lock().expect("script lock");
      let success = if script.is_empty() {
        true
      } else {
        script.remove(0).1
      };
      Ok(if success {
        crate::push::engine::Outcome::ok("")
      } else {
        crate::push::engine::Outcome::failed("unreachable")
      })
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: CopyVia,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("not exercised by magic-rollback tests")
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("not exercised by magic-rollback tests")
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
      unreachable!("not exercised by magic-rollback tests")
    }
  }

  fn target() -> TargetAddr {
    TargetAddr {
      name:     "web1".to_owned(),
      ssh_host: "web1".to_owned(),
      ssh_opts: Vec::new(),
    }
  }

  #[test]
  fn planting_writes_the_sentinel_and_the_escape_hatch_in_one_command() {
    let transport = ScriptedTransport::new(vec![("", true)]);
    plant(&transport, &target(), false, 3).expect("plants");
    let commands = transport.commands();
    assert_eq!(commands.len(), 1, "one round trip, not two");
    assert!(commands[0].script.contains(SENTINEL_PATH));
    assert!(commands[0].script.contains(ESCAPE_HATCH_PATH));
    assert!(
      commands[0]
        .script
        .contains("switch-to-configuration --rollback")
    );
  }

  #[test]
  fn a_failed_plant_is_an_error() {
    let transport = ScriptedTransport::new(vec![("", false)]);
    let err = plant(&transport, &target(), false, 3).expect_err("plant fails");
    assert!(format!("{err}").contains("planting"), "got: {err}");
  }

  #[test]
  fn probe_reads_true_when_the_sentinel_is_readable() {
    let transport = ScriptedTransport::new(vec![("", true)]);
    assert!(probe(&transport, &target()).expect("probes"));
    assert!(
      transport.commands()[0].script.contains(SENTINEL_PATH),
      "probe should test the sentinel path"
    );
  }

  #[test]
  fn probe_reads_false_without_erroring_when_the_target_is_unreachable() {
    let transport = ScriptedTransport::new(vec![("", false)]);
    assert!(!probe(&transport, &target()).expect("probe does not error"));
  }

  #[test]
  fn trigger_runs_the_escape_hatch() {
    let transport = ScriptedTransport::new(vec![("", true)]);
    trigger(&transport, &target(), false, 3).expect("triggers");
    assert!(transport.commands()[0].script.contains(ESCAPE_HATCH_PATH));
  }

  #[test]
  fn a_failed_trigger_is_an_error() {
    let transport = ScriptedTransport::new(vec![("", false)]);
    let err = trigger(&transport, &target(), false, 3).expect_err("fails");
    assert!(format!("{err}").contains("escape hatch"), "got: {err}");
  }

  #[test]
  fn a_sentinel_readable_for_the_whole_window_confirms() {
    // plant + at least one probe, all succeeding.
    let transport =
      ScriptedTransport::new(vec![("", true), ("", true), ("", true)]);
    let outcome = confirm(
      &transport,
      &target(),
      Duration::from_millis(20),
      Duration::from_millis(5),
      false,
      3,
    )
    .expect("confirms");
    assert_eq!(outcome, RollbackOutcome::Confirmed);
    // Last command must not be the escape hatch.
    let commands = transport.commands();
    assert!(
      !commands
        .last()
        .expect("a command ran")
        .script
        .contains("rollback.sh")
    );
  }

  #[test]
  fn a_missing_sentinel_triggers_rollback_within_the_window() {
    // plant succeeds, first probe fails -> trigger.
    let transport = ScriptedTransport::new(vec![("", true), ("", false)]);
    let outcome = confirm(
      &transport,
      &target(),
      Duration::from_mins(1),
      Duration::from_millis(5),
      false,
      3,
    )
    .expect("confirms");
    assert_eq!(outcome, RollbackOutcome::RolledBack);
    let commands = transport.commands();
    assert_eq!(commands.len(), 3, "plant, probe, trigger");
    assert!(commands[2].script.contains(ESCAPE_HATCH_PATH));
  }

  #[test]
  fn a_zero_window_confirms_after_a_single_probe() {
    let transport = ScriptedTransport::new(vec![("", true), ("", true)]);
    let outcome = confirm(
      &transport,
      &target(),
      Duration::ZERO,
      Duration::from_millis(1),
      false,
      3,
    )
    .expect("confirms");
    assert_eq!(outcome, RollbackOutcome::Confirmed);
  }

  #[test]
  fn interactive_sudo_propagates_to_the_elevated_steps() {
    let transport = ScriptedTransport::new(vec![("", true), ("", false)]);
    confirm(
      &transport,
      &target(),
      Duration::from_secs(5),
      Duration::from_millis(5),
      true,
      3,
    )
    .expect("confirms");
    // Elevated commands under interactive sudo request a PTY and
    // `Elevate::Interactive`; the probe in between is never elevated.
    let commands = transport.commands();
    assert_eq!(commands.len(), 3, "plant, probe, trigger");
    assert_eq!(commands[0].elevate, Elevate::Interactive);
    assert!(commands[0].pty);
    assert_eq!(commands[1].elevate, Elevate::No, "a probe");
    assert_eq!(commands[2].elevate, Elevate::Interactive);
    assert!(commands[2].pty);
  }

  #[test]
  fn retry_attempts_propagates_to_plant_and_trigger_but_not_the_probe() {
    let transport = ScriptedTransport::new(vec![("", true), ("", false)]);
    confirm(
      &transport,
      &target(),
      Duration::from_secs(5),
      Duration::from_millis(5),
      false,
      9,
    )
    .expect("confirms");
    let commands = transport.commands();
    assert_eq!(commands.len(), 3, "plant, probe, trigger");
    assert_eq!(commands[0].retry_attempts, 9, "plant");
    assert_eq!(commands[2].retry_attempts, 9, "trigger");
  }
}
