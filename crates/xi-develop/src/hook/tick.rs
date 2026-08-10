//! One prompt tick — composes the hook primitives into a bounded operation.
//!
//! The shell prompt hook fires on every prompt keystroke. Each firing MUST
//! finish inside `Timeouts::prompt_budget` (default 100 ms) regardless of
//! daemon health. This module orchestrates the pieces `hook::` already
//! ships:
//!
//! 1. **Shortcut check** — if `should_shortcut` says the shell is still in
//!    a trusted subshell with a matching lock hash, return
//!    [`TickOutcome::Shortcut`] with no I/O.
//! 2. **Daemon call** — hand the [`Query`] to the injected
//!    [`DaemonCaller`], respecting the remaining budget.
//! 3. **Fallback** — on daemon error or budget exhaustion, enqueue the
//!    query for the next tick and return [`TickOutcome::Deferred`] (or
//!    [`TickOutcome::Offline`] if the cache is still valid).
//!
//! `prompt.rs` plugs a real socket-backed caller in; the BDD tests
//! plug a mock in and drive every branch deterministically. Wiring
//! `prompt.rs` to route through this module is a separate task — the
//! interactive migration is intentionally decoupled from the
//! testability of the orchestrator.

use std::path::Path;
use std::time::Duration;

use color_eyre::Result;

use super::budget::Budget;
use super::queue::{Queue, QueueEntry, QueuePayload};
use super::shortcut::should_shortcut;
use crate::daemon::protocol_v3::{Query, QueryReply};

/// One-shot daemon call surface used by [`run_tick`].
///
/// The production impl wraps `daemon::client_v3::query`; tests supply a
/// deterministic mock that simulates cold/warm/evaluating/unreachable
/// scenarios without touching a real socket.
pub trait DaemonCaller {
  /// Send a query, honouring the remaining budget. Returning `Ok(None)`
  /// means "budget exhausted mid-call"; that's distinct from
  /// [`Err`] which means "transport failed".
  ///
  /// # Errors
  ///
  /// Any transport-level failure (socket missing, EOF, decode error).
  fn call(
    &self,
    query: Query,
    remaining: Duration,
  ) -> Result<Option<QueryReply>>;
}

/// Every terminal state of one tick. Callers render or persist based on
/// which variant they get.
#[derive(Debug)]
pub enum TickOutcome {
  /// Env-based shortcut succeeded; no daemon call was made.
  Shortcut,
  /// Daemon answered inside the budget with a [`QueryReply`].
  Live(QueryReply),
  /// Daemon call didn't finish in time; the request was enqueued for a
  /// later tick.
  Deferred,
  /// Daemon transport failed but the offline cache is still trustworthy;
  /// caller renders from the cache and enqueues the request.
  Offline,
  /// Neither daemon nor cache produced an answer; nothing to render.
  ///
  /// The queue still holds the request so the next tick retries.
  Silent,
}

/// Context passed into [`run_tick`] — the caller assembles this once per
/// prompt and re-uses the [`Queue`] handle across ticks.
pub struct TickCtx<'a> {
  pub cwd: &'a Path,
  pub last_lock_hash: Option<&'a str>,
  pub last_reply_at: Option<std::time::SystemTime>,
  pub offline_valid: bool,
  pub queue: &'a Queue,
  pub budget: Budget,
}

/// Run one tick.
///
/// See module docs for the full state chart. Never blocks for longer
/// than `ctx.budget`.
///
/// # Errors
///
/// Returns an error only if the queue append fails on the fallback path.
/// Every other failure is folded into [`TickOutcome`] variants.
pub fn run_tick<C: DaemonCaller>(
  ctx: &TickCtx<'_>,
  query: Query,
  caller: &C,
) -> Result<TickOutcome> {
  if should_shortcut(ctx.cwd, ctx.last_lock_hash, ctx.last_reply_at) {
    return Ok(TickOutcome::Shortcut);
  }

  if ctx.budget.exceeded() {
    defer(ctx.queue, query)?;
    return Ok(fallback_outcome(ctx));
  }

  if let Ok(Some(reply)) = caller.call(query.clone(), ctx.budget.remaining()) {
    Ok(TickOutcome::Live(reply))
  } else {
    defer(ctx.queue, query)?;
    Ok(fallback_outcome(ctx))
  }
}

const fn fallback_outcome(ctx: &TickCtx<'_>) -> TickOutcome {
  if ctx.offline_valid {
    TickOutcome::Offline
  } else {
    TickOutcome::Silent
  }
}

fn defer(queue: &Queue, query: Query) -> Result<()> {
  queue.append(&QueueEntry::new(QueuePayload::Query(query)))
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;

  use super::*;

  struct MockCaller<F: Fn(Duration) -> Result<Option<QueryReply>>>(F);

  impl<F: Fn(Duration) -> Result<Option<QueryReply>>> DaemonCaller
    for MockCaller<F>
  {
    fn call(
      &self,
      _query: Query,
      remaining: Duration,
    ) -> Result<Option<QueryReply>> {
      (self.0)(remaining)
    }
  }

  fn make_queue() -> (Queue, tempfile::TempDir) {
    let _lock = crate::hook::test_env::XDG_LOCK
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let td = tempfile::tempdir().expect("mktemp");
    // SAFETY: guarded by hook::test_env::XDG_LOCK for the duration.
    unsafe {
      std::env::set_var("XDG_STATE_HOME", td.path());
    }
    let q = Queue::open("test-flake").expect("queue");
    // Guard drops with td — return both to keep them alive.
    drop(_lock);
    (q, td)
  }

  fn make_ctx<'a>(
    cwd: &'a Path,
    queue: &'a Queue,
    budget: Budget,
    offline_valid: bool,
  ) -> TickCtx<'a> {
    TickCtx {
      cwd,
      last_lock_hash: None,
      last_reply_at: None,
      offline_valid,
      queue,
      budget,
    }
  }

  #[test]
  fn warm_call_returns_live_reply_within_budget() {
    let (q, _td) = make_queue();
    let calls = Cell::new(0);
    let caller = MockCaller(|_r| {
      calls.set(calls.get() + 1);
      Ok(Some(QueryReply::HeartBeat))
    });
    let ctx = make_ctx(
      Path::new("/tmp"),
      &q,
      Budget::start(Duration::from_secs(1)),
      false,
    );
    let out = run_tick(&ctx, Query::HeartBeat, &caller).expect("tick");
    assert!(matches!(out, TickOutcome::Live(QueryReply::HeartBeat)));
    assert_eq!(calls.get(), 1);
    assert_eq!(
      q.load().expect("load").len(),
      0,
      "warm path enqueues nothing"
    );
  }

  #[test]
  fn exhausted_budget_defers_and_reports_silent_without_cache() {
    let (q, _td) = make_queue();
    let caller = MockCaller(|_r| {
      panic!("caller must not run when budget is already exceeded")
    });
    let ctx =
      make_ctx(Path::new("/tmp"), &q, Budget::start(Duration::ZERO), false);
    let out = run_tick(&ctx, Query::HeartBeat, &caller).expect("tick");
    assert!(matches!(out, TickOutcome::Silent));
    assert_eq!(
      q.load().expect("load").len(),
      1,
      "budget-exhausted enqueues"
    );
  }

  #[test]
  fn exhausted_budget_defers_and_reports_offline_when_cache_valid() {
    let (q, _td) = make_queue();
    let caller = MockCaller(|_r| unreachable!());
    let ctx =
      make_ctx(Path::new("/tmp"), &q, Budget::start(Duration::ZERO), true);
    let out = run_tick(&ctx, Query::HeartBeat, &caller).expect("tick");
    assert!(matches!(out, TickOutcome::Offline));
    assert_eq!(q.load().expect("load").len(), 1);
  }

  #[test]
  fn daemon_error_defers_and_falls_back_to_offline() {
    let (q, _td) = make_queue();
    let caller =
      MockCaller(|_r| Err(color_eyre::eyre::eyre!("connection refused")));
    let ctx = make_ctx(
      Path::new("/tmp"),
      &q,
      Budget::start(Duration::from_secs(1)),
      true,
    );
    let out = run_tick(&ctx, Query::HeartBeat, &caller).expect("tick");
    assert!(matches!(out, TickOutcome::Offline));
    assert_eq!(q.load().expect("load").len(), 1, "error path enqueues");
  }

  #[test]
  fn evaluating_daemon_returns_pending_reply_live() {
    use crate::daemon::protocol_v3::Availability;
    let (q, _td) = make_queue();
    let caller = MockCaller(|_r| {
      Ok(Some(QueryReply::Devshell(Availability::Pending {
        job_id: "current-eval".into(),
      })))
    });
    let ctx = make_ctx(
      Path::new("/tmp"),
      &q,
      Budget::start(Duration::from_secs(1)),
      false,
    );
    let out = run_tick(&ctx, Query::Devshell, &caller).expect("tick");
    // Pending is still `Live` — the reply arrived inside the budget.
    // Callers distinguish Pending from Ready at the render layer.
    assert!(matches!(out, TickOutcome::Live(QueryReply::Devshell(_))));
    assert_eq!(q.load().expect("load").len(), 0);
  }

  #[test]
  fn caller_none_defers_without_calling_daemon_error() {
    let (q, _td) = make_queue();
    let caller = MockCaller(|_r| Ok(None));
    let ctx = make_ctx(
      Path::new("/tmp"),
      &q,
      Budget::start(Duration::from_secs(1)),
      false,
    );
    let out = run_tick(&ctx, Query::HeartBeat, &caller).expect("tick");
    assert!(matches!(out, TickOutcome::Silent));
    assert_eq!(q.load().expect("load").len(), 1);
  }
}
