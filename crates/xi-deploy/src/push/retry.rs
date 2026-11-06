//! Transience classification for [`SshTransport`](super::engine::SshTransport)'s
//! raw process results.
//!
//! `run`, `copy_closure` and `sync_dir` are the only code that sees the raw
//! `ssh`/`rsync`/`nix copy` exit status, which is why the classifier lives
//! here rather than in `xi_core::retry`: deciding what a given exit code
//! *means* is a judgement specific to those spawns, while the retry loop
//! itself (`xi_core::retry::retry`) stays agnostic to what it is retrying.

use std::process::ExitStatus;

use color_eyre::Result;
use xi_core::retry::Transience;

/// Classify a completed `ssh`/`rsync`/`nix copy` spawn for
/// [`xi_core::retry::retry`].
///
/// A local spawn failure (`Err`) is always transient: the binary existing
/// one second and not the next is itself the transient condition. Of a
/// completed process, exit code `255` is `ssh`'s own "could not establish
/// or maintain the connection" code and is classified transient; any other
/// non-zero is the remote command's own failure and is never retried
/// automatically, because the remote side may have already made a
/// partial, non-idempotent change.
#[must_use]
pub fn classify_ssh_exit(result: &Result<ExitStatus>) -> Transience {
  result
    .as_ref()
    .map_or(Transience::Transient, |status| match status.code() {
      Some(255) => Transience::Transient,
      _ => Transience::Permanent,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use std::process::Command;

  use color_eyre::eyre::eyre;

  use super::*;

  /// An `ExitStatus` for the given code, produced by actually running a
  /// shell rather than hand-rolling the platform-specific raw encoding.
  fn exit_status(code: i32) -> ExitStatus {
    Command::new("sh")
      .arg("-c")
      .arg(format!("exit {code}"))
      .status()
      .unwrap()
  }

  #[test]
  fn a_local_spawn_failure_is_transient() {
    let result: Result<ExitStatus> = Err(eyre!("failed to spawn ssh"));
    assert_eq!(classify_ssh_exit(&result), Transience::Transient);
  }

  #[test]
  fn exit_255_is_transient() {
    let result: Result<ExitStatus> = Ok(exit_status(255));
    assert_eq!(classify_ssh_exit(&result), Transience::Transient);
  }

  #[test]
  fn exit_1_is_permanent() {
    let result: Result<ExitStatus> = Ok(exit_status(1));
    assert_eq!(classify_ssh_exit(&result), Transience::Permanent);
  }

  #[test]
  fn exit_0_is_permanent() {
    let result: Result<ExitStatus> = Ok(exit_status(0));
    assert_eq!(classify_ssh_exit(&result), Transience::Permanent);
  }

  #[test]
  fn other_non_zero_exits_are_permanent() {
    for code in [2, 42, 127] {
      let result: Result<ExitStatus> = Ok(exit_status(code));
      assert_eq!(
        classify_ssh_exit(&result),
        Transience::Permanent,
        "exit {code} should not be retried"
      );
    }
  }
}
