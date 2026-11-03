//! Lifecycle hooks: `on_advance`, `on_build`, `on_switch`.
//!
//! Three points, fixed by the spec, each an operator-supplied shell
//! command string run through `sh -c`. A hook is a notification, not a
//! precondition: its failure is logged and surfaced as `Degraded` on the
//! next status call, but it never rolls back the step that triggered it
//! (the advance, the build, or the activation already happened by the
//! time the hook runs).
//!
//! Output goes to `<state dir>/hooks/<point>.log`, rotated one generation
//! deep (`<point>.log` becomes `<point>.log.1` before the new run is
//! written) so a runaway hook does not grow its log without bound, while
//! an operator debugging a failure still has the previous run to compare
//! against.

use std::{fs, io, path::Path};

use crate::{
  exec::{Exec, Invocation, Outcome},
  health::DegradedReason,
};

/// Subdirectory of the state dir hook logs live under.
pub const HOOKS_SUBDIR: &str = "hooks";

/// What one hook invocation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRun {
  /// The hook point this was, in its token spelling (`on_advance`).
  pub point:   &'static str,
  /// What the command did.
  pub outcome: Outcome,
}

impl HookRun {
  /// Whether the hook needs to be surfaced as `Degraded`.
  #[must_use]
  pub fn degraded_reason(&self) -> Option<DegradedReason> {
    if self.outcome.ok() {
      None
    } else {
      Some(DegradedReason::HookFailed {
        hook: self.point.to_owned(),
        code: self.outcome.code.unwrap_or(-1),
      })
    }
  }
}

/// Run `command` for hook point `point`, capturing output to the state
/// dir.
///
/// `point` MUST be one of `"on_advance"`, `"on_build"`, `"on_switch"` -
/// the three the spec names. It is not validated here because the three
/// call sites each pass a `'static` literal; a fourth call site would be
/// a bug caught by the `DegradedReason` token tests, not something this
/// function needs to guard against at runtime.
///
/// A log write failure does not fail the hook: the command's own exit
/// code is what matters to the caller, and a full disk should not be
/// confused with a hook that misbehaved.
pub fn run(
  exec: &dyn Exec,
  state_dir: &Path,
  point: &'static str,
  command: &str,
) -> HookRun {
  let invocation = Invocation::new("sh", ["-c", command]);
  let outcome = exec.run(&invocation).unwrap_or_else(|err| {
    Outcome {
      code:   None,
      stdout: String::new(),
      stderr: format!("failed to spawn hook `{point}`: {err}"),
    }
  });

  if let Err(err) = write_log(state_dir, point, &outcome) {
    tracing::warn!(point, error = %err, "could not write hook log");
  }

  HookRun { point, outcome }
}

/// Rotate the previous log (if any) and write this run's output.
fn write_log(
  state_dir: &Path,
  point: &str,
  outcome: &Outcome,
) -> io::Result<()> {
  let dir = state_dir.join(HOOKS_SUBDIR);
  fs::create_dir_all(&dir)?;

  let current = dir.join(format!("{point}.log"));
  let previous = dir.join(format!("{point}.log.1"));
  if current.exists() {
    fs::rename(&current, &previous)?;
  }

  let body = format!(
    "exit: {}\n--- stdout ---\n{}\n--- stderr ---\n{}\n",
    outcome
      .code
      .map_or_else(|| "signalled".to_owned(), |c| c.to_string()),
    outcome.stdout,
    outcome.stderr,
  );
  fs::write(&current, body)
}

/// Path a hook's current log would be written to.
#[must_use]
pub fn log_path(state_dir: &Path, point: &str) -> std::path::PathBuf {
  state_dir.join(HOOKS_SUBDIR).join(format!("{point}.log"))
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use tempfile::TempDir;

  use super::*;
  use crate::exec::recorder::RecordingExec;

  #[test]
  fn a_successful_hook_carries_no_degraded_reason() {
    let tmp = TempDir::new().expect("tempdir");
    let exec = RecordingExec::always_ok();
    let run = run(&exec, tmp.path(), "on_advance", "true");
    assert!(run.outcome.ok());
    assert!(run.degraded_reason().is_none());
  }

  #[test]
  fn a_failing_hook_is_surfaced_as_hook_failed_with_its_point_and_code() {
    let tmp = TempDir::new().expect("tempdir");
    let exec =
      RecordingExec::always_ok().with_reply(3, "", "service restart failed");
    let run = run(&exec, tmp.path(), "on_switch", "systemctl restart svc");

    let reason = run
      .degraded_reason()
      .expect("a non-zero exit must be degraded");
    assert_eq!(reason.token(), "hook-on_switch-failed");
    assert!(reason.to_string().contains('3'), "got: {reason}");
  }

  #[test]
  fn the_command_runs_through_a_shell_so_operators_can_use_shell_syntax() {
    let tmp = TempDir::new().expect("tempdir");
    let exec = RecordingExec::always_ok();
    run(&exec, tmp.path(), "on_build", "echo hi && echo bye");
    assert_eq!(exec.argv_log(), vec![
      "sh -c echo hi && echo bye".to_owned()
    ]);
  }

  #[test]
  fn output_is_captured_to_the_hooks_log() {
    let tmp = TempDir::new().expect("tempdir");
    let exec = RecordingExec::always_ok().with_reply(0, "built ok\n", "");
    run(&exec, tmp.path(), "on_build", "nix build");

    let log = fs::read_to_string(log_path(tmp.path(), "on_build"))
      .expect("log should exist");
    assert!(log.contains("exit: 0"), "got: {log}");
    assert!(log.contains("built ok"), "got: {log}");
  }

  #[test]
  fn a_second_run_rotates_the_first_log_rather_than_overwriting_it() {
    let tmp = TempDir::new().expect("tempdir");
    let first = RecordingExec::always_ok().with_reply(0, "first run", "");
    run(&first, tmp.path(), "on_advance", "echo");

    let second = RecordingExec::always_ok().with_reply(0, "second run", "");
    run(&second, tmp.path(), "on_advance", "echo");

    let dir = tmp.path().join(HOOKS_SUBDIR);
    let current =
      fs::read_to_string(dir.join("on_advance.log")).expect("current");
    let rotated =
      fs::read_to_string(dir.join("on_advance.log.1")).expect("rotated");
    assert!(current.contains("second run"), "got: {current}");
    assert!(rotated.contains("first run"), "got: {rotated}");
  }
}
