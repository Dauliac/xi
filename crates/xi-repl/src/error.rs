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
  /// No daemon answered on `socket` and `--no-daemon` was passed, so the
  /// session never attempted to spawn one for `flake`.
  #[error(
    "no daemon reachable at {}  -  started with --no-daemon, so none was \
     spawned. Run `xi develop daemon start --flake {}` first, or drop \
     --no-daemon to let `xi repl` manage it.",
    socket.display(), flake.display()
  )]
  DaemonUnreachable { socket: PathBuf, flake: PathBuf },

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
