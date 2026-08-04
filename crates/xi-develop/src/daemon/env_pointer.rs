//! Atomic pointer pair (`current_env` / `pending_env`) preserving the last-good
//! devshell env across `BuildFailed` transitions.
//!
//! ## Contract
//!
//! - `current_env` is the last **successfully evaluated** devshell env. Interactive
//!   shells that have already sourced this env must never observe it disappear
//!   or change while an in-flight re-evaluation fails.
//! - `pending_env` is the in-flight eval's env — populated by
//!   [`EnvPointer::begin_eval`], promoted to `current` by
//!   [`EnvPointer::commit_eval`], or discarded by [`EnvPointer::abandon_eval`]
//!   (on `BuildFailed` / `AbortJob` / `ConfigError`).
//!
//! ## Storage shape
//!
//! One [`ArcSwap<EnvState>`] holding both pointers, rather than two
//! independent `ArcSwap<Option<EnvSnapshot>>`. Rationale: `commit_eval`
//! (promote `pending` → `current`, clear `pending`) is one atomic swap — a
//! reader cannot observe an intermediate torn state where `current` is stale
//! but `pending` is already cleared (or vice versa). The trade-off is one
//! extra `Arc` allocation per transition, which is negligible compared to a
//! Nix eval.
//!
//! ## SC-014 guarantee
//!
//! `abandon_eval` writes a new [`EnvState`] whose `current` is a clone of the
//! **previous** `current`. Readers holding an `Arc<EnvState>` from before the
//! `begin_eval` see the old snapshot until they reload; readers who load
//! after `abandon_eval` see a state whose `current` equals what it was before
//! `begin_eval`. The pointer never becomes `None` on failure, only on
//! never-succeeded startup.

use std::path::PathBuf;
use std::time::SystemTime;

use arc_swap::ArcSwap;

/// Job identifier attached to an [`EnvSnapshot`].
///
/// The daemon-wide `job_registry` (sibling task 2.2) will provide the
/// authoritative `JobId` type. This crate compiles standalone today, so we
/// carry a local placeholder here.
///
/// TODO(integrator): replace with `crate::daemon::job_registry::JobId` once
/// task 2.2 lands. The wire representation (a string) is compatible with the
/// `protocol_v3::query::JobId = String` alias from task 1.4.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(pub String);

impl JobId {
  /// Construct a [`JobId`] from any string-like value.
  pub fn new(id: impl Into<String>) -> Self {
    Self(id.into())
  }

  /// Borrow the underlying id as a string slice.
  #[must_use]
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

/// One evaluated devshell env pointer.
///
/// This is the payload the daemon publishes for consumers (interactive shells,
/// `xi develop diag`, watchers) to source the resulting env file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSnapshot {
  /// Path under `/nix/store/...` (or the daemon's env-file staging dir) that
  /// downstream consumers should read.
  pub store_path: PathBuf,
  /// Content-addressed hash of the `flake.lock` (or equivalent) that produced
  /// this env. Used by clients to detect drift without re-evaluating.
  pub lock_hash: String,
  /// Wall-clock time the eval finished. Only used for diagnostics — do not
  /// rely on this for ordering (use `JobId` sequencing).
  pub evaluated_at: SystemTime,
  /// The job that produced this snapshot. Ties the env back to a completed
  /// unit of work in the job registry.
  pub job_id: JobId,
}

/// Internal composite state — the payload of the single `ArcSwap`.
#[derive(Debug, Clone, Default)]
struct EnvState {
  current: Option<EnvSnapshot>,
  pending: Option<EnvSnapshot>,
}

/// Atomic pair of `current` / `pending` env pointers.
///
/// See module-level docs for the state machine and SC-014 rationale.
#[derive(Debug)]
pub struct EnvPointer {
  state: ArcSwap<EnvState>,
}

impl Default for EnvPointer {
  fn default() -> Self {
    Self::new()
  }
}

impl EnvPointer {
  /// Fresh pointer with no `current` and no `pending`.
  #[must_use]
  pub fn new() -> Self {
    Self {
      state: ArcSwap::from_pointee(EnvState::default()),
    }
  }

  /// Mark an eval as in flight. Sets `pending` and leaves `current` untouched.
  ///
  /// Called by the state machine on entry to `Evaluating`. Overwrites any
  /// prior `pending` (a later eval supersedes an earlier one).
  pub fn begin_eval(&self, pending: EnvSnapshot) {
    // `rcu` (read-copy-update) is a compare-and-swap loop: we clone the
    // observed state, mutate the clone, and swap only if nothing else beat
    // us. This is contention-safe without any locks.
    self.state.rcu(|cur| EnvState {
      current: cur.current.clone(),
      pending: Some(pending.clone()),
    });
  }

  /// Promote `pending` → `current` and clear `pending`.
  ///
  /// Called on successful eval. If no eval was in flight, this is a no-op
  /// (`current` unchanged).
  pub fn commit_eval(&self) {
    self.state.rcu(|cur| match cur.pending.clone() {
      Some(new_current) => EnvState {
        current: Some(new_current),
        pending: None,
      },
      None => EnvState {
        current: cur.current.clone(),
        pending: None,
      },
    });
  }

  /// Discard the in-flight eval; keep `current` untouched.
  ///
  /// Called on `BuildFailed`, `AbortJob`, or `ConfigError`. This is the core
  /// mechanism behind SC-014 — after this call, `current()` returns exactly
  /// what it returned before the matching `begin_eval`.
  pub fn abandon_eval(&self) {
    self.state.rcu(|cur| EnvState {
      current: cur.current.clone(),
      pending: None,
    });
  }

  /// Cheap atomic read of the last-good env, if one exists.
  ///
  /// Consumers should call this at every subscription boundary. Returns
  /// `None` only before the first successful eval.
  #[must_use]
  pub fn current(&self) -> Option<EnvSnapshot> {
    self.state.load().current.clone()
  }

  /// Cheap atomic read of the in-flight env, if one exists.
  ///
  /// Diagnostic surface only — real consumers should wait for
  /// `commit_eval` to promote before sourcing.
  #[must_use]
  pub fn pending(&self) -> Option<EnvSnapshot> {
    self.state.load().pending.clone()
  }

  /// `true` iff an eval is in flight (i.e. `pending.is_some()`).
  #[must_use]
  pub fn is_evaluating(&self) -> bool {
    self.state.load().pending.is_some()
  }
}

impl EnvPointer {
  /// Test-only helper — swap in an arbitrary state directly. Not exposed
  /// outside the crate.
  #[cfg(test)]
  fn set_current_for_test(&self, snapshot: EnvSnapshot) {
    self.state.rcu(|cur| EnvState {
      current: Some(snapshot.clone()),
      pending: cur.pending.clone(),
    });
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::thread;
  use std::time::{Duration, SystemTime};

  use super::*;

  fn snap(id: &str, path: &str) -> EnvSnapshot {
    EnvSnapshot {
      store_path: PathBuf::from(path),
      lock_hash: format!("hash-{id}"),
      evaluated_at: SystemTime::now(),
      job_id: JobId::new(id),
    }
  }

  #[test]
  fn new_pointer_has_no_current_and_no_pending() {
    let p = EnvPointer::new();
    assert!(p.current().is_none());
    assert!(p.pending().is_none());
    assert!(!p.is_evaluating());
  }

  #[test]
  fn begin_eval_sets_pending_and_leaves_current_untouched() {
    let p = EnvPointer::new();
    let good = snap("good", "/nix/store/good");
    p.set_current_for_test(good.clone());

    let in_flight = snap("in-flight", "/nix/store/in-flight");
    p.begin_eval(in_flight.clone());

    assert_eq!(p.current().as_ref(), Some(&good));
    assert_eq!(p.pending().as_ref(), Some(&in_flight));
    assert!(p.is_evaluating());
  }

  #[test]
  fn commit_eval_promotes_pending_to_current_and_clears_pending() {
    let p = EnvPointer::new();
    let good = snap("good", "/nix/store/good");
    p.set_current_for_test(good.clone());

    let in_flight = snap("in-flight", "/nix/store/in-flight");
    p.begin_eval(in_flight.clone());
    p.commit_eval();

    assert_eq!(p.current().as_ref(), Some(&in_flight));
    assert!(p.pending().is_none());
    assert!(!p.is_evaluating());
  }

  #[test]
  fn commit_eval_without_pending_is_a_noop() {
    let p = EnvPointer::new();
    let good = snap("good", "/nix/store/good");
    p.set_current_for_test(good.clone());

    p.commit_eval();

    assert_eq!(p.current().as_ref(), Some(&good));
    assert!(p.pending().is_none());
  }

  /// SC-014: `abandon_eval` (simulating `BuildFailed`) must clear `pending`
  /// while leaving `current` bit-identical to what it was before
  /// `begin_eval`. This is the primary invariant this module guards.
  #[test]
  fn abandon_eval_clears_pending_and_leaves_current_intact_sc014() {
    let p = EnvPointer::new();
    let good = snap("good", "/nix/store/good");
    p.set_current_for_test(good.clone());

    let before = p.current();
    let in_flight = snap("in-flight", "/nix/store/in-flight");
    p.begin_eval(in_flight);
    p.abandon_eval();
    let after = p.current();

    assert_eq!(before, after, "SC-014: current_env must survive BuildFailed");
    assert_eq!(after.as_ref(), Some(&good));
    assert!(p.pending().is_none());
    assert!(!p.is_evaluating());
  }

  #[test]
  fn abandon_eval_when_current_is_none_leaves_none() {
    let p = EnvPointer::new();
    let in_flight = snap("in-flight", "/nix/store/in-flight");
    p.begin_eval(in_flight);
    p.abandon_eval();

    assert!(p.current().is_none());
    assert!(p.pending().is_none());
  }

  #[test]
  fn is_evaluating_toggles_across_lifecycle() {
    let p = EnvPointer::new();
    assert!(!p.is_evaluating());

    p.begin_eval(snap("a", "/nix/store/a"));
    assert!(p.is_evaluating());

    p.commit_eval();
    assert!(!p.is_evaluating());

    p.begin_eval(snap("b", "/nix/store/b"));
    assert!(p.is_evaluating());

    p.abandon_eval();
    assert!(!p.is_evaluating());
  }

  #[test]
  fn successive_begin_eval_overwrites_prior_pending() {
    let p = EnvPointer::new();
    let good = snap("good", "/nix/store/good");
    p.set_current_for_test(good.clone());

    p.begin_eval(snap("first", "/nix/store/first"));
    let second = snap("second", "/nix/store/second");
    p.begin_eval(second.clone());

    assert_eq!(p.pending().as_ref(), Some(&second));
    assert_eq!(p.current().as_ref(), Some(&good));
  }

  /// Under concurrent begin/abandon cycles, `current()` must always return
  /// the last-good snapshot — never `None`, never a torn read.
  #[test]
  fn concurrent_readers_never_see_torn_current_during_failed_eval() {
    let p = Arc::new(EnvPointer::new());
    let good = snap("good", "/nix/store/good");
    p.set_current_for_test(good.clone());

    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let reader_p = Arc::clone(&p);
    let good_for_reader = good.clone();

    let reader = thread::spawn(move || {
      let mut observations: u64 = 0;
      while !reader_stop.load(Ordering::Relaxed) {
        let obs = reader_p.current();
        assert_eq!(
          obs.as_ref(),
          Some(&good_for_reader),
          "current() diverged from the last-good snapshot during a failed-eval cycle",
        );
        observations = observations.wrapping_add(1);
      }
      observations
    });

    // Writer: hammer begin_eval + abandon_eval (never commit_eval, so current
    // must never change).
    let deadline = std::time::Instant::now() + Duration::from_millis(120);
    let mut cycle: u64 = 0;
    while std::time::Instant::now() < deadline {
      let candidate = snap(
        &format!("candidate-{cycle}"),
        &format!("/nix/store/candidate-{cycle}"),
      );
      p.begin_eval(candidate);
      p.abandon_eval();
      cycle = cycle.wrapping_add(1);
    }
    stop.store(true, Ordering::Relaxed);
    let observations = reader.join().expect("reader panicked");

    assert!(cycle > 0, "writer should have run at least one cycle");
    assert!(observations > 0, "reader should have observed at least once");
    // Final state is still the last-good.
    assert_eq!(p.current().as_ref(), Some(&good));
    assert!(!p.is_evaluating());
  }

  #[test]
  fn concurrent_reader_across_commit_sees_either_old_or_new_never_none() {
    let p = Arc::new(EnvPointer::new());
    let old = snap("old", "/nix/store/old");
    p.set_current_for_test(old.clone());
    let new = snap("new", "/nix/store/new");

    let reader_p = Arc::clone(&p);
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let old_for_reader = old.clone();
    let new_for_reader = new.clone();

    let reader = thread::spawn(move || {
      while !reader_stop.load(Ordering::Relaxed) {
        let obs = reader_p
          .current()
          .expect("current must never be None once a snapshot has been committed");
        assert!(
          obs == old_for_reader || obs == new_for_reader,
          "torn read observed: {obs:?}",
        );
      }
    });

    // A single begin_eval + commit_eval flips current old → new.
    p.begin_eval(new.clone());
    p.commit_eval();
    // Give the reader a tick to observe the new state.
    thread::sleep(Duration::from_millis(5));
    stop.store(true, Ordering::Relaxed);
    reader.join().expect("reader panicked");

    assert_eq!(p.current().as_ref(), Some(&new));
    assert!(p.pending().is_none());
  }

}
