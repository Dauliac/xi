//! Independent eval and apply concurrency limits.
//!
//! Two counting semaphores, not one: a target that is still evaluating
//! must not hold a slot that a different target could be activating in.
//! That split is the behaviour colmena gets from its separate
//! `eval_node_limit` / `apply_node_limit` pair, and without it a slow
//! evaluation serialises the whole fleet.
//!
//! `tasks.md` names `tokio::sync::Semaphore`. This crate - and the whole
//! workspace - is synchronous: every Nix and SSH call is a blocking
//! `std::process` spawn. Pulling in an async runtime to gate blocking
//! work would add a dependency and an executor without changing the
//! observable contract, so the same split-semaphore semantics are built
//! on `std::sync` here. See the note in
//! `openspec/changes/xi-sources-push/tasks.md`.

use std::sync::{Condvar, Mutex};

/// A counting semaphore with a blocking acquire.
#[derive(Debug)]
pub struct Semaphore {
  permits: Mutex<usize>,
  free:    Condvar,
}

impl Semaphore {
  /// A semaphore with `permits` slots.
  ///
  /// Zero is clamped to one: a limit of zero would deadlock every
  /// caller, and the CLI already rejects `--parallel 0`.
  #[must_use]
  pub fn new(permits: usize) -> Self {
    Self {
      permits: Mutex::new(permits.max(1)),
      free:    Condvar::new(),
    }
  }

  /// Take a permit, blocking until one is free.
  ///
  /// A poisoned mutex is recovered from rather than propagated: the
  /// guarded state is a single counter, so a panic while holding it
  /// cannot have left it in a shape the next waiter can misread.
  pub fn acquire(&self) -> Permit<'_> {
    let mut permits = self
      .permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *permits == 0 {
      permits = self
        .free
        .wait(permits)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    *permits -= 1;
    // Released before the permit is handed back, so the caller does not
    // contend with the next waiter for a lock it no longer needs.
    drop(permits);
    Permit { semaphore: self }
  }
}

/// A held permit, returned on drop.
#[derive(Debug)]
pub struct Permit<'a> {
  semaphore: &'a Semaphore,
}

impl Drop for Permit<'_> {
  fn drop(&mut self) {
    let mut permits = self
      .semaphore
      .permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    *permits += 1;
    drop(permits);
    self.semaphore.free.notify_one();
  }
}

/// The two independent phase limits of one push run.
#[derive(Debug)]
pub struct PhaseLimits {
  /// Gate on local evaluation and building.
  pub eval:  Semaphore,
  /// Gate on anything that mutates a target.
  pub apply: Semaphore,
}

impl PhaseLimits {
  /// Both phases limited to `parallel` concurrent targets.
  ///
  /// The two counts are equal but the semaphores are not shared, which
  /// is the whole point: with `--parallel 2` and three targets, one can
  /// be activating while another evaluates.
  #[must_use]
  pub fn new(parallel: usize) -> Self {
    Self {
      eval:  Semaphore::new(parallel),
      apply: Semaphore::new(parallel),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::{
    sync::{
      Arc,
      atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
  };

  use super::*;

  /// Tracks how many holders were inside the gate at the same time.
  #[derive(Debug, Default)]
  struct Peak {
    current: AtomicUsize,
    peak:    AtomicUsize,
  }

  impl Peak {
    fn enter(&self) {
      let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
      self.peak.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
      self.current.fetch_sub(1, Ordering::SeqCst);
    }
  }

  #[test]
  fn a_semaphore_caps_concurrent_holders() {
    let semaphore = Arc::new(Semaphore::new(2));
    let peak = Arc::new(Peak::default());

    let handles: Vec<_> = (0..6)
      .map(|_| {
        let semaphore = Arc::clone(&semaphore);
        let peak = Arc::clone(&peak);
        std::thread::spawn(move || {
          let permit = semaphore.acquire();
          peak.enter();
          std::thread::sleep(Duration::from_millis(20));
          peak.leave();
          drop(permit);
        })
      })
      .collect();

    for handle in handles {
      assert!(handle.join().is_ok(), "worker panicked");
    }
    assert!(
      peak.peak.load(Ordering::SeqCst) <= 2,
      "saw {} concurrent holders, cap is 2",
      peak.peak.load(Ordering::SeqCst)
    );
  }

  #[test]
  fn the_two_phases_do_not_share_permits() {
    let limits = PhaseLimits::new(1);
    let eval = limits.eval.acquire();
    // The apply gate must still be open with the eval gate full; if the
    // two shared a counter this would block forever.
    let apply = limits.apply.acquire();
    drop(apply);
    drop(eval);
  }

  #[test]
  fn a_zero_limit_is_clamped_rather_than_deadlocking() {
    let semaphore = Semaphore::new(0);
    drop(semaphore.acquire());
  }
}
