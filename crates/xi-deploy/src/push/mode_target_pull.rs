//! `--mode target-pull`: ask the target's own `xi-sources-daemon` to
//! fetch-and-fast-forward itself, instead of the operator copying bytes
//! to it ([[xi-push-target-pull]]).
//!
//! Every byte of the actual transport lives in
//! [`super::engine::Transport::advance_now`] (the `ssh ... --
//! xi-sources-daemon ctl` relay); this module only sends the one
//! [`AdvanceNow`] request a target-pull push needs and maps the daemon's
//! [`AdvanceReply`] onto the same success/error reporting every other
//! push mode already uses.

use color_eyre::{Result, eyre::bail};
use xi_core::sources::Scope;
use xi_sources_daemon::control::{AdvanceNow, AdvanceReply};

use super::engine::{TargetAddr, Transport};

/// Ask `target` to fetch-and-fast-forward to `to_sha` right now, through
/// its own `xi-sources-daemon` control socket.
///
/// - [`AdvanceReply::Advanced`] is success.
/// - [`AdvanceReply::NotFastForward`] is a conflict: the target's sources dir
///   is not an ancestor of `to_sha` (a force-pushed remote, or a local
///   divergence on the target), which `xi push` cannot resolve from here.
/// - [`AdvanceReply::Busy`] means a tick (poll-driven, or a previous
///   `AdvanceNow`) is already in flight; `AdvanceNow` never queues, so the
///   caller should retry rather than wait.
///
/// # Errors
///
/// Returns an error if [`Transport::advance_now`] itself fails (no
/// daemon listening, the SSH relay could not be spawned, the reply frame
/// could not be decoded), or if the daemon replies `NotFastForward` or
/// `Busy`.
pub fn advance(
  transport: &dyn Transport,
  target: &TargetAddr,
  scope: Scope,
  to_sha: String,
) -> Result<()> {
  let request = AdvanceNow {
    to_sha,
    requester_pid: std::process::id(),
  };
  match transport.advance_now(target, scope, &request)? {
    AdvanceReply::Advanced { .. } => Ok(()),
    AdvanceReply::NotFastForward => {
      bail!(
        "target-pull on {} refused: its sources dir is not a fast-forward \
         descendant of the requested commit; resolve the conflict on the \
         target before retrying",
        target.name
      )
    },
    AdvanceReply::Busy => {
      bail!(
        "target-pull on {} is busy (a tick is already in flight); retry the \
         push",
        target.name
      )
    },
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::sync::Mutex;

  use super::*;

  /// Records every `advance_now` call and answers from a scripted
  /// reply list, one entry per call.
  struct RecordingTransport {
    replies: Mutex<Vec<AdvanceReply>>,
    calls:   Mutex<Vec<(String, Scope, AdvanceNow)>>,
  }

  impl RecordingTransport {
    fn new(replies: Vec<AdvanceReply>) -> Self {
      Self {
        replies: Mutex::new(replies),
        calls:   Mutex::new(Vec::new()),
      }
    }
  }

  #[expect(
    clippy::unreachable,
    reason = "Fine in tests; the unreachable! calls are unimplemented \
              Transport methods this module's tests never exercise"
  )]
  impl Transport for RecordingTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      _cmd: &super::super::engine::RemoteCommand,
    ) -> Result<super::super::engine::Outcome> {
      unreachable!("not exercised by target-pull tests")
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: super::super::args::CopyVia,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("not exercised by target-pull tests")
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("not exercised by target-pull tests")
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
      unreachable!("not exercised by target-pull tests")
    }

    fn advance_now(
      &self,
      target: &TargetAddr,
      scope: Scope,
      request: &AdvanceNow,
    ) -> Result<AdvanceReply> {
      self.calls.lock().expect("calls lock").push((
        target.name.clone(),
        scope,
        request.clone(),
      ));
      let mut replies = self.replies.lock().expect("replies lock");
      Ok(if replies.is_empty() {
        AdvanceReply::Advanced {
          new_head: request.to_sha.clone(),
        }
      } else {
        replies.remove(0)
      })
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
  fn an_advanced_reply_is_success() {
    let transport = RecordingTransport::new(vec![AdvanceReply::Advanced {
      new_head: "deadbeef".to_owned(),
    }]);
    advance(&transport, &target(), Scope::System, "deadbeef".to_owned())
      .expect("advances");
  }

  #[test]
  fn a_not_fast_forward_reply_is_a_conflict_error() {
    let transport = RecordingTransport::new(vec![AdvanceReply::NotFastForward]);
    let err =
      advance(&transport, &target(), Scope::System, "deadbeef".to_owned())
        .expect_err("conflict");
    assert!(
      format!("{err}").contains("not a fast-forward"),
      "got: {err}"
    );
  }

  #[test]
  fn a_busy_reply_asks_the_caller_to_retry() {
    let transport = RecordingTransport::new(vec![AdvanceReply::Busy]);
    let err =
      advance(&transport, &target(), Scope::System, "deadbeef".to_owned())
        .expect_err("busy");
    assert!(format!("{err}").contains("retry"), "got: {err}");
  }

  #[test]
  fn the_request_carries_the_requested_sha_and_this_processs_pid() {
    let transport = RecordingTransport::new(vec![AdvanceReply::Advanced {
      new_head: "deadbeef".to_owned(),
    }]);
    advance(&transport, &target(), Scope::User, "deadbeef".to_owned())
      .expect("advances");

    let calls = transport.calls.lock().expect("calls lock").clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "web1");
    assert_eq!(calls[0].1, Scope::User);
    assert_eq!(calls[0].2.to_sha, "deadbeef");
    assert_eq!(calls[0].2.requester_pid, std::process::id());
  }
}
