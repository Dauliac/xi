//! The closed span vocabulary every tick emits.
//!
//! Two span shapes exist and no more: one [`TickSpan`] root per daemon
//! iteration, and one [`PhaseSpan`] child per phase it reaches. Both
//! declare their whole field set up front (with
//! [`tracing::field::Empty`] for the fields only known at the end), so a
//! log consumer can rely on the keys being present and a sink never has
//! to cope with a span that invented a field.
//!
//! Anything that does not fit the vocabulary is an *event*, not a span
//! field - see the planned `events` store. That split is what keeps the log
//! joinable: `sources_id` means the same thing in every line.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::{Span, field::Empty, info_span};

/// Name of the root span. Fixed: dashboards and OTLP consumers key on
/// it.
pub const TICK_SPAN_NAME: &str = "xi.sources.tick";

/// The actor recorded on every daemon-emitted root span.
///
/// A tick emitted by an interactive `xi switch` is a different actor and
/// would carry a different value, which is why the field exists at all
/// rather than being implied by the span name.
pub const DAEMON_ACTOR: &str = "daemon";

/// How a tick or a phase ended.
///
/// Three states, not two: `Degraded` is the case an operator most needs
/// to see, because it means the loop kept running while something was
/// already wrong.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
  /// Everything the phase set out to do happened.
  Ok,
  /// The phase completed, but something needs an operator's attention.
  Degraded,
  /// The phase did not complete.
  Failed,
}

impl Outcome {
  /// Every spelling, in the order metric labels enumerate them.
  pub const ALL: &'static [Self] = &[Self::Ok, Self::Degraded, Self::Failed];

  /// The wire spelling, matching the serde representation.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Ok => "ok",
      Self::Degraded => "degraded",
      Self::Failed => "failed",
    }
  }

  /// Parse from the wire spelling.
  #[must_use]
  pub fn from_str_opt(raw: &str) -> Option<Self> {
    match raw {
      "ok" => Some(Self::Ok),
      "degraded" => Some(Self::Degraded),
      "failed" => Some(Self::Failed),
      _ => None,
    }
  }

  /// Whether this outcome lets `/healthz` answer 200.
  ///
  /// `Degraded` is healthy on purpose: the probe exists to restart a
  /// daemon that stopped working, not one that is reporting a problem it
  /// survived.
  #[must_use]
  pub const fn is_healthy(self) -> bool {
    matches!(self, Self::Ok | Self::Degraded)
  }
}

impl std::fmt::Display for Outcome {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The six phases a tick can pass through, in the order they run.
///
/// The order is part of the contract: a tick that fetches, verifies,
/// resolves, builds and activates emits exactly these first five, in
/// this sequence.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
  /// Talk to the remote.
  Fetch,
  /// Check signatures against the trust root.
  Verify,
  /// Reconcile the working dir against the sources dir.
  Resolve,
  /// Realise the new generation.
  Build,
  /// Make the new generation current.
  Activate,
  /// Run an operator-supplied hook.
  Hook,
}

impl Phase {
  /// The five phases a full, successful tick emits, in order.
  pub const FULL_TICK: &'static [Self] = &[
    Self::Fetch,
    Self::Verify,
    Self::Resolve,
    Self::Build,
    Self::Activate,
  ];

  /// The wire spelling, matching the serde representation.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Fetch => "fetch",
      Self::Verify => "verify",
      Self::Resolve => "resolve",
      Self::Build => "build",
      Self::Activate => "activate",
      Self::Hook => "hook",
    }
  }

  /// Parse from the wire spelling.
  #[must_use]
  pub fn from_str_opt(raw: &str) -> Option<Self> {
    match raw {
      "fetch" => Some(Self::Fetch),
      "verify" => Some(Self::Verify),
      "resolve" => Some(Self::Resolve),
      "build" => Some(Self::Build),
      "activate" => Some(Self::Activate),
      "hook" => Some(Self::Hook),
      _ => None,
    }
  }
}

impl std::fmt::Display for Phase {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The three hook points, as a fixed enumeration.
///
/// Hook *names* come from operator config, so they can never become a
/// metric label: that would let a config file drive label cardinality.
/// The hook *point* is xi's own vocabulary, and that is what gets
/// labelled.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum HookPoint {
  /// Ran after the sources dir advanced to a new sha.
  OnAdvance,
  /// Ran after a successful build.
  OnBuild,
  /// Ran after activation.
  OnSwitch,
}

impl HookPoint {
  /// Every hook point, in the order metric labels enumerate them.
  pub const ALL: &'static [Self] =
    &[Self::OnAdvance, Self::OnBuild, Self::OnSwitch];

  /// The wire spelling, matching the serde representation.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::OnAdvance => "on-advance",
      Self::OnBuild => "on-build",
      Self::OnSwitch => "on-switch",
    }
  }
}

impl std::fmt::Display for HookPoint {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// Every field name a tick root span carries.
pub const TICK_FIELDS: &[&str] = &[
  "sources_id",
  "remote",
  "actor_name",
  "outcome",
  "duration_ms",
];

/// Every field name a phase child span carries.
pub const PHASE_FIELDS: &[&str] =
  &["phase", "outcome", "duration_ms", "old_sha", "new_sha"];

/// The root span of one daemon iteration.
///
/// Created at the top of a tick, finished at the bottom. `outcome` and
/// `duration_ms` are declared empty at creation and recorded by
/// [`TickSpan::finish`], so the span that reaches the sink on close
/// carries the full field set in one line rather than forcing a consumer
/// to join a "started" line against a "finished" line.
#[derive(Debug)]
pub struct TickSpan {
  span:    Span,
  started: Instant,
}

impl TickSpan {
  /// Open a tick root span for `sources_id` tracking `remote`.
  #[must_use]
  pub fn start(sources_id: &str, remote: &str) -> Self {
    let span = info_span!(
      TICK_SPAN_NAME,
      sources_id = sources_id,
      remote = remote,
      actor_name = DAEMON_ACTOR,
      outcome = Empty,
      duration_ms = Empty,
    );
    Self {
      span,
      started: Instant::now(),
    }
  }

  /// The underlying span, for entering it or attaching a child.
  #[must_use]
  pub const fn span(&self) -> &Span {
    &self.span
  }

  /// Open a phase child span under this tick.
  #[must_use]
  pub fn phase(&self, phase: Phase) -> PhaseSpan {
    let _guard = self.span.enter();
    PhaseSpan::start(phase)
  }

  /// How long the tick has been running.
  #[must_use]
  pub fn elapsed_ms(&self) -> u64 {
    elapsed_ms(self.started)
  }

  /// Record the outcome and duration, closing the span.
  ///
  /// Returns the duration it recorded so a caller can feed the same
  /// number to the planned `metrics` registry without measuring twice.
  #[must_use]
  pub fn finish(self, outcome: Outcome) -> u64 {
    let duration_ms = self.elapsed_ms();
    self.span.record("outcome", outcome.as_str());
    self.span.record("duration_ms", duration_ms);
    duration_ms
  }
}

/// One phase of a tick.
///
/// `old_sha` and `new_sha` are optional because most phases do not move
/// a ref; they are declared anyway so the key set does not vary by
/// phase.
#[derive(Debug)]
pub struct PhaseSpan {
  span:    Span,
  started: Instant,
}

impl PhaseSpan {
  /// Open a phase span. Prefer [`TickSpan::phase`], which parents it
  /// correctly.
  #[must_use]
  pub fn start(phase: Phase) -> Self {
    let span = info_span!(
      "xi.sources.phase",
      phase = phase.as_str(),
      outcome = Empty,
      duration_ms = Empty,
      old_sha = Empty,
      new_sha = Empty,
    );
    Self {
      span,
      started: Instant::now(),
    }
  }

  /// The underlying span, for entering it.
  #[must_use]
  pub const fn span(&self) -> &Span {
    &self.span
  }

  /// Record the ref movement this phase caused.
  ///
  /// Shas are git object ids, never credentials, so they are recorded
  /// verbatim.
  pub fn record_shas(&self, old_sha: Option<&str>, new_sha: Option<&str>) {
    if let Some(sha) = old_sha {
      self.span.record("old_sha", sha);
    }
    if let Some(sha) = new_sha {
      self.span.record("new_sha", sha);
    }
  }

  /// Record the outcome and duration, closing the span.
  #[must_use]
  pub fn finish(self, outcome: Outcome) -> u64 {
    let duration_ms = elapsed_ms(self.started);
    self.span.record("outcome", outcome.as_str());
    self.span.record("duration_ms", duration_ms);
    duration_ms
  }
}

/// Milliseconds since `started`, saturating rather than wrapping.
fn elapsed_ms(started: Instant) -> u64 {
  u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use super::*;

  #[test]
  fn outcome_round_trips_through_its_wire_spelling() {
    for outcome in Outcome::ALL {
      assert_eq!(Outcome::from_str_opt(outcome.as_str()), Some(*outcome));
    }
  }

  #[test]
  fn phase_round_trips_through_its_wire_spelling() {
    for phase in [
      Phase::Fetch,
      Phase::Verify,
      Phase::Resolve,
      Phase::Build,
      Phase::Activate,
      Phase::Hook,
    ] {
      assert_eq!(Phase::from_str_opt(phase.as_str()), Some(phase));
    }
  }

  #[test]
  fn unknown_spellings_do_not_parse() {
    assert_eq!(Outcome::from_str_opt("fine"), None);
    assert_eq!(Phase::from_str_opt("push"), None);
  }

  #[test]
  fn full_tick_is_the_first_five_phases_in_order() {
    assert_eq!(Phase::FULL_TICK, &[
      Phase::Fetch,
      Phase::Verify,
      Phase::Resolve,
      Phase::Build,
      Phase::Activate
    ]);
  }

  #[test]
  fn degraded_is_healthy_but_failed_is_not() {
    assert!(Outcome::Ok.is_healthy());
    assert!(Outcome::Degraded.is_healthy());
    assert!(!Outcome::Failed.is_healthy());
  }

  #[test]
  fn hook_points_are_a_fixed_enumeration() {
    let spellings: Vec<&str> =
      HookPoint::ALL.iter().map(|h| h.as_str()).collect();
    assert_eq!(spellings, vec!["on-advance", "on-build", "on-switch"]);
  }

  #[test]
  fn finish_reports_a_duration() {
    let tick = TickSpan::start("user:https://git.example/cfg", "origin");
    let ms = tick.finish(Outcome::Ok);
    assert!(ms < 60_000, "implausible tick duration: {ms}");
  }
}
