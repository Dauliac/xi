//! BDD: async-daemon-cqrs / per-tick prompt budget (MVP task 4.3).
//!
//! Exercises the client-side budget guarantee across the four scenarios
//! that appear in the daemon spec (`Client — prompt hook total budget`,
//! `Client — state-query timeout and offline failover`,
//! `Client — persistent prompt queue with refinement`):
//!
//! * **cold** — no daemon reachable (call returns instantly with an error),
//!   the tick must fall to the offline/silent path under budget.
//! * **warm** — daemon answers within the budget; live reply flows.
//! * **evaluating** — daemon returns `Pending` fast; still a live reply.
//! * **unreachable / slow** — the caller sleeps beyond the budget; the
//!   tick must bail (returning `None`) and enqueue for the next tick,
//!   still finishing under budget.
//!
//! Every scenario asserts a wall-clock ceiling on the tick with a
//! generous safety margin so the suite is stable on loaded CI.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use color_eyre::Result;
use tempfile::TempDir;
use xi_develop::daemon::protocol_v3::{Availability, Query, QueryReply};
use xi_develop::hook::{
  Budget, DaemonCaller, Queue, TickCtx, TickOutcome, run_tick,
};

/// A caller whose behaviour is scripted per test.
struct ScriptedCaller<F> {
  f:     F,
  calls: AtomicUsize,
}

impl<F: Fn(Duration) -> Result<Option<QueryReply>>> DaemonCaller
  for ScriptedCaller<F>
{
  fn call(
    &self,
    _query: Query,
    remaining: Duration,
  ) -> Result<Option<QueryReply>> {
    self.calls.fetch_add(1, Ordering::SeqCst);
    (self.f)(remaining)
  }
}

fn ceiling(scaled: Duration) -> Duration {
  // 3× headroom so a loaded runner doesn't false-fail.
  scaled.saturating_mul(3)
}

/// Serialises tests that mutate `XDG_STATE_HOME` (per-process env is
/// shared across cargo's parallel test threads).
static ENV_SERIAL: Mutex<()> = Mutex::new(());

struct TestSandbox {
  _lock: std::sync::MutexGuard<'static, ()>,
  _td:   TempDir,
  queue: Queue,
}

impl TestSandbox {
  fn new(flake_id: &str) -> Self {
    // Each `tests/*.rs` is its own binary, so we only need to serialise
    // against sibling tests in this file (unit tests in the lib run in a
    // separate process and can't race us).
    let lock = ENV_SERIAL
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let td = tempfile::tempdir().expect("mktemp");
    // SAFETY: guarded by ENV_SERIAL for the sandbox lifetime.
    unsafe {
      std::env::set_var("XDG_STATE_HOME", td.path());
    }
    let queue = Queue::open(flake_id).expect("queue");
    Self {
      _lock: lock,
      _td: td,
      queue,
    }
  }

  fn queue(&self) -> &Queue {
    &self.queue
  }
}

fn ctx<'a>(
  sandbox: &'a TestSandbox,
  budget: Budget,
  offline_valid: bool,
) -> TickCtx<'a> {
  TickCtx {
    cwd:            std::path::Path::new("/tmp"),
    last_lock_hash: None,
    last_reply_at:  None,
    offline_valid,
    queue:          sandbox.queue(),
    budget,
  }
}

/// **warm**: daemon answers instantly with `HeartBeat`. Live reply,
/// no enqueue, tick well under budget.
#[test]
fn warm_daemon_tick_under_budget() {
  let sb = TestSandbox::new("warm");
  let budget = Duration::from_millis(100);
  let caller = ScriptedCaller {
    f:     |_r| Ok(Some(QueryReply::HeartBeat)),
    calls: AtomicUsize::new(0),
  };

  let t0 = Instant::now();
  let out = run_tick(
    &ctx(&sb, Budget::start(budget), false),
    Query::HeartBeat,
    &caller,
  )
  .expect("tick");
  let elapsed = t0.elapsed();

  assert!(matches!(out, TickOutcome::Live(QueryReply::HeartBeat)));
  assert!(
    elapsed < ceiling(budget),
    "warm tick took {elapsed:?}, expected < {:?}",
    ceiling(budget)
  );
  assert_eq!(caller.calls.load(Ordering::SeqCst), 1);
}

/// **evaluating**: daemon replies fast with `Pending(job_id)`. The
/// prompt still learns "evaluation in flight" without blocking.
#[test]
fn evaluating_daemon_tick_returns_pending_under_budget() {
  let sb = TestSandbox::new("evaluating");
  let budget = Duration::from_millis(100);
  let caller = ScriptedCaller {
    f:     |_r| {
      Ok(Some(QueryReply::Devshell(Availability::Pending {
        job_id: "current-eval".into(),
      })))
    },
    calls: AtomicUsize::new(0),
  };

  let t0 = Instant::now();
  let out = run_tick(
    &ctx(&sb, Budget::start(budget), false),
    Query::Devshell,
    &caller,
  )
  .expect("tick");
  let elapsed = t0.elapsed();

  match out {
    TickOutcome::Live(QueryReply::Devshell(Availability::Pending { .. })) => {},
    other => unreachable!("expected Pending live reply, got {other:?}"),
  }
  assert!(elapsed < ceiling(budget));
}

/// **cold**: no daemon reachable (caller returns transport error). The
/// tick must enqueue for the next attempt and report either Offline (if
/// cache is valid) or Silent, still under budget.
#[test]
fn cold_no_daemon_tick_defers_and_offline_when_cache_valid() {
  let sb = TestSandbox::new("cold-offline");
  let budget = Duration::from_millis(100);
  let caller = ScriptedCaller {
    f:     |_r| Err(color_eyre::eyre::eyre!("connection refused")),
    calls: AtomicUsize::new(0),
  };

  let t0 = Instant::now();
  let out = run_tick(
    &ctx(&sb, Budget::start(budget), /* offline_valid = */ true),
    Query::HeartBeat,
    &caller,
  )
  .expect("tick");
  let elapsed = t0.elapsed();

  assert!(matches!(out, TickOutcome::Offline));
  assert!(elapsed < ceiling(budget));

  // Deferred entry landed on disk.
  let queued: Vec<_> = sb.queue().load().expect("load");
  assert_eq!(queued.len(), 1, "cold tick must enqueue exactly one entry");
}

#[test]
fn cold_no_daemon_no_cache_reports_silent_and_defers() {
  let sb = TestSandbox::new("cold-silent");
  let budget = Duration::from_millis(100);
  let caller = ScriptedCaller {
    f:     |_r| Err(color_eyre::eyre::eyre!("connection refused")),
    calls: AtomicUsize::new(0),
  };

  let out = run_tick(
    &ctx(&sb, Budget::start(budget), false),
    Query::HeartBeat,
    &caller,
  )
  .expect("tick");
  assert!(matches!(out, TickOutcome::Silent));
  assert_eq!(sb.queue().load().expect("load").len(), 1);
}

/// **unreachable / slow**: the caller sleeps past the budget then
/// answers. `run_tick` cannot cancel the mock, but the caller must
/// honour the remaining budget by returning `None` when it sees it's
/// out of time. The bdd_client_budget test verifies our contract:
/// callers that respect the remaining budget produce a deferred tick.
#[test]
fn slow_daemon_returning_none_defers_within_budget() {
  let sb = TestSandbox::new("slow");
  let budget = Duration::from_millis(100);

  // Caller inspects remaining and returns `None` once it's under
  // `state_query` (20 ms by default). Simulates the real client giving
  // up its socket read when the deadline approaches.
  let caller = ScriptedCaller {
    f:     |remaining| {
      // Simulate a short wait then decide.
      std::thread::sleep(Duration::from_millis(15));
      if remaining < Duration::from_millis(30) {
        Ok(None)
      } else {
        Ok(Some(QueryReply::HeartBeat))
      }
    },
    calls: AtomicUsize::new(0),
  };

  let t0 = Instant::now();
  let out = run_tick(
    &ctx(&sb, Budget::start(budget), true),
    Query::HeartBeat,
    &caller,
  )
  .expect("tick");
  let elapsed = t0.elapsed();

  // With 100 ms - 15 ms = 85 ms remaining, the caller answers Live.
  assert!(matches!(out, TickOutcome::Live(_)));
  assert!(elapsed < ceiling(budget));

  // Second tick: tighter budget so the caller returns None.
  let out2 = run_tick(
    &ctx(&sb, Budget::start(Duration::from_millis(25)), true),
    Query::HeartBeat,
    &caller,
  )
  .expect("tick");
  assert!(matches!(out2, TickOutcome::Offline));
  assert_eq!(sb.queue().load().expect("load").len(), 1);
}

/// A tick that starts with an already-exhausted budget bails immediately.
/// The daemon caller must not be invoked; the shortcut+defer path runs.
#[test]
fn exhausted_budget_skips_daemon_and_defers() {
  let sb = TestSandbox::new("exhausted");
  let caller = ScriptedCaller {
    f:     |_r| panic!("caller must not be invoked"),
    calls: AtomicUsize::new(0),
  };
  let out = run_tick(
    &ctx(&sb, Budget::start(Duration::ZERO), true),
    Query::HeartBeat,
    &caller,
  )
  .expect("tick");
  assert!(matches!(out, TickOutcome::Offline));
  assert_eq!(caller.calls.load(Ordering::SeqCst), 0);
  assert_eq!(sb.queue().load().expect("load").len(), 1);
}

/// A run of six identical failing ticks appends six entries; the queue
/// then refines them down to one (dedup queries → most recent) on the
/// next `refine_and_flush` call. Verifies the "duplicate queries
/// collapse" spec clause under repeated tick failures.
#[test]
fn repeated_failing_ticks_collapse_on_refine() {
  let sb = TestSandbox::new("refine");
  let budget = Duration::from_millis(100);
  let caller = ScriptedCaller {
    f:     |_r| Err(color_eyre::eyre::eyre!("no daemon")),
    calls: AtomicUsize::new(0),
  };

  for _ in 0..6 {
    let _ = run_tick(
      &ctx(&sb, Budget::start(budget), true),
      Query::HeartBeat,
      &caller,
    );
  }
  assert_eq!(sb.queue().load().expect("load").len(), 6);

  let refined =
    xi_develop::hook::refine_and_flush(sb.queue()).expect("refine");
  assert_eq!(
    refined.len(),
    1,
    "six identical HeartBeat queries must collapse to one"
  );
}

// Suppress `PathBuf` unused-import lint on nightly rustc runs.
fn _keep_paths(_p: PathBuf) {}
