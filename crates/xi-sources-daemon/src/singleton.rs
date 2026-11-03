//! The per-`SourcesId` semaphore of 1.
//!
//! One scheduler per `SourcesId`, and inside it every tick  -  from every
//! remote  -  passes through one gate. That is what makes a 120s fetch on
//! a 60s interval wait rather than stack, and it is also the "shared
//! `sources_head` writer with a mutex" the multi-remote requirement asks
//! for: two remotes that advance in the same window serialise here, so
//! the second one sees the first one's `sources_head` and its ancestry
//! check is made against reality.
//!
//! # Why not `tokio::sync::Semaphore`
//!
//! The change's task list names `tokio::sync::Semaphore(1)`, following
//! comin's `gocron.LimitModeReschedule`. This crate uses a std
//! [`Mutex`] instead, because nothing else in this workspace is async:
//! `xi-develop`'s daemon is threads plus `crossbeam-channel`, there is no
//! tokio anywhere in the dependency graph, and a poll loop that sleeps
//! for minutes at a time gains nothing from a reactor. The observable
//! property the spec pins  -  "no two fetches run concurrently for the
//! same `SourcesId`", and the second tick *waits* rather than being
//! dropped  -  is exactly what a blocking mutex gives.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// A gate that admits one holder at a time.
#[derive(Debug, Default)]
pub struct Singleton {
  gate: Mutex<()>,
}

impl Singleton {
  /// A fresh, unheld gate.
  #[must_use]
  pub const fn new() -> Self {
    Self {
      gate: Mutex::new(()),
    }
  }

  /// Wait for the gate, then hold it until the returned guard drops.
  ///
  /// Poisoning is recovered from rather than propagated. The gate
  /// protects no data  -  it only orders ticks  -  so a thread that
  /// panicked mid-tick has left nothing inconsistent behind, and
  /// refusing to ever run another tick because one panicked would turn a
  /// single failure into a permanently stuck daemon.
  pub fn enter(&self) -> MutexGuard<'_, ()> {
    self.gate.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Take the gate only if it is free right now.
  ///
  /// Used by the status surface, which wants to report "a tick is in
  /// flight" without queueing behind it.
  #[must_use]
  pub fn try_enter(&self) -> Option<MutexGuard<'_, ()>> {
    match self.gate.try_lock() {
      Ok(guard) => Some(guard),
      Err(std::sync::TryLockError::Poisoned(poisoned)) => {
        Some(poisoned.into_inner())
      },
      Err(std::sync::TryLockError::WouldBlock) => None,
    }
  }
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  reason = "Fine in tests; the panic test deliberately panics to exercise \
            poison recovery"
)]
mod tests {
  use std::{
    sync::{
      Arc,
      atomic::{AtomicU32, Ordering},
    },
    thread,
    time::Duration,
  };

  use super::*;

  /// The SC-G-001 property, scaled down: a slow holder makes the next
  /// entrant wait instead of running beside it.
  #[test]
  fn second_entrant_waits_for_the_first() {
    let gate = Arc::new(Singleton::new());
    let inside = Arc::new(AtomicU32::new(0));
    let peak = Arc::new(AtomicU32::new(0));

    let workers: Vec<_> = (0..4)
      .map(|_| {
        let gate = Arc::clone(&gate);
        let inside = Arc::clone(&inside);
        let peak = Arc::clone(&peak);
        thread::spawn(move || {
          let _permit = gate.enter();
          let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
          peak.fetch_max(now, Ordering::SeqCst);
          thread::sleep(Duration::from_millis(20));
          inside.fetch_sub(1, Ordering::SeqCst);
        })
      })
      .collect();

    for worker in workers {
      worker.join().expect("worker should not panic");
    }

    assert_eq!(
      peak.load(Ordering::SeqCst),
      1,
      "two ticks were inside the gate at once"
    );
  }

  #[test]
  fn try_enter_reports_a_tick_in_flight() {
    let gate = Singleton::new();
    let held = gate.enter();
    assert!(gate.try_enter().is_none(), "gate should read as busy");
    drop(held);
    assert!(gate.try_enter().is_some(), "gate should read as free");
  }

  /// A panicking tick must not wedge the daemon: poisoning is recovered
  /// from, because the gate orders ticks and guards no data.
  #[test]
  fn a_panicked_holder_does_not_wedge_the_gate() {
    let gate = Arc::new(Singleton::new());
    let poisoner = {
      let gate = Arc::clone(&gate);
      thread::spawn(move || {
        let _permit = gate.enter();
        panic!("tick blew up");
      })
    };
    assert!(poisoner.join().is_err(), "the worker should have panicked");

    let _permit = gate.enter();
  }
}
