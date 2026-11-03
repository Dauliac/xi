//! Independent per-remote scheduling.
//!
//! [`crate::main`]'s loop used to tick every configured remote on every
//! pass, sleeping for the *shortest* configured `poll_interval` in
//! between - which meant a remote configured with a long interval (a
//! quiet mirror, polled hourly to be a good citizen) was ticked just as
//! often as a remote configured with a short one. That is not what the
//! spec's "Each remote carries its own `poll_interval`" line asks for:
//! each entry's cadence is its own, not clamped to the minimum across
//! the set.
//!
//! [`Scheduler`] tracks one `next_due` instant per remote (by index, in
//! the same order as the resolved remote list) and answers two
//! questions: which remotes are due *now* ([`Scheduler::due`]), and how
//! long until the next one becomes due ([`Scheduler::sleep_duration`]).
//! It does not run anything itself - [`crate::main`] still funnels every
//! due remote's tick through the one [`crate::singleton::Singleton`]
//! gate, which is what gives the set its "shared `sources_head` writer
//! with a mutex" property: two remotes becoming due in the same instant
//! still serialise through that gate, so the ancestry check in
//! [`crate::tick::run`] is always made against a `sources_head` no
//! concurrent write could have moved out from under it.

use std::time::{Duration, Instant};

/// One remote's own polling cadence, independent of every other
/// configured remote's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
  next_due:      Instant,
  poll_interval: Duration,
}

/// Tracks each remote's next-due instant independently.
#[derive(Debug, Clone)]
pub struct Scheduler {
  entries: Vec<Entry>,
}

impl Scheduler {
  /// A scheduler for `poll_intervals.len()` remotes, all due immediately.
  /// That is the shape a freshly started daemon wants, so its first pass
  /// ticks every configured remote once, matching `--once`'s existing
  /// "one tick per configured remote" contract.
  #[must_use]
  pub fn new(now: Instant, poll_intervals: &[Duration]) -> Self {
    Self {
      entries: poll_intervals
        .iter()
        .map(|&poll_interval| {
          Entry {
            next_due: now,
            poll_interval,
          }
        })
        .collect(),
    }
  }

  /// Indices (into the slice `poll_intervals` was built from) that are
  /// due to tick at `now`.
  #[must_use]
  pub fn due(&self, now: Instant) -> Vec<usize> {
    self
      .entries
      .iter()
      .enumerate()
      .filter_map(|(i, e)| (e.next_due <= now).then_some(i))
      .collect()
  }

  /// Record that remote `index` just ticked at `now`; it becomes due
  /// again at `now + its own poll_interval`.
  ///
  /// `now` is the completion time, not the time it became due: a slow
  /// fetch pushes the *next* tick back by its own duration rather than
  /// letting the scheduler immediately mark it due again and starve the
  /// other remotes, which is the same "a slow fetch cannot stack"
  /// property [`crate::singleton::Singleton`] gives a single remote,
  /// extended to the set.
  pub fn mark_ticked(&mut self, index: usize, now: Instant) {
    if let Some(entry) = self.entries.get_mut(index) {
      entry.next_due = now + entry.poll_interval;
    }
  }

  /// How long to sleep before any remote becomes due again, measured
  /// from `now`. `Duration::ZERO` when one is already due (or overdue).
  #[must_use]
  pub fn sleep_duration(&self, now: Instant) -> Duration {
    self
      .entries
      .iter()
      .map(|e| e.next_due.saturating_duration_since(now))
      .min()
      .unwrap_or(Duration::ZERO)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A fresh scheduler has every remote due on its very first pass -
  /// matching a freshly started daemon, and `--once`'s "one tick per
  /// configured remote" contract.
  #[test]
  fn a_fresh_scheduler_has_every_remote_due_immediately() {
    let now = Instant::now();
    let scheduler = Scheduler::new(now, &[
      Duration::from_secs(30),
      Duration::from_mins(5),
    ]);
    assert_eq!(scheduler.due(now), vec![0, 1]);
  }

  /// The property this bead exists for: a short-interval remote and a
  /// long-interval remote do not tick on the same cadence. After both
  /// tick once, only the short one is due again soon; the long one stays
  /// quiet.
  #[test]
  fn each_remote_is_scheduled_on_its_own_interval() {
    let now = Instant::now();
    let mut scheduler =
      Scheduler::new(now, &[Duration::from_secs(30), Duration::from_mins(5)]);

    scheduler.mark_ticked(0, now);
    scheduler.mark_ticked(1, now);

    let ten_seconds_later = now + Duration::from_secs(10);
    assert!(
      scheduler.due(ten_seconds_later).is_empty(),
      "neither remote should be due yet"
    );

    let after_the_short_interval = now + Duration::from_secs(31);
    assert_eq!(
      scheduler.due(after_the_short_interval),
      vec![0],
      "only the 30s remote should be due; the 300s one must stay quiet"
    );

    let after_both_intervals = now + Duration::from_secs(301);
    assert_eq!(
      scheduler.due(after_both_intervals),
      vec![0, 1],
      "by 301s both remotes have become due"
    );
  }

  /// One remote's outage (modelled here as simply never being marked
  /// ticked) must not block another remote's cadence - the scheduler
  /// only ever looks at the instant each entry says it is next due, never
  /// at whether some other entry made progress.
  #[test]
  fn one_remote_falling_behind_does_not_delay_another() {
    let now = Instant::now();
    let mut scheduler =
      Scheduler::new(now, &[Duration::from_mins(1), Duration::from_mins(1)]);

    // Remote 0 ticks (and keeps ticking); remote 1 never does, as if its
    // fetches keep failing before `mark_ticked` is ever reached.
    scheduler.mark_ticked(0, now);

    let later = now + Duration::from_secs(61);
    assert_eq!(
      scheduler.due(later),
      vec![0, 1],
      "remote 0's own cadence must still bring it due on schedule"
    );
  }

  /// A slow tick pushes the *next* due time out from its own completion,
  /// not from when it became due - otherwise a tick that took longer than
  /// its own interval would read as immediately due again and starve
  /// every other remote behind the singleton gate.
  #[test]
  fn mark_ticked_measures_the_next_interval_from_completion_not_from_due_time() {
    let now = Instant::now();
    let mut scheduler = Scheduler::new(now, &[Duration::from_secs(30)]);

    // The tick became due at `now` but took 45s to actually run.
    let completed_at = now + Duration::from_secs(45);
    scheduler.mark_ticked(0, completed_at);

    assert!(
      scheduler.due(completed_at).is_empty(),
      "must not read as immediately due again right after completing"
    );
    assert_eq!(
      scheduler.due(completed_at + Duration::from_secs(30)),
      vec![0]
    );
  }

  /// `sleep_duration` is what the daemon's loop actually sleeps on
  /// between passes; it must track whichever remote is closest to due,
  /// not the slowest one.
  #[test]
  fn sleep_duration_is_bounded_by_the_soonest_remote() {
    let now = Instant::now();
    let mut scheduler =
      Scheduler::new(now, &[Duration::from_secs(30), Duration::from_mins(5)]);
    scheduler.mark_ticked(0, now);
    scheduler.mark_ticked(1, now);

    assert_eq!(scheduler.sleep_duration(now), Duration::from_secs(30));
  }

  #[test]
  fn sleep_duration_is_zero_when_something_is_already_due() {
    let now = Instant::now();
    let scheduler = Scheduler::new(now, &[Duration::from_mins(1)]);
    assert_eq!(scheduler.sleep_duration(now), Duration::ZERO);
  }
}
