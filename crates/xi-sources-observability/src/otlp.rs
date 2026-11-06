//! Opt-in OpenTelemetry trace export.
//!
//! An export target is an operator decision - the default config has no
//! `xi.sources.observability.otlp.endpoint`, and this whole module (plus
//! its dependencies: `opentelemetry`, `opentelemetry_sdk`,
//! `opentelemetry-otlp`, `tracing-opentelemetry`, and the blocking HTTP
//! client they export over) only links in behind the `otlp` cargo
//! feature. [`crate::config::OtlpConfig`] is the one way an operator
//! names an endpoint; this module is the one way that config becomes a
//! [`tracing_subscriber::Layer`] a daemon can stack next to
//! [`crate::subscriber::ndjson_layer`].
//!
//! Task 6.1 built the exporter pipeline behind its feature flag,
//! already capped at the spec's 5-second batch latency ceiling
//! ([`crate::config::OtlpConfig::new`]). Task 6.2, this module's
//! [`FailureCounterLayer`], closes the remaining piece spec.md's
//! "Opt-in OTLP export" requirement names: "export failures MUST NOT
//! block the tick loop; they MUST be logged and counted via
//! `xi_sources_otlp_export_failures_total`". The "not block the tick
//! loop" half is structural - [`opentelemetry_sdk::trace::BatchSpanProcessor`]
//! exports on its own background thread, so nothing in a tick's own
//! call stack ever waits on an export, success or failure. The
//! "logged and counted" half needs active wiring: an export failure
//! surfaces only as the SDK's own internal `otel_error!` log event
//! (gated behind this crate's `internal-logs` feature on the
//! `opentelemetry` dependency - see `Cargo.toml`), never as a value
//! this module's code receives directly, so [`FailureCounterLayer`] is
//! a `tracing_subscriber` layer that watches for that specific event
//! and increments whatever counter
//! [`crate::metrics::Metrics::register_otlp_export_failures`] handed
//! it.
//!
//! # Why the blocking HTTP transport
//!
//! Nothing else in this crate runs an async executor, and pulling in
//! `tokio` for one optional feature would be a cost every consumer of
//! `otlp` pays even if their collector never changes. `opentelemetry_sdk`
//! 0.33's batch span processor runs its own dedicated worker thread
//! rather than requiring one -
//! `experimental_trace_batch_span_processor_with_async_runtime` is an opt-in
//! feature this crate does not enable - so the blocking `reqwest` client
//! (already a dependency, gated the same way) is sufficient.

use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::trace::{
  BatchConfigBuilder,
  BatchSpanProcessor,
  SdkTracerProvider,
  Tracer,
};
use tracing_subscriber::Registry;

use crate::config::OtlpConfig;

/// Everything that went wrong building the OTLP export stack.
#[derive(Debug, thiserror::Error)]
pub enum OtlpError {
  /// The exporter could not be constructed for `endpoint`.
  #[error("failed to build the OTLP span exporter for '{endpoint}': {source}")]
  Exporter {
    /// The endpoint that was being configured.
    endpoint: String,
    /// What `opentelemetry-otlp` said.
    #[source]
    source:   opentelemetry_otlp::ExporterBuildError,
  },
}

/// A built tracer provider plus the tracer [`Trace::layer`] wraps on
/// request.
///
/// Holds the provider because it owns the exporter's background worker
/// thread: a caller must keep `Trace` alive for the process lifetime
/// and call [`Trace::shutdown`] on the way out so the final batch
/// flushes instead of being dropped mid-queue.
pub struct Trace {
  provider: SdkTracerProvider,
  tracer:   Tracer,
}

impl std::fmt::Debug for Trace {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    // Neither `SdkTracerProvider` nor `Tracer` implement `Debug`; name
    // the type without pretending to show its live state.
    f.write_str("Trace { .. }")
  }
}

impl Trace {
  /// Build the exporter, the batching processor (capped at
  /// `config.batch_max_latency`, which [`OtlpConfig::new`] already caps
  /// at the spec's 5-second ceiling), and the tracer provider, then
  /// wrap it as a layer ready to stack on a [`Registry`].
  ///
  /// # Errors
  ///
  /// Returns [`OtlpError::Exporter`] if the exporter cannot be built
  /// for `config.endpoint` - a malformed URL, mainly, since this does
  /// not connect to the collector yet; nothing about building the
  /// export pipeline requires the collector to be reachable.
  pub fn build(config: &OtlpConfig) -> Result<Self, OtlpError> {
    let exporter = SpanExporter::builder()
      .with_http()
      .with_protocol(Protocol::HttpBinary)
      .with_endpoint(&config.endpoint)
      .build()
      .map_err(|source| {
        OtlpError::Exporter {
          endpoint: config.endpoint.clone(),
          source,
        }
      })?;

    let batch_config = BatchConfigBuilder::default()
      .with_scheduled_delay(config.batch_max_latency)
      .build();
    let processor = BatchSpanProcessor::builder(exporter)
      .with_batch_config(batch_config)
      .build();

    let provider = SdkTracerProvider::builder()
      .with_span_processor(processor)
      .build();
    let tracer = provider.tracer("xi-sources");

    Ok(Self { provider, tracer })
  }

  /// The layer to stack on the daemon's [`Registry`], next to
  /// [`crate::subscriber::ndjson_layer`].
  ///
  /// Built fresh on each call rather than stored, since
  /// `OpenTelemetryLayer` implements neither `Clone` nor `Debug`
  /// (unlike the `Tracer` it wraps, which this struct does keep
  /// around) - cheap either way, as it borrows the same underlying
  /// tracer rather than opening a new export pipeline.
  #[must_use]
  pub fn layer(
    &self,
  ) -> tracing_opentelemetry::OpenTelemetryLayer<Registry, Tracer> {
    tracing_opentelemetry::layer().with_tracer(self.tracer.clone())
  }

  /// Flush any queued spans and stop the export worker thread.
  ///
  /// # Errors
  ///
  /// Propagates the SDK's shutdown error, which it documents as
  /// meaning the final flush could not complete - the spec's "export
  /// failures must not block the tick loop" is about a single export,
  /// not shutdown, so a caller on the way out should log this rather
  /// than treat it as fatal.
  pub fn shutdown(self) -> Result<(), opentelemetry_sdk::error::OTelSdkError> {
    self.provider.shutdown()
  }
}

/// How long [`Trace::build`] waits before giving up on an export - not
/// used by this module directly, but the ceiling
/// [`crate::config::MAX_OTLP_BATCH_LATENCY`] enforces on
/// `batch_max_latency`, restated here for anyone reading this module in
/// isolation.
pub const SPEC_BATCH_LATENCY_CEILING: Duration =
  crate::config::MAX_OTLP_BATCH_LATENCY;

/// The vendored SDK's own event name for a failed batch export.
///
/// Sourced from `opentelemetry_sdk`'s `span_processor.rs`
/// (`BatchSpanProcessor::export_batch`'s `otel_error!(name:
/// "BatchSpanProcessor.ExportError", ...)` call) - the one stable
/// surface the SDK's internal logging exposes for this, since the crate
/// does not otherwise return or call back on an export failure.
const EXPORT_ERROR_EVENT_NAME: &str = "BatchSpanProcessor.ExportError";

/// A `tracing_subscriber` layer that counts OTLP export failures.
///
/// Stack this on the same [`tracing_subscriber::Registry`] as
/// [`Trace::layer`] (order does not matter: this layer only reacts to
/// the SDK's own internal log event, not to anything
/// [`tracing_opentelemetry::OpenTelemetryLayer`] does). Decoupled from
/// [`crate::metrics::Metrics`] by taking a plain callback rather than a
/// `Metrics` reference: this module's `otlp` feature does not depend on
/// `daemon` (so `Metrics` may not even be compiled in), and the only
/// thing this layer needs to do on a match is increment *something*.
pub struct FailureCounterLayer {
  on_failure: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for FailureCounterLayer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("FailureCounterLayer { .. }")
  }
}

impl FailureCounterLayer {
  /// Call `on_failure` every time an export-failure event is observed.
  ///
  /// A typical `on_failure` is `move || counter.inc()` for the
  /// [`prometheus::Counter`] handle
  /// [`crate::metrics::Metrics::register_otlp_export_failures`] returns.
  pub fn new(on_failure: impl Fn() + Send + Sync + 'static) -> Self {
    Self {
      on_failure: Box::new(on_failure),
    }
  }
}

impl<S> tracing_subscriber::Layer<S> for FailureCounterLayer
where
  S: tracing::Subscriber,
{
  fn on_event(
    &self,
    event: &tracing::Event<'_>,
    _ctx: tracing_subscriber::layer::Context<'_, S>,
  ) {
    if event.metadata().name() == EXPORT_ERROR_EVENT_NAME {
      (self.on_failure)();
    }
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
  use super::*;

  #[test]
  fn builds_a_layer_without_a_reachable_collector() {
    // Building the exporter and the batch processor does not connect to
    // anything: the worker thread only talks to the collector when it
    // has a batch to flush. A fake, well-formed endpoint is enough to
    // prove the pipeline assembles.
    let config = OtlpConfig::new("http://127.0.0.1:1/v1/traces".to_owned());
    let trace = Trace::build(&config).expect("exporter must build offline");
    let _layer = trace.layer();
    trace
      .shutdown()
      .expect("shutdown must flush cleanly with nothing queued");
  }

  #[test]
  fn a_malformed_endpoint_is_reported_as_an_exporter_error() {
    // Not a syntactically valid URI at all (an embedded space), as
    // opposed to an endpoint `OtlpConfig::new` would accept but a
    // collector would reject - this exercises the exporter builder's
    // own validation, not `config::validate_otlp_endpoint`'s (which
    // runs earlier, at config-load time, and is not this test's
    // concern).
    let config = OtlpConfig::new("http://not a valid uri/v1/traces".to_owned());
    let err =
      Trace::build(&config).expect_err("a malformed URI must fail to build");
    assert!(matches!(err, OtlpError::Exporter { .. }));
    assert!(err.to_string().contains("OTLP span exporter"));
  }

  #[test]
  fn the_batch_latency_ceiling_matches_the_spec_cap() {
    assert_eq!(SPEC_BATCH_LATENCY_CEILING, Duration::from_secs(5));
  }

  // -- FailureCounterLayer: matching logic ---------------------------------

  #[test]
  fn an_export_error_event_increments_the_callback() {
    use std::sync::{
      Arc,
      atomic::{AtomicUsize, Ordering},
    };

    use tracing_subscriber::layer::SubscriberExt as _;

    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    let layer = FailureCounterLayer::new(move || {
      counted.fetch_add(1, Ordering::SeqCst);
    });
    let subscriber = tracing_subscriber::Registry::default().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    // The exact shape `opentelemetry_sdk`'s `otel_error!` macro expands
    // to: a static event name set via `name:`, matching
    // `EXPORT_ERROR_EVENT_NAME`.
    tracing::error!(
      name: "BatchSpanProcessor.ExportError",
      error = "connection refused"
    );
    // An unrelated event, including one that merely mentions the same
    // text in its message, must not be mistaken for a match.
    tracing::error!("BatchSpanProcessor.ExportError happened, allegedly");
    tracing::info!(
      name: "BatchSpanProcessor.ExportingDueToForceFlush",
      "flushing"
    );

    assert_eq!(count.load(Ordering::SeqCst), 1);
  }

  // The end-to-end proof - a real `Trace` built against an unreachable
  // endpoint, flushed via `Trace::shutdown`, observed through
  // `FailureCounterLayer` installed as the process-global default so
  // the SDK's background export thread is covered too - needs a
  // process-global subscriber, which this module's other tests must
  // not share a process with (each sets its own thread-local default;
  // a stray global default would outlive any one test and could shadow
  // a later test's thread-local one on the same thread, or simply fail
  // to install a second time). It lives in its own file,
  // `tests/bdd_sources_otlp_export.rs`, which cargo gives its own
  // process the way every `tests/*.rs` file does.
}
