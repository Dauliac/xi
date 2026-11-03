//! The daemon pause/resume protocol for inbound push (`xi-sources-race`).
//!
//! An inbound `xi push` and a gitops daemon tick both want to write the same
//! sources dir. The lock in [`super::lock`] makes that safe but not pleasant:
//! a push arriving mid-tick would sit in the queue behind every subsequent
//! tick. So before a push takes the lock it asks the local daemon to stand
//! down, and hands the daemon back when it is done.
//!
//! # The seam
//!
//! The control socket, its path, and its framing belong to
//! `xi-sources-gitops`, which does not exist yet. This module owns the half
//! that is not the daemon's: the wire types, and [`PauseController`] - the
//! state machine deciding when ticks may run. The daemon wires them up by
//! doing exactly four things:
//!
//! 1. Hold one [`PauseController`] for the life of the process.
//! 2. On a decoded [`PauseRequest`], call [`PauseController::on_request`],
//!    passing whether a tick is in flight right now. Reply with the returned
//!    [`PauseReply`] - except for [`PauseReply::Pausing`], which is a
//!    *deferred* reply (see below).
//! 3. Before starting a tick, call [`PauseController::gate`] and skip the tick
//!    on [`TickGate::Hold`].
//! 4. When a tick finishes, drop the lock, then call
//!    [`PauseController::on_tick_complete`]. A returned reply is the answer
//!    owed to the `pause` caller from step 2.
//!
//! Step 4's ordering is task 4.2's whole content: `pause` must not return
//! until the lock is actually released, or the push would race the tick it
//! just asked to stop.
//!
//! # Why a controller and not a flag
//!
//! Three things can end a pause - an explicit `resume`, the timeout, and the
//! requester dying - and two of them are discovered by polling rather than by
//! a message arriving. Centralising that in one type is what keeps "the
//! daemon is wedged because a push crashed in CI three hours ago" from being
//! possible.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::lock::pid_is_running;

/// How long a pause survives without a matching `resume`.
///
/// Thirty minutes is the spec default: long enough for a large switch on the
/// pushing side, short enough that a crashed push does not cost a day of
/// missed ticks.
pub const DEFAULT_PAUSE_TIMEOUT: Duration = Duration::from_mins(30);

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// A control-socket call against the daemon's tick loop.
///
/// `requester_pid` is not decoration: it is how the daemon notices that the
/// push which paused it is gone and resumes without waiting out the timeout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum PauseRequest {
  /// Finish the current tick, release the lock, start no new ticks.
  Pause {
    /// Pid of the process asking (the `xi push` client).
    requester_pid: u32,
    /// Override [`DEFAULT_PAUSE_TIMEOUT`] for this pause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_secs:  Option<u64>,
  },
  /// Restart the tick loop.
  Resume {
    /// Pid of the process asking.
    requester_pid: u32,
  },
}

/// The daemon's answer to a [`PauseRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PauseReply {
  /// Ticks are suspended and the lock is released. Safe to proceed.
  Paused,
  /// A tick is in flight. The daemon owes a [`PauseReply::Paused`] when it
  /// drains; the caller must wait for that, not for this.
  Pausing,
  /// The tick loop is running again.
  Resumed,
  /// `resume` arrived for a daemon that was not paused. Not an error: a push
  /// that times out client-side sends `resume` anyway, and should be able to.
  NotPaused,
}

/// Whether the tick loop may run right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickGate {
  /// Go ahead.
  Run,
  /// Paused. Skip this tick.
  Hold,
}

/// Why a pause ended without a `resume`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum AutoResume {
  /// No `resume` arrived inside the timeout.
  Timeout {
    /// The timeout that elapsed, in seconds.
    after_secs: u64,
  },
  /// The process that paused the daemon no longer exists.
  RequesterGone {
    /// Pid that vanished.
    pid: u32,
  },
}

/// Observable pause state, for the daemon's own status surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum PauseState {
  /// Ticking normally.
  Running,
  /// A pause was requested; the in-flight tick has not finished.
  Pausing {
    /// Pid that asked.
    requester_pid: u32,
  },
  /// Ticks suspended, lock released.
  Paused {
    /// Pid that asked.
    requester_pid: u32,
  },
}

// ---------------------------------------------------------------------------
// PauseController
// ---------------------------------------------------------------------------

/// The tick loop's gate, and every way a pause can end.
#[derive(Debug)]
pub struct PauseController {
  state:     PauseState,
  /// When the current pause (or pause request) started.
  since:     Option<Instant>,
  /// Timeout for the current pause, defaulting to [`DEFAULT_PAUSE_TIMEOUT`].
  timeout:   Duration,
  /// Configured default, restored on each new pause.
  default_t: Duration,
}

impl Default for PauseController {
  fn default() -> Self {
    Self::new(DEFAULT_PAUSE_TIMEOUT)
  }
}

impl PauseController {
  /// A running controller with `timeout` as its auto-resume default.
  #[must_use]
  pub const fn new(timeout: Duration) -> Self {
    Self {
      state: PauseState::Running,
      since: None,
      timeout,
      default_t: timeout,
    }
  }

  /// Current state, for a status surface.
  #[must_use]
  pub const fn state(&self) -> PauseState {
    self.state
  }

  /// Is the tick loop suspended, or on its way to being?
  #[must_use]
  pub const fn is_pausing_or_paused(&self) -> bool {
    !matches!(self.state, PauseState::Running)
  }

  /// Handle a control-socket call.
  ///
  /// `tick_in_flight` is the daemon's own answer to "am I mid-tick right
  /// now". When it is true the reply is [`PauseReply::Pausing`] and the real
  /// answer is owed by [`PauseController::on_tick_complete`]; the lock is
  /// still held at that point and saying `Paused` would be a lie.
  pub fn on_request(
    &mut self,
    request: &PauseRequest,
    tick_in_flight: bool,
    now: Instant,
  ) -> PauseReply {
    match *request {
      PauseRequest::Pause {
        requester_pid,
        timeout_secs,
      } => {
        self.timeout = timeout_secs.map_or(self.default_t, Duration::from_secs);
        self.since = Some(now);

        // A second `pause` from another pid re-arms the timeout and takes
        // ownership. Refusing it would be worse: the first requester may
        // already be gone, and there is no third party to arbitrate.
        if tick_in_flight {
          self.state = PauseState::Pausing { requester_pid };
          info!(
            "pause requested by pid {requester_pid}; draining the in-flight \
             tick before releasing the lock"
          );
          PauseReply::Pausing
        } else {
          self.state = PauseState::Paused { requester_pid };
          info!("paused by pid {requester_pid}; tick loop suspended");
          PauseReply::Paused
        }
      },
      PauseRequest::Resume { requester_pid } => {
        if matches!(self.state, PauseState::Running) {
          return PauseReply::NotPaused;
        }
        info!("resumed by pid {requester_pid}; tick loop restarting");
        self.clear();
        PauseReply::Resumed
      },
    }
  }

  /// The in-flight tick has finished and the lock has been released.
  ///
  /// Returns the deferred [`PauseReply::Paused`] owed to a caller that got
  /// [`PauseReply::Pausing`], or `None` when nobody is waiting on one.
  ///
  /// The daemon MUST drop its [`super::lock::SourcesLock`] before calling
  /// this. That ordering is the contract task 4.2 names: the `pause` caller
  /// treats this reply as permission to acquire.
  pub fn on_tick_complete(&mut self) -> Option<PauseReply> {
    if let PauseState::Pausing { requester_pid } = self.state {
      self.state = PauseState::Paused { requester_pid };
      info!(
        "tick drained and lock released; pause for pid {requester_pid} is \
         complete"
      );
      return Some(PauseReply::Paused);
    }
    None
  }

  /// May a tick start now?
  ///
  /// Also the auto-resume poll: a daemon that only ever asks this question
  /// cannot forget to check whether its pause has expired.
  pub fn gate(&mut self, now: Instant) -> TickGate {
    self.poll_auto_resume(now);
    if self.is_pausing_or_paused() {
      TickGate::Hold
    } else {
      TickGate::Run
    }
  }

  /// End an expired or orphaned pause, naming the reason.
  ///
  /// Uses [`pid_is_running`] for the liveness half. See
  /// [`PauseController::poll_auto_resume_with`] to supply your own.
  pub fn poll_auto_resume(&mut self, now: Instant) -> Option<AutoResume> {
    self.poll_auto_resume_with(now, pid_is_running)
  }

  /// [`PauseController::poll_auto_resume`] with an injected liveness check.
  ///
  /// Taking the predicate as an argument is what lets the orphan path be
  /// asserted without spawning and killing a real process.
  pub fn poll_auto_resume_with(
    &mut self,
    now: Instant,
    is_running: impl Fn(u32) -> bool,
  ) -> Option<AutoResume> {
    let (
      PauseState::Pausing { requester_pid }
      | PauseState::Paused { requester_pid },
      Some(since),
    ) = (self.state, self.since)
    else {
      // Not paused, or paused with no timestamp to age: either way
      // there is nothing for auto-resume to decide.
      return None;
    };

    // Cheaper check first, and the one that is almost always false.
    if now.saturating_duration_since(since) >= self.timeout {
      let reason = AutoResume::Timeout {
        after_secs: self.timeout.as_secs(),
      };
      warn!(
        "pause from pid {requester_pid} expired after {}; resuming the tick \
         loop",
        humantime::format_duration(self.timeout)
      );
      self.clear();
      return Some(reason);
    }

    // Beyond the spec's minimum, and deliberately: the failure the timeout
    // guards against is a push that died, and waiting out thirty minutes to
    // notice something the kernel already knows is pure downtime.
    if !is_running(requester_pid) {
      let reason = AutoResume::RequesterGone { pid: requester_pid };
      warn!(
        "pid {requester_pid} paused the daemon and no longer exists; resuming \
         the tick loop"
      );
      self.clear();
      return Some(reason);
    }

    None
  }

  const fn clear(&mut self) {
    self.state = PauseState::Running;
    self.since = None;
    self.timeout = self.default_t;
  }
}
