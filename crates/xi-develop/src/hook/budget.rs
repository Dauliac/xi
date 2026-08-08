//! Per-tick budget guard for the prompt hook.
//!
//! Every prompt tick is bounded by a total client budget (default 100 ms; see
//! [`crate::timeouts::Timeouts::prompt_budget`]). The client MUST NOT block the
//! shell for more than that window on any single tick, so every network,
//! filesystem, or lock operation on the hook path consults a [`Budget`] before
//! it commits.
//!
//! The type intentionally has zero deps beyond `std`: it is used from cold
//! start paths where every allocation matters.

use std::time::{Duration, Instant};

/// A monotonic per-tick deadline.
///
/// Started once at the top of a prompt tick; queried by every step that could
/// otherwise overshoot the client's total budget. The type is `Copy` — pass
/// it by value.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
  /// Instant at which the budget was armed.
  start:    Instant,
  /// Total allowed wall-time from [`Self::start`].
  duration: Duration,
}

impl Budget {
  /// Start a new budget counting from now with the given total duration.
  #[must_use]
  pub fn start(duration: Duration) -> Self {
    Self { start: Instant::now(), duration }
  }

  /// True iff the budget has been fully consumed.
  ///
  /// Callers on the hot path use this as an early-return signal; a `true`
  /// result means "stop trying, enqueue for the next tick".
  #[must_use]
  pub fn exceeded(&self) -> bool {
    self.start.elapsed() >= self.duration
  }

  /// Time still available on the budget. Saturates to zero once exceeded so
  /// downstream `Duration` arithmetic stays well-defined.
  #[must_use]
  pub fn remaining(&self) -> Duration {
    self.duration.saturating_sub(self.start.elapsed())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn does_not_trigger_before_deadline() {
    let b = Budget::start(Duration::from_secs(60));
    assert!(!b.exceeded());
    assert!(b.remaining() > Duration::from_secs(59));
  }

  #[test]
  fn triggers_at_or_past_deadline() {
    let b = Budget::start(Duration::from_millis(1));
    // Busy-sleep past the deadline. We use a short sleep rather than a spin
    // loop to keep the test cheap.
    std::thread::sleep(Duration::from_millis(5));
    assert!(b.exceeded());
    assert_eq!(b.remaining(), Duration::ZERO);
  }

  #[test]
  fn zero_duration_is_immediately_exceeded() {
    let b = Budget::start(Duration::ZERO);
    assert!(b.exceeded());
    assert_eq!(b.remaining(), Duration::ZERO);
  }
}
