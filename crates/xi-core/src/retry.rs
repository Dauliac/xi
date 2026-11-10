//! Blocking retry-with-backoff primitive for transient failures.
//!
//! Every `ssh`/`rsync`/`nix copy` call in this workspace is a blocking
//! `std::process` spawn, so this is `thread::sleep`-based rather than
//! `tokio`-based  -  see `push::limits`'s documented rationale for staying
//! off an async runtime in a crate where nothing is actually async.
//!
//! The retry boundary is deliberately *outside* the call this module
//! wraps: whether a given attempt's outcome is worth retrying is a
//! judgement only the caller can make (a local spawn failure is always
//! worth another try; a remote command that ran and failed may have left
//! partial, non-idempotent state and must not be retried blindly). This
//! module only owns the backoff schedule and the attempt loop; the
//! `classify` closure is where that judgement lives.

use std::{
  hash::{BuildHasher, Hasher},
  thread,
  time::Duration,
};

use color_eyre::{Result, eyre::Context};
use tracing::debug;

/// Whether an attempt's result is worth retrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transience {
  /// May clear on its own; spend another attempt on it.
  Transient,
  /// Retrying would not help  -  a clean success, or a failure that is
  /// final (e.g. a remote command that ran and may have left partial
  /// state behind).
  Permanent,
}

/// Exponential backoff with a cap, driving a bounded number of attempts.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
  /// Total attempts, including the first. `1` disables retrying.
  pub max_attempts: u32,
  /// Delay before the first retry.
  pub base_delay:   Duration,
  /// Upper bound on any single delay, before jitter is applied.
  pub cap:          Duration,
  /// Whether to randomize each delay within `[0, scheduled)` ("full
  /// jitter"), rather than sleeping the scheduled delay exactly. Spreads
  /// out retries from callers that all failed at the same moment (e.g. a
  /// flaky link dropping several concurrent pushes at once).
  pub jitter:       bool,
}

impl Default for RetryPolicy {
  /// 3 attempts, exponential backoff from 500ms capped at 10s, jittered.
  ///
  /// Matches the proposal's documented default: additive-only, so a
  /// connection that never fails sees no behavior change.
  fn default() -> Self {
    Self {
      max_attempts: 3,
      base_delay:   Duration::from_millis(500),
      cap:          Duration::from_secs(10),
      jitter:       true,
    }
  }
}

impl RetryPolicy {
  /// A policy with the given knobs. `max_attempts` is clamped to at
  /// least `1`  -  zero attempts would never call the closure at all.
  #[must_use]
  pub fn new(
    max_attempts: u32,
    base_delay: Duration,
    cap: Duration,
    jitter: bool,
  ) -> Self {
    Self {
      max_attempts: max_attempts.max(1),
      base_delay,
      cap,
      jitter,
    }
  }

  /// A policy that never retries: one attempt, bare error on failure.
  #[must_use]
  pub fn once() -> Self {
    Self {
      max_attempts: 1,
      ..Self::default()
    }
  }

  /// Delay before the retry that follows the `retry_index`'th failed
  /// attempt (`0` for the delay after the first attempt).
  fn delay_for(&self, retry_index: u32) -> Duration {
    let factor = 1u64.checked_shl(retry_index).unwrap_or(u64::MAX);
    let base_ms =
      u64::try_from(self.base_delay.as_millis()).unwrap_or(u64::MAX);
    let cap_ms = u64::try_from(self.cap.as_millis()).unwrap_or(u64::MAX);
    let scheduled_ms = base_ms.saturating_mul(factor).min(cap_ms);
    let scheduled = Duration::from_millis(scheduled_ms);
    if self.jitter {
      full_jitter(scheduled)
    } else {
      scheduled
    }
  }
}

/// A uniformly random duration in `[0, upper]`.
///
/// `RandomState::new()` does not hit the OS on every call: each thread
/// caches a seed pair drawn from the OS once, and every subsequent
/// `RandomState::new()` on that thread just derives from the cached
/// value - cheap, and different enough from one call to the next to
/// spread out retries without pulling in a `rand` dependency for it.
/// Scaling is done in integer arithmetic (`u128` headroom, no floats) so
/// the result stays an exact fraction of `upper` rather than an
/// approximation.
fn full_jitter(upper: Duration) -> Duration {
  let sample = std::collections::hash_map::RandomState::new()
    .build_hasher()
    .finish();
  let scaled_nanos =
    upper.as_nanos().saturating_mul(u128::from(sample)) / u128::from(u64::MAX);
  Duration::from_nanos(u64::try_from(scaled_nanos).unwrap_or(u64::MAX))
}

/// Run `attempt` until `classify` calls its result [`Transience::Permanent`]
/// or `policy.max_attempts` is exhausted, sleeping a backoff delay between
/// tries.
///
/// `classify` sees every attempt's result, `Ok` or `Err` alike, so it can
/// encode judgements like "an `Err` here is always transient" or "an `Ok`
/// with this particular shape is a final failure"  -  this primitive has no
/// opinion on what makes a given `T` or error retry-worthy.
///
/// If more than one attempt was made, the returned `Err` is wrapped with
/// the attempt count so the caller sees more than the last attempt's bare
/// error; a single-attempt policy (or a `Permanent` verdict on the first
/// attempt) returns the error exactly as `attempt` produced it.
///
/// Each scheduled retry emits one `debug`-level `"transient failure;
/// retrying"` line - the only operator-visible difference between a call
/// that needed retrying and one that did not, since the returned `T` or
/// `Err` is otherwise identical either way.
///
/// # Errors
///
/// Returns whatever `attempt` last produced, once `classify` calls it
/// [`Transience::Permanent`] or attempts run out.
pub fn retry<T>(
  policy: &RetryPolicy,
  classify: impl Fn(&Result<T>) -> Transience,
  mut attempt: impl FnMut() -> Result<T>,
) -> Result<T> {
  let attempts = policy.max_attempts.max(1);
  let mut attempt_index = 0;
  loop {
    let result = attempt();
    let is_last = attempt_index + 1 == attempts;
    let transience = classify(&result);
    if is_last || transience == Transience::Permanent {
      return if attempt_index > 0 {
        result.wrap_err_with(|| {
          format!("gave up after {} attempts", attempt_index + 1)
        })
      } else {
        result
      };
    }
    let delay = policy.delay_for(attempt_index);
    debug!(
      attempt = attempt_index + 1,
      max_attempts = attempts,
      delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
      "transient failure; retrying"
    );
    thread::sleep(delay);
    attempt_index += 1;
  }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use std::sync::atomic::{AtomicU32, Ordering};

  use color_eyre::eyre::eyre;

  use super::*;

  fn immediate(max_attempts: u32) -> RetryPolicy {
    RetryPolicy::new(
      max_attempts,
      Duration::from_millis(1),
      Duration::from_millis(5),
      false,
    )
  }

  fn always_transient_on_err<T>(result: &Result<T>) -> Transience {
    if result.is_err() {
      Transience::Transient
    } else {
      Transience::Permanent
    }
  }

  #[test]
  fn succeeds_on_first_attempt_without_sleeping() {
    let calls = AtomicU32::new(0);
    let result = retry(&immediate(3), always_transient_on_err, || {
      calls.fetch_add(1, Ordering::SeqCst);
      Ok(42)
    });
    assert_eq!(result.unwrap(), 42);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn retries_transient_failures_until_success() {
    let calls = AtomicU32::new(0);
    let result = retry(&immediate(3), always_transient_on_err, || {
      let n = calls.fetch_add(1, Ordering::SeqCst);
      if n < 2 {
        Err(eyre!("transient failure #{n}"))
      } else {
        Ok("ok")
      }
    });
    assert_eq!(result.unwrap(), "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
  }

  #[test]
  fn exhausts_attempts_and_wraps_with_attempt_count() {
    let calls = AtomicU32::new(0);
    let result: Result<()> =
      retry(&immediate(3), always_transient_on_err, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(eyre!("always fails"))
      });
    let err = result.unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(err.to_string().contains("gave up after 3 attempts"));
    assert!(format!("{err:#}").contains("always fails"));
  }

  #[test]
  fn single_attempt_policy_never_retries_and_returns_bare_error() {
    let calls = AtomicU32::new(0);
    let result: Result<()> =
      retry(&RetryPolicy::once(), always_transient_on_err, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(eyre!("bare failure"))
      });
    let err = result.unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(err.to_string(), "bare failure");
  }

  #[test]
  fn permanent_verdict_on_first_attempt_stops_immediately() {
    let calls = AtomicU32::new(0);
    let result: Result<()> = retry(
      &immediate(5),
      |_: &Result<()>| Transience::Permanent,
      || {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(eyre!("final, do not retry"))
      },
    );
    let err = result.unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(err.to_string(), "final, do not retry");
  }

  #[test]
  fn delay_for_grows_exponentially_and_respects_cap() {
    let policy = RetryPolicy::new(
      10,
      Duration::from_millis(100),
      Duration::from_secs(1),
      false,
    );
    assert_eq!(policy.delay_for(0), Duration::from_millis(100));
    assert_eq!(policy.delay_for(1), Duration::from_millis(200));
    assert_eq!(policy.delay_for(2), Duration::from_millis(400));
    // 100ms * 2^10 would overflow the 1s cap; it must clamp, not panic.
    assert_eq!(policy.delay_for(10), Duration::from_secs(1));
  }

  #[test]
  fn full_jitter_never_exceeds_its_upper_bound() {
    let upper = Duration::from_millis(50);
    for _ in 0..200 {
      assert!(full_jitter(upper) <= upper);
    }
  }
}
