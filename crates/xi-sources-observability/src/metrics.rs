//! The closed Prometheus metric set.
//!
//! Every metric this crate exposes is declared once, here, in the
//! constructor: the set is closed, exactly the nine names the spec
//! lists, and nothing calling [`Metrics`] can register a tenth.
//! Labels are closed too - `outcome` only ever takes a
//! [`crate::fields::Outcome`] spelling and `hook` only a
//! [`crate::fields::HookPoint`] spelling, both fixed enumerations, so a
//! metric label can never become a vector for operator-controlled
//! cardinality (or, per [`crate::redact`], a vector for a credential:
//! a `with_label_values` call here only ever takes `as_str()` of a
//! closed enum, which [`crate::redact::lint`] does not need to special
//! case because there is no field name from this module on its banned
//! list).
//!
//! [`crate::redact::lint`]'s call-site matcher already watches
//! `.with_label_values(` and `.set(`/`.inc(`/`.observe(` call sites;
//! this module's own calls are exercised by
//! `tests::no_observable_call_in_the_workspace_names_a_credential` in
//! `tests/credential_lint.rs` same as every other crate under
//! `crates/`.
//!
//! What [`Metrics::encode`] produces, and whether anyone can reach it,
//! is `server`'s job (task 5.2/5.3, not this one) - this module only
//! owns the registry and the typed recording API.

use std::time::{Duration, SystemTime};

use prometheus::{
  CounterVec,
  Gauge,
  Histogram,
  HistogramOpts,
  Opts,
  Registry,
  TextEncoder,
};

use crate::fields::{HookPoint, Outcome};

/// Metric name constants, so `server`'s `/metrics` tests (and this
/// module's own) assert against one spelling rather than a
/// hand-copied string.
pub mod names {
  pub const FETCH_TOTAL: &str = "xi_sources_fetch_total";
  pub const FETCH_DURATION_SECONDS: &str = "xi_sources_fetch_duration_seconds";
  pub const SOURCES_HEAD_AGE_SECONDS: &str =
    "xi_sources_sources_head_age_seconds";
  pub const BUILD_DURATION_SECONDS: &str = "xi_sources_build_duration_seconds";
  pub const BUILD_TOTAL: &str = "xi_sources_build_total";
  pub const ACTIVATION_TOTAL: &str = "xi_sources_activation_total";
  pub const LAST_FAILURE_TIMESTAMP_SECONDS: &str =
    "xi_sources_last_failure_timestamp_seconds";
  pub const LOCK_WAIT_DURATION_SECONDS: &str =
    "xi_sources_lock_wait_duration_seconds";
  pub const HOOK_TOTAL: &str = "xi_sources_hook_total";

  /// Every metric name this crate exposes, in the order the spec lists
  /// them. `server`'s "every metric name listed above" scenario (5.3)
  /// asserts against this rather than re-typing the list.
  pub const ALL: &[&str] = &[
    FETCH_TOTAL,
    FETCH_DURATION_SECONDS,
    SOURCES_HEAD_AGE_SECONDS,
    BUILD_DURATION_SECONDS,
    BUILD_TOTAL,
    ACTIVATION_TOTAL,
    LAST_FAILURE_TIMESTAMP_SECONDS,
    LOCK_WAIT_DURATION_SECONDS,
    HOOK_TOTAL,
  ];
}

/// The daemon's metric registry and the one typed way to write to it.
#[derive(Debug, Clone)]
pub struct Metrics {
  registry: Registry,

  fetch_total: CounterVec,
  fetch_duration_seconds: Histogram,
  sources_head_age_seconds: Gauge,
  build_duration_seconds: Histogram,
  build_total: CounterVec,
  activation_total: CounterVec,
  last_failure_timestamp_seconds: Gauge,
  lock_wait_duration_seconds: Histogram,
  hook_total: CounterVec,
}

impl Metrics {
  /// Build a fresh registry with every metric registered.
  ///
  /// # Errors
  ///
  /// Returns a [`prometheus::Error`] if a metric's name or label set is
  /// malformed, or if two metrics collide - neither of which can
  /// happen with the fixed names and labels this constructor uses, but
  /// the underlying crate's API is fallible and this does not paper
  /// over that with an `expect`.
  pub fn new() -> Result<Self, prometheus::Error> {
    let registry = Registry::new();

    let fetch_total = CounterVec::new(
      Opts::new(names::FETCH_TOTAL, "Outcomes of fetching the remote."),
      &["outcome"],
    )?;
    let fetch_duration_seconds = Histogram::with_opts(HistogramOpts::new(
      names::FETCH_DURATION_SECONDS,
      "How long a fetch took.",
    ))?;
    let sources_head_age_seconds = Gauge::with_opts(Opts::new(
      names::SOURCES_HEAD_AGE_SECONDS,
      "Seconds since the last successful fetch.",
    ))?;
    let build_duration_seconds = Histogram::with_opts(HistogramOpts::new(
      names::BUILD_DURATION_SECONDS,
      "How long realising the new generation took.",
    ))?;
    let build_total = CounterVec::new(
      Opts::new(names::BUILD_TOTAL, "Outcomes of realising a generation."),
      &["outcome"],
    )?;
    let activation_total = CounterVec::new(
      Opts::new(names::ACTIVATION_TOTAL, "Outcomes of activating a generation."),
      &["outcome"],
    )?;
    let last_failure_timestamp_seconds = Gauge::with_opts(Opts::new(
      names::LAST_FAILURE_TIMESTAMP_SECONDS,
      "Unix timestamp of the most recent tick failure.",
    ))?;
    let lock_wait_duration_seconds = Histogram::with_opts(HistogramOpts::new(
      names::LOCK_WAIT_DURATION_SECONDS,
      "How long a tick waited to acquire the sources lock.",
    ))?;
    let hook_total = CounterVec::new(
      Opts::new(names::HOOK_TOTAL, "Outcomes of a lifecycle hook."),
      &["hook", "outcome"],
    )?;

    registry.register(Box::new(fetch_total.clone()))?;
    registry.register(Box::new(fetch_duration_seconds.clone()))?;
    registry.register(Box::new(sources_head_age_seconds.clone()))?;
    registry.register(Box::new(build_duration_seconds.clone()))?;
    registry.register(Box::new(build_total.clone()))?;
    registry.register(Box::new(activation_total.clone()))?;
    registry.register(Box::new(last_failure_timestamp_seconds.clone()))?;
    registry.register(Box::new(lock_wait_duration_seconds.clone()))?;
    registry.register(Box::new(hook_total.clone()))?;

    // A `*Vec` metric with no label combination touched yet emits
    // nothing at all when gathered - not even its HELP/TYPE header -
    // which is indistinguishable from "the exporter crashed before
    // registering this metric" to a scraper. Since every label value
    // here comes from a closed enum, there is no cardinality cost to
    // declaring every combination up front at zero.
    for outcome in Outcome::ALL {
      fetch_total.with_label_values(&[outcome.as_str()]);
      build_total.with_label_values(&[outcome.as_str()]);
      activation_total.with_label_values(&[outcome.as_str()]);
    }
    for hook in HookPoint::ALL {
      for outcome in Outcome::ALL {
        hook_total.with_label_values(&[hook.as_str(), outcome.as_str()]);
      }
    }

    Ok(Self {
      registry,
      fetch_total,
      fetch_duration_seconds,
      sources_head_age_seconds,
      build_duration_seconds,
      build_total,
      activation_total,
      last_failure_timestamp_seconds,
      lock_wait_duration_seconds,
      hook_total,
    })
  }

  /// The registry backing this metric set, for `server` to hand to a
  /// text encoder on every `/metrics` request.
  #[must_use]
  pub const fn registry(&self) -> &Registry {
    &self.registry
  }

  /// Record one fetch's outcome and duration.
  pub fn record_fetch(&self, outcome: Outcome, duration: Duration) {
    self.fetch_total.with_label_values(&[outcome.as_str()]).inc();
    self.fetch_duration_seconds.observe(duration.as_secs_f64());
  }

  /// Set how long it has been since the last successful fetch.
  pub fn set_sources_head_age(&self, age: Duration) {
    self.sources_head_age_seconds.set(age.as_secs_f64());
  }

  /// Record one build's outcome and duration.
  pub fn record_build(&self, outcome: Outcome, duration: Duration) {
    self.build_total.with_label_values(&[outcome.as_str()]).inc();
    self.build_duration_seconds.observe(duration.as_secs_f64());
  }

  /// Record one activation's outcome.
  pub fn record_activation(&self, outcome: Outcome) {
    self.activation_total.with_label_values(&[outcome.as_str()]).inc();
  }

  /// Record that a tick failed at `when`.
  ///
  /// Takes the instant rather than reading the clock itself, so a
  /// caller recording a failure discovered after the fact (replaying a
  /// crash) can backdate it, and so this method stays trivially
  /// testable with a fixed time.
  ///
  /// # Panics
  ///
  /// Never in practice: `when` is only ever before `UNIX_EPOCH` if the
  /// system clock is set earlier than 1970, which is not a
  /// configuration this daemon can run under at all.
  pub fn record_failure_at(&self, when: SystemTime) {
    #[expect(
      clippy::unwrap_used,
      reason = "see the doc comment: not reachable on a real clock"
    )]
    let seconds = when.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs_f64();
    self.last_failure_timestamp_seconds.set(seconds);
  }

  /// Record how long a tick waited to acquire the sources lock.
  pub fn record_lock_wait(&self, duration: Duration) {
    self.lock_wait_duration_seconds.observe(duration.as_secs_f64());
  }

  /// Record one lifecycle hook's outcome.
  pub fn record_hook(&self, hook: HookPoint, outcome: Outcome) {
    self
      .hook_total
      .with_label_values(&[hook.as_str(), outcome.as_str()])
      .inc();
  }

  /// Render the registry in Prometheus text exposition format.
  ///
  /// # Errors
  ///
  /// Returns a [`prometheus::Error`] if encoding fails, which the
  /// underlying crate documents as only happening on a write error -
  /// unreachable against the in-memory buffer this always encodes to,
  /// but the signature stays honest about the fallible API beneath it.
  pub fn encode(&self) -> Result<String, prometheus::Error> {
    let encoder = TextEncoder::new();
    let families = self.registry.gather();
    let mut buf = String::new();
    encoder.encode_utf8(&families, &mut buf)?;
    Ok(buf)
  }
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use std::time::Duration;

  use super::*;

  #[test]
  fn every_metric_in_the_spec_list_is_registered() {
    let metrics = Metrics::new().expect("metrics must register");
    let encoded = metrics.encode().expect("must encode");
    for name in names::ALL {
      assert!(
        encoded.contains(name),
        "{name} missing from encoded output:\n{encoded}"
      );
    }
  }

  #[test]
  fn fetch_total_is_labelled_by_the_closed_outcome_set() {
    let metrics = Metrics::new().expect("metrics must register");
    metrics.record_fetch(Outcome::Ok, Duration::from_millis(50));
    metrics.record_fetch(Outcome::Failed, Duration::from_millis(10));

    let encoded = metrics.encode().expect("must encode");
    assert!(encoded.contains("xi_sources_fetch_total{outcome=\"ok\"} 1"));
    assert!(encoded.contains("xi_sources_fetch_total{outcome=\"failed\"} 1"));
  }

  #[test]
  fn hook_total_carries_both_labels() {
    let metrics = Metrics::new().expect("metrics must register");
    metrics.record_hook(crate::fields::HookPoint::OnSwitch, Outcome::Degraded);

    let encoded = metrics.encode().expect("must encode");
    assert!(encoded.contains(
      "xi_sources_hook_total{hook=\"on-switch\",outcome=\"degraded\"} 1"
    ));
  }

  #[test]
  fn gauges_report_the_last_value_set() {
    let metrics = Metrics::new().expect("metrics must register");
    metrics.set_sources_head_age(Duration::from_secs(120));
    metrics.record_failure_at(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));

    let encoded = metrics.encode().expect("must encode");
    assert!(encoded.contains("xi_sources_sources_head_age_seconds 120"));
    assert!(
      encoded.contains("xi_sources_last_failure_timestamp_seconds 1700000000")
    );
  }

  #[test]
  fn histograms_record_observations() {
    let metrics = Metrics::new().expect("metrics must register");
    metrics.record_lock_wait(Duration::from_millis(250));

    let encoded = metrics.encode().expect("must encode");
    assert!(encoded.contains("xi_sources_lock_wait_duration_seconds_count 1"));
  }

  #[test]
  fn a_fresh_registry_still_exposes_every_metric_name_with_zero_values() {
    // Prometheus best practice (and this spec's "lists every metric
    // name" scenario): a counter or histogram with no observations
    // yet must still appear in `/metrics`, because a scraper that only
    // sees metrics after their first event cannot distinguish "zero"
    // from "the exporter is down".
    let metrics = Metrics::new().expect("metrics must register");
    let encoded = metrics.encode().expect("must encode");
    assert!(encoded.contains("xi_sources_build_duration_seconds_count 0"));
  }
}
