//! Client-side hook internals for the async CQRS daemon.
//!
//! Composed submodules that together let the shell prompt hook honour its
//! per-tick budget, survive daemon outages, and never write to interactive
//! stderr:
//!
//! - [`budget`] — monotonic per-tick deadline guard.
//! - [`queue`] — persistent per-flake JSONL queue for deferred work.
//! - [`refine`] — load + dedup + rewrite the queue between ticks.
//! - [`shortcut`] — env-driven fast path when already in a trusted devshell.
//! - [`offline`] — per-flake cache + reconnect backoff state.
//! - [`diag_log`] — rotating JSONL sink at `$XDG_STATE_HOME/xi/hook.log`.
//!
//! Wiring into `prompt.rs` is a follow-up task; this module ships the
//! primitives.

pub mod budget;
pub mod diag_log;
pub mod offline;
pub mod queue;
pub mod refine;
pub mod shortcut;
pub mod tick;

pub use budget::Budget;
pub use diag_log::DiagLog;
pub use offline::{BackoffState, OfflineCache};
pub use queue::{Queue, QueueEntry, QueuePayload};
pub use refine::refine_and_flush;
pub use shortcut::should_shortcut;
pub use tick::{DaemonCaller, TickCtx, TickOutcome, run_tick};

/// Test-only shared lock protecting the `XDG_STATE_HOME` process env var.
///
/// Every unit test that mutates that variable must acquire this lock first
/// so the parallel cargo-test runner does not cross-contaminate temp dirs
/// across modules.
#[cfg(test)]
pub(crate) mod test_env {
  use std::sync::Mutex;

  pub(crate) static XDG_LOCK: Mutex<()> = Mutex::new(());
}
