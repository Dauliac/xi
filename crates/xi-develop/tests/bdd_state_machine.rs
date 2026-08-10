//! BDD: async-daemon-cqrs / single-writer state machine (MVP task 4.1).
//!
//! Exercises the invariants that the daemon spec `Requirement: Daemon —
//! single-writer state machine` demands:
//!
//! 1. All transitions go through one writer; readers observe a lock-free
//!    monotonic snapshot sequence.
//! 2. Illegal transitions are rejected explicitly (`TransitionRejected`),
//!    do not publish a new snapshot, and do not bump `seq`.
//! 3. A concurrent reader thread never observes torn state — snapshots
//!    are wait-free via `arc_swap::ArcSwap<StateSnapshot>`.

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use xi_develop::daemon::protocol::DaemonState;
use xi_develop::daemon::state_machine::{
  DaemonStateKind, StateMachine, TransitionEvent, kind_of,
};

/// A newly-constructed machine publishes `Starting` at seq=0.
#[test]
fn initial_snapshot_is_starting_at_seq_zero() {
  let sm = StateMachine::new();
  let snap = sm.snapshot();
  assert_eq!(kind_of(&snap.state), DaemonStateKind::Starting);
  assert_eq!(snap.seq, 0);
}

/// Every accepted transition increments `seq` by exactly one.
#[test]
fn seq_increments_monotonically_across_legal_path() {
  let mut sm = StateMachine::new();
  assert_eq!(sm.snapshot().seq, 0);

  sm.transition(&TransitionEvent::EvalStarted).expect("legal");
  assert_eq!(sm.snapshot().seq, 1);

  sm.transition(&TransitionEvent::EvalSucceeded {
    env: PathBuf::from("/nix/store/fake-env"),
  })
  .expect("legal");
  assert_eq!(sm.snapshot().seq, 2);

  sm.transition(&TransitionEvent::EvalStarted).expect("legal");
  assert_eq!(sm.snapshot().seq, 3);
}

/// An illegal transition is rejected explicitly and the snapshot is
/// unchanged — no seq bump, no state change.
#[test]
fn illegal_transition_rejected_and_snapshot_unchanged() {
  let mut sm = StateMachine::new();
  // Starting + EvalSucceeded is illegal (must EvalStarted first).
  let before = sm.snapshot();
  let rejection = sm
    .transition(&TransitionEvent::EvalSucceeded {
      env: PathBuf::from("/nix/store/fake"),
    })
    .expect_err("must be rejected");

  assert_eq!(rejection.from, DaemonStateKind::Starting);
  assert_eq!(rejection.event_kind, "EvalSucceeded");

  let after = sm.snapshot();
  assert_eq!(after.seq, before.seq, "seq must not bump on reject");
  assert_eq!(
    kind_of(&after.state),
    kind_of(&before.state),
    "state must not change on reject"
  );
}

/// The `Shutdown` state is terminal — no further transitions are accepted.
#[test]
fn shutting_down_is_terminal() {
  let mut sm = StateMachine::new();
  sm.transition(&TransitionEvent::Shutdown).expect("legal");
  assert_eq!(kind_of(&sm.snapshot().state), DaemonStateKind::ShuttingDown);

  let seq_before = sm.snapshot().seq;
  for event in [
    TransitionEvent::EvalStarted,
    TransitionEvent::Restart,
    TransitionEvent::SelfHeal,
    TransitionEvent::WatcherDegraded,
  ] {
    let err = sm.transition(&event).expect_err("shutdown is terminal");
    assert_eq!(err.from, DaemonStateKind::ShuttingDown);
  }
  assert_eq!(sm.snapshot().seq, seq_before, "no seq bump post-shutdown");
}

/// `BuildFailed` preserves the last error and retry count on the snapshot.
#[test]
fn build_failed_carries_error_and_retry_count() {
  let mut sm = StateMachine::new();
  sm.transition(&TransitionEvent::EvalStarted).expect("legal");
  let snap = sm
    .transition(&TransitionEvent::EvalFailed {
      error: "flake syntax error".into(),
      retry_count: 3,
    })
    .expect("legal");
  let DaemonState::BuildFailed { error, retry_count } = snap.state else {
    unreachable!("expected BuildFailed after failed eval");
  };
  assert_eq!(error, "flake syntax error");
  assert_eq!(retry_count, 3);
}

/// Concurrent readers observe monotonically-increasing snapshots without
/// blocking or seeing a torn state. Verifies the lock-free
/// `arc_swap::ArcSwap<StateSnapshot>` publication guarantee.
#[test]
fn readers_observe_monotonic_snapshots_under_concurrent_writes() {
  let mut sm = StateMachine::new();
  let handle = sm.handle();

  let reader_count = 4;
  let write_count = 500;
  let barrier = Arc::new(Barrier::new(reader_count + 1));

  let readers: Vec<_> = (0..reader_count)
    .map(|_| {
      let h = Arc::clone(&handle);
      let b = Arc::clone(&barrier);
      thread::spawn(move || {
        b.wait();
        let mut last_seq = 0u64;
        let mut observed = 0usize;
        while observed < write_count {
          let snap = h.load();
          assert!(
            snap.seq >= last_seq,
            "reader saw seq go backwards: {} < {}",
            snap.seq,
            last_seq
          );
          last_seq = snap.seq;
          if last_seq >= write_count as u64 {
            observed = write_count;
          } else {
            observed = observed.saturating_add(1);
          }
        }
        last_seq
      })
    })
    .collect();

  barrier.wait();
  // Toggle between Evaluating and Ready to bump seq deterministically.
  for i in 0..write_count {
    if i % 2 == 0 {
      sm.transition(&TransitionEvent::EvalStarted).expect("legal");
    } else {
      sm.transition(&TransitionEvent::EvalSucceeded {
        env: PathBuf::from(format!("/nix/store/e-{i}")),
      })
      .expect("legal");
    }
  }

  for r in readers {
    let last = r.join().expect("reader thread");
    assert!(last >= 1, "reader observed at least one transition");
  }
  assert_eq!(sm.snapshot().seq, write_count as u64);
}

/// A restart moves any live state back to `Starting` and still bumps seq.
#[test]
fn restart_from_ready_returns_to_starting() {
  let mut sm = StateMachine::new();
  sm.transition(&TransitionEvent::EvalStarted).expect("legal");
  sm.transition(&TransitionEvent::EvalSucceeded {
    env: PathBuf::from("/x"),
  })
  .expect("legal");
  let before = sm.snapshot().seq;

  let after = sm.transition(&TransitionEvent::Restart).expect("legal");
  assert_eq!(kind_of(&after.state), DaemonStateKind::Starting);
  assert_eq!(after.seq, before + 1);
}

/// `Restart` from `Starting` is a no-op (already starting) — rejected
/// explicitly rather than silently bumped.
#[test]
fn restart_from_starting_is_rejected() {
  let mut sm = StateMachine::new();
  let err = sm
    .transition(&TransitionEvent::Restart)
    .expect_err("already starting");
  assert_eq!(err.from, DaemonStateKind::Starting);
  assert_eq!(err.event_kind, "Restart");
}
