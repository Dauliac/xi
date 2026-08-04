//! Single-writer state machine for the devshell daemon.
//!
//! Every mutation to [`DaemonState`] flows through [`StateMachine::transition`].
//! Illegal transitions are rejected explicitly with [`TransitionRejected`] — never
//! silently ignored — so upstream logic can log or alert.
//!
//! The current snapshot is published lock-free via
//! [`arc_swap::ArcSwap<StateSnapshot>`]. Reader threads (e.g. the state-responder
//! from task 2.1) clone the [`StateHandle`] once at startup and call `.load()` on
//! every read to get the latest snapshot in one atomic op — they never contend
//! with the writer.
//!
//! # Design invariants
//!
//! * Single writer: only [`StateMachine::transition`] mutates state.
//! * `seq` is monotonic and increases **only** on accepted transitions.
//! * `emitted_at` is stamped at accept time and captures the moment the snapshot
//!   became visible to readers.
//! * Rejected transitions do **not** publish a new snapshot and do **not** bump
//!   `seq` — they return a typed error the caller must handle.
//!
//! [`StateSnapshot`] is intentionally minimal in this task (state + seq +
//! emitted_at). Sibling tasks will extend it with `current_env`, `pending_env`,
//! `active_jobs`, `subscribers`, etc. — extend as sibling tasks wire in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use arc_swap::ArcSwap;

use super::protocol::DaemonState;

/// Job identifier — the state machine only carries the id opaquely; the job
/// registry (task 2.x) owns the details.
pub type JobId = u64;

/// Immutable snapshot of the daemon's current state.
///
/// Publishing works by swapping a fresh `Arc<StateSnapshot>` into the
/// [`StateHandle`]. Readers get a wait-free view of a consistent past state.
///
/// Extend as sibling tasks wire in (`current_env`, `pending_env`, `active_jobs`,
/// `subscribers`, …).
#[derive(Debug, Clone)]
pub struct StateSnapshot {
  /// The current daemon state.
  pub state: DaemonState,
  /// Monotonic sequence number — bumped once per accepted transition.
  pub seq: u64,
  /// Wall-clock time the snapshot became visible to readers.
  pub emitted_at: SystemTime,
}

impl StateSnapshot {
  /// Build the initial snapshot (seq=0, state=Starting, emitted_at=now).
  #[must_use]
  pub fn initial() -> Self {
    Self {
      state: DaemonState::Starting,
      seq: 0,
      emitted_at: SystemTime::now(),
    }
  }
}

/// Shared, lock-free handle to the current [`StateSnapshot`].
///
/// Readers clone the [`Arc`] once and call `.load()` on every read.
pub type StateHandle = Arc<ArcSwap<StateSnapshot>>;

/// A typed event that *may* drive a state transition.
///
/// Variants intentionally carry the payload the machine needs to publish the
/// resulting snapshot (e.g. `EvalFailed` carries the error string that becomes
/// part of `DaemonState::BuildFailed`).
#[derive(Debug, Clone)]
pub enum TransitionEvent {
  /// Kick off a new evaluation.
  ///
  /// Accepted from `Starting`, `Ready`, `BuildFailed`, `WatcherDegraded` — i.e.
  /// any settled state that is *not* mid-eval, mid-shutdown, or in a config
  /// error.
  EvalStarted,
  /// Evaluation produced a usable env file.
  EvalSucceeded {
    /// Path to the emitted env file.
    env: PathBuf,
  },
  /// Evaluation failed with a captured error.
  EvalFailed {
    /// Human-readable error rendered in `BuildFailed`.
    error: String,
    /// Retry count carried by `BuildFailed`.
    retry_count: u32,
  },
  /// The filesystem watcher lost fidelity (fell back to polling, etc.).
  WatcherDegraded,
  /// Operator or supervisor requested a restart: go back to `Starting`.
  ///
  /// Accepted from any live state (not `ShuttingDown`).
  Restart,
  /// Requested a graceful shutdown. Terminal.
  Shutdown,
  /// A job has been stuck longer than the configured threshold (task 1.5).
  ///
  /// The state machine surfaces this by transitioning to a `BuildFailed`-shaped
  /// state — sibling tasks introduce a dedicated `Stuck` variant, at which
  /// point we swap the target here. Until then we reject if the current state
  /// doesn't have a home for `Stuck`.
  Stuck {
    /// The stuck job.
    job_id: JobId,
    /// Time in milliseconds the job has been stuck.
    stuck_ms: u64,
  },
  /// Recover from a degraded / failed state back to `Evaluating`.
  SelfHeal,
  /// A configuration file failed to parse / validate.
  ConfigError {
    /// Human-readable error rendered in `ConfigError`.
    error: String,
  },
}

impl TransitionEvent {
  /// Static tag used in [`TransitionRejected::event_kind`] — cheap to log.
  #[must_use]
  pub const fn kind(&self) -> &'static str {
    match self {
      Self::EvalStarted => "EvalStarted",
      Self::EvalSucceeded { .. } => "EvalSucceeded",
      Self::EvalFailed { .. } => "EvalFailed",
      Self::WatcherDegraded => "WatcherDegraded",
      Self::Restart => "Restart",
      Self::Shutdown => "Shutdown",
      Self::Stuck { .. } => "Stuck",
      Self::SelfHeal => "SelfHeal",
      Self::ConfigError { .. } => "ConfigError",
    }
  }
}

pub use super::state_meta::{DaemonStateKind, kind_of};

/// Explicit rejection type for an illegal transition.
///
/// Emitted only when the machine refuses to move — never on success. Log it,
/// do not swallow it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
  "illegal transition: {event_kind} from {from:?} rejected ({reason})"
)]
pub struct TransitionRejected {
  /// Coarse kind of the state the machine was in when the event arrived.
  pub from: DaemonStateKind,
  /// Static tag of the offending event (see [`TransitionEvent::kind`]).
  pub event_kind: &'static str,
  /// Static, human-readable reason for the rejection.
  pub reason: &'static str,
}

/// Single-writer state machine.
///
/// Owns the current [`StateSnapshot`] and the [`StateHandle`] readers subscribe
/// to. Only [`Self::transition`] mutates.
#[derive(Debug)]
pub struct StateMachine {
  handle: StateHandle,
  /// The last accepted seq — writer-local counter, cheap to bump.
  seq: u64,
}

impl StateMachine {
  /// Build a fresh machine at [`StateSnapshot::initial`].
  #[must_use]
  pub fn new() -> Self {
    let snapshot = StateSnapshot::initial();
    let seq = snapshot.seq;
    Self {
      handle: Arc::new(ArcSwap::from_pointee(snapshot)),
      seq,
    }
  }

  /// Clone the reader handle. Readers `.load()` this to observe transitions.
  #[must_use]
  pub fn handle(&self) -> StateHandle {
    Arc::clone(&self.handle)
  }

  /// Currently-published snapshot (writer-side, cheap read).
  #[must_use]
  pub fn snapshot(&self) -> Arc<StateSnapshot> {
    self.handle.load_full()
  }

  /// Apply an event.
  ///
  /// On success, publishes a fresh snapshot with `seq = old.seq + 1` and
  /// returns a clone. On failure, publishes nothing, does not bump `seq`, and
  /// returns [`TransitionRejected`] with the reason.
  ///
  /// # Errors
  /// Returns [`TransitionRejected`] when the event is not legal from the
  /// current state.
  pub fn transition(
    &mut self,
    event: TransitionEvent,
  ) -> Result<StateSnapshot, TransitionRejected> {
    let current = self.handle.load();
    let from_kind = kind_of(&current.state);
    let next = Self::next_state(&current.state, &event).ok_or_else(|| {
      TransitionRejected {
        from: from_kind,
        event_kind: event.kind(),
        reason: reason_for(&current.state, &event),
      }
    })?;

    self.seq = self.seq.saturating_add(1);
    let snapshot = StateSnapshot {
      state: next,
      seq: self.seq,
      emitted_at: SystemTime::now(),
    };
    self.handle.store(Arc::new(snapshot.clone()));
    Ok(snapshot)
  }

  /// Pure transition function. Returns `None` when the transition is illegal.
  ///
  /// Kept `pub(crate)` and pure so the routing layer (task 2.4) can unit-test
  /// legality without spinning up a full machine.
  #[allow(clippy::needless_pass_by_value)] // future-proof: payloads move soon
  fn next_state(
    current: &DaemonState,
    event: &TransitionEvent,
  ) -> Option<DaemonState> {
    use DaemonState as S;
    use TransitionEvent as E;

    // Shutdown is terminal — nothing escapes it.
    if matches!(current, S::ShuttingDown) {
      return None;
    }

    // Shutdown is accepted from every non-shutdown state.
    if matches!(event, E::Shutdown) {
      return Some(S::ShuttingDown);
    }

    // Restart is accepted from every non-shutdown, non-starting state.
    if matches!(event, E::Restart) && !matches!(current, S::Starting) {
      return Some(S::Starting);
    }

    match (current, event) {
      // Starting → Evaluating
      (S::Starting, E::EvalStarted) => Some(S::Evaluating),

      // Evaluating → Ready | BuildFailed | ConfigError
      (S::Evaluating, E::EvalSucceeded { .. }) => Some(S::Ready),
      (
        S::Evaluating,
        E::EvalFailed {
          error,
          retry_count,
        },
      ) => Some(S::BuildFailed {
        error: error.clone(),
        retry_count: *retry_count,
      }),
      (S::Evaluating, E::ConfigError { error }) => Some(S::ConfigError {
        error: error.clone(),
      }),
      (S::Evaluating, E::WatcherDegraded) => Some(S::WatcherDegraded),

      // Ready → Evaluating (reload) | WatcherDegraded | ConfigError
      (S::Ready, E::EvalStarted) => Some(S::Evaluating),
      (S::Ready, E::WatcherDegraded) => Some(S::WatcherDegraded),
      (S::Ready, E::ConfigError { error }) => Some(S::ConfigError {
        error: error.clone(),
      }),

      // BuildFailed → Evaluating (retry) | ConfigError
      (S::BuildFailed { .. }, E::EvalStarted) => Some(S::Evaluating),
      (S::BuildFailed { .. }, E::SelfHeal) => Some(S::Evaluating),
      (S::BuildFailed { .. }, E::ConfigError { error }) => {
        Some(S::ConfigError {
          error: error.clone(),
        })
      }

      // WatcherDegraded → Evaluating (reload) | ConfigError | SelfHeal → Ready
      (S::WatcherDegraded, E::EvalStarted) => Some(S::Evaluating),
      (S::WatcherDegraded, E::SelfHeal) => Some(S::Ready),
      (S::WatcherDegraded, E::ConfigError { error }) => {
        Some(S::ConfigError {
          error: error.clone(),
        })
      }

      // ConfigError → Starting only via Restart (handled above);
      // SelfHeal explicitly tries to recover by re-evaluating.
      (S::ConfigError { .. }, E::SelfHeal) => Some(S::Evaluating),

      // Everything else is illegal.
      _ => None,
    }
  }
}

impl Default for StateMachine {
  fn default() -> Self {
    Self::new()
  }
}

/// Best-effort static reason string for logging a rejection.
///
/// Kept as a free function so it stays pure and testable.
const fn reason_for(
  current: &DaemonState,
  event: &TransitionEvent,
) -> &'static str {
  use DaemonState as S;
  use TransitionEvent as E;

  match (current, event) {
    (S::ShuttingDown, _) => "daemon is shutting down; no further transitions",
    (S::Starting, E::Restart) => "already starting",
    (S::Starting, E::EvalSucceeded { .. } | E::EvalFailed { .. }) => {
      "must EvalStarted before eval can succeed or fail"
    }
    (S::Ready, E::EvalSucceeded { .. } | E::EvalFailed { .. }) => {
      "no eval in flight from Ready"
    }
    (S::BuildFailed { .. }, E::EvalSucceeded { .. } | E::EvalFailed { .. }) => {
      "no eval in flight from BuildFailed"
    }
    (_, E::Stuck { .. }) => {
      "no target state for Stuck yet — sibling task adds the variant"
    }
    (S::ConfigError { .. }, _) => {
      "config must be fixed and daemon restarted before other events apply"
    }
    _ => "no transition defined for this (state, event) pair",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn assert_state(actual: &DaemonState, expected: &DaemonState) {
    assert_eq!(
      kind_of(actual),
      kind_of(expected),
      "expected {expected:?}, got {actual:?}"
    );
  }

  #[test]
  fn initial_state_is_starting_at_seq_zero() {
    let sm = StateMachine::new();
    let snap = sm.snapshot();
    assert_state(&snap.state, &DaemonState::Starting);
    assert_eq!(snap.seq, 0);
  }

  #[test]
  fn legal_path_starting_evaluating_ready() {
    let mut sm = StateMachine::new();

    let s1 = sm
      .transition(TransitionEvent::EvalStarted)
      .expect("Starting + EvalStarted must be legal");
    assert_state(&s1.state, &DaemonState::Evaluating);
    assert_eq!(s1.seq, 1);

    let s2 = sm
      .transition(TransitionEvent::EvalSucceeded {
        env: PathBuf::from("/nix/store/fake-env"),
      })
      .expect("Evaluating + EvalSucceeded must be legal");
    assert_state(&s2.state, &DaemonState::Ready);
    assert_eq!(s2.seq, 2);
  }

  #[test]
  fn illegal_ready_to_starting_via_eval_succeeded_is_rejected() {
    // Ready cannot receive EvalSucceeded — no eval is in flight.
    let mut sm = StateMachine::new();
    sm.transition(TransitionEvent::EvalStarted)
      .expect("first EvalStarted must be legal");
    sm.transition(TransitionEvent::EvalSucceeded {
      env: PathBuf::from("/nix/store/fake-env"),
    })
    .expect("EvalSucceeded from Evaluating must be legal");

    let err = sm
      .transition(TransitionEvent::EvalSucceeded {
        env: PathBuf::from("/nix/store/fake-env-2"),
      })
      .expect_err("Ready + EvalSucceeded must be rejected");

    assert_eq!(err.from, DaemonStateKind::Ready);
    assert_eq!(err.event_kind, "EvalSucceeded");
    assert_eq!(err.reason, "no eval in flight from Ready");
  }

  #[test]
  fn illegal_starting_to_ready_is_rejected() {
    // Directly Starting → Ready via EvalSucceeded (skipping Evaluating) illegal.
    let mut sm = StateMachine::new();
    let err = sm
      .transition(TransitionEvent::EvalSucceeded {
        env: PathBuf::from("/nix/store/fake-env"),
      })
      .expect_err("Starting + EvalSucceeded must be rejected");

    assert_eq!(err.from, DaemonStateKind::Starting);
    assert_eq!(err.event_kind, "EvalSucceeded");
  }

  #[test]
  fn seq_increases_on_accept_only() {
    let mut sm = StateMachine::new();
    assert_eq!(sm.snapshot().seq, 0);

    sm.transition(TransitionEvent::EvalStarted).expect("accept");
    assert_eq!(sm.snapshot().seq, 1);

    // Rejected — seq must NOT increase.
    let _ = sm
      .transition(TransitionEvent::EvalStarted)
      .expect_err("Evaluating + EvalStarted illegal");
    assert_eq!(sm.snapshot().seq, 1, "seq must not bump on rejection");

    sm.transition(TransitionEvent::EvalSucceeded {
      env: PathBuf::from("/nix/store/fake-env"),
    })
    .expect("accept");
    assert_eq!(sm.snapshot().seq, 2);
  }

  #[test]
  fn arc_swap_publish_visible_to_cloned_handle() {
    let mut sm = StateMachine::new();
    let reader = sm.handle();

    // Reader observes initial state.
    let observed = reader.load();
    assert_state(&observed.state, &DaemonState::Starting);
    assert_eq!(observed.seq, 0);
    drop(observed);

    // Writer transitions.
    sm.transition(TransitionEvent::EvalStarted)
      .expect("accept EvalStarted");

    // Same reader handle observes the new snapshot without any signalling.
    let observed = reader.load();
    assert_state(&observed.state, &DaemonState::Evaluating);
    assert_eq!(observed.seq, 1);
  }

  #[test]
  fn shutdown_is_terminal() {
    let mut sm = StateMachine::new();
    sm.transition(TransitionEvent::Shutdown)
      .expect("Shutdown from Starting must be legal");

    for ev in [
      TransitionEvent::EvalStarted,
      TransitionEvent::Restart,
      TransitionEvent::SelfHeal,
      TransitionEvent::WatcherDegraded,
    ] {
      let err = sm
        .transition(ev.clone())
        .expect_err("ShuttingDown must reject every event");
      assert_eq!(err.from, DaemonStateKind::ShuttingDown);
    }
  }

  #[test]
  fn restart_from_any_live_state_goes_to_starting() {
    // Ready → Restart → Starting.
    let mut sm = StateMachine::new();
    sm.transition(TransitionEvent::EvalStarted).expect("accept");
    sm.transition(TransitionEvent::EvalSucceeded {
      env: PathBuf::from("/nix/store/fake-env"),
    })
    .expect("accept");

    let s = sm.transition(TransitionEvent::Restart).expect("accept");
    assert_state(&s.state, &DaemonState::Starting);

    // Starting + Restart is rejected — already there.
    let err = sm
      .transition(TransitionEvent::Restart)
      .expect_err("Starting + Restart illegal");
    assert_eq!(err.reason, "already starting");
  }

  #[test]
  fn self_heal_from_watcher_degraded_returns_to_ready() {
    let mut sm = StateMachine::new();
    sm.transition(TransitionEvent::EvalStarted).expect("accept");
    sm.transition(TransitionEvent::WatcherDegraded)
      .expect("Evaluating + WatcherDegraded must be legal");

    let s = sm.transition(TransitionEvent::SelfHeal).expect("accept");
    assert_state(&s.state, &DaemonState::Ready);
  }
}
