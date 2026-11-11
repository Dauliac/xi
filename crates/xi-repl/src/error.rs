//! Repl-specific error types.
//!
//! Most of this crate reports failures via `color_eyre::eyre::eyre!`/`bail!`
//! like the rest of the workspace. The three outcomes here get a named
//! `thiserror` shape instead because a caller may need to branch on which
//! one happened  -  today just this crate's own loop (it prints a friendlier
//! hint for [`ReplError::DaemonUnreachable`] than a generic I/O message
//! would), later the `:raw` escape hatch from the proposal's Phase 3
//! (`openspec/changes/xi-repl/tasks.md` task 7.3), which needs to tell "the
//! daemon is just not there" apart from "the daemon answered but rejected
//! the expression".

use std::path::PathBuf;

use thiserror::Error;

/// Errors raised while driving one `ExprEval` round-trip against the
/// daemon.
#[derive(Debug, Error)]
pub enum ReplError {
  /// `client_v3::expr_eval` failed - either no daemon is listening on
  /// `socket` at all (connection refused/not found - the `--no-daemon`
  /// case, or a daemon that died mid-session), or one is listening but
  /// didn't answer in time (a real `nix eval` taking longer than the
  /// client's read timeout). `eval()` doesn't know at this point whether
  /// `--no-daemon` was passed, so the message stays neutral about why and
  /// just surfaces `cause` rather than guessing.
  #[error(
    "could not reach the daemon at {} for flake {}: {cause}  -  if no \
     daemon is running, drop --no-daemon (or run `xi develop daemon \
     start --flake {}` yourself first) to let `xi repl` manage it.",
    socket.display(), flake.display(), flake.display()
  )]
  DaemonUnreachable {
    socket: PathBuf,
    flake:  PathBuf,
    cause:  String,
  },

  /// The daemon answered and classified the expression as a genuine
  /// evaluation failure ([`super::ExprEvalResult::Error`]).
  #[error("eval error: {message}")]
  Eval { message: String },

  /// The daemon answered, but not with an `ExprEval` reply shape (a
  /// protocol mismatch, a pending/missing/degraded availability a plain
  /// query must never produce, or a v2-only daemon that has no `ExprEval`
  /// at all).
  #[error("daemon did not answer with a usable ExprEval reply: {detail}")]
  UnexpectedReply { detail: String },
}
