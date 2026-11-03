//! BDD: a failed OTLP export is logged and counted, not swallowed.
//!
//! Mirrors `openspec/specs/xi-sources-observability/spec.md`'s "Opt-in
//! OTLP export" requirement: "Export failures MUST NOT block the tick
//! loop; they MUST be logged and counted via
//! `xi_sources_otlp_export_failures_total`".
//!
//! This is the one test in the crate that needs a process-global
//! `tracing` subscriber - [`opentelemetry_sdk`]'s `BatchSpanProcessor`
//! exports on its own background thread, which has no thread-local
//! subscriber override of its own and so only ever sees the global
//! default. [`tracing::subscriber::set_global_default`] can only
//! succeed once per process, which is exactly why this lives alone in
//! its own `tests/*.rs` file: cargo gives every integration-test file
//! its own process, so this is the only call to it anywhere in this
//! crate's test suite.
//!
//! "MUST NOT block the tick loop" is structural, not something this
//! test exercises directly: the export happens on a background thread
//! by construction, so nothing on the calling thread's stack can
//! observe it blocking. What this test proves is the half that *is*
//! active wiring: the failure reaches both a `tracing` event (the "MUST
//! be logged" half, covered by the global subscriber receiving the SDK's
//! own internal log) and the Prometheus counter (the "MUST be counted"
//! half).

#![expect(clippy::expect_used, reason = "Fine in tests")]

use tracing_subscriber::layer::SubscriberExt as _;
use xi_sources_observability::{
  config::OtlpConfig,
  metrics::{Metrics, names},
  otlp::{FailureCounterLayer, Trace},
};

#[test]
fn a_failed_export_is_logged_and_counted() {
  let metrics = Metrics::new().expect("metrics must register");
  let failure_counter = metrics
    .register_otlp_export_failures()
    .expect("must register the OTLP failure counter");

  // Port 1 is a privileged port nothing in this sandbox listens on;
  // connection is refused immediately rather than timing out, keeping
  // the test fast.
  let config = OtlpConfig::new("http://127.0.0.1:1/v1/traces".to_owned());
  let trace = Trace::build(&config).expect("exporter must build offline");

  let failure_layer = FailureCounterLayer::new(move || failure_counter.inc());
  // `trace.layer()` in the same stack as the failure layer: a span
  // opened and closed under this subscriber is what actually reaches
  // the SDK's processor (its `on_end` queues the span for export) -
  // without it, `shutdown` below would have nothing queued and would
  // skip the export call entirely (`get_spans_and_export` is a no-op
  // when its target batch size is zero), proving nothing.
  let subscriber = tracing_subscriber::Registry::default()
    .with(trace.layer())
    .with(failure_layer);
  tracing::subscriber::set_global_default(subscriber)
    .expect("this is the only test in this process installing a global default");

  {
    let span = tracing::info_span!("xi.sources.tick", sources_id = "user:x");
    let _entered = span.enter();
    // Closes (and is handed to the OTel layer's `on_end`, which queues
    // it with the processor) when `_entered` and `span` drop here.
  }

  // Drains and exports the now-queued span before returning - see
  // `opentelemetry_sdk`'s `span_processor.rs`, `BatchMessage::Shutdown`
  // handling, which calls `get_spans_and_export` (and thus attempts the
  // HTTP POST, and thus logs `BatchSpanProcessor.ExportError` on
  // failure) synchronously before replying to the shutdown call.
  // `shutdown` itself returns `Ok(())` even though the export it drove
  // failed: shutdown's own contract is "did the processor shut down
  // cleanly", not "did every queued span make it out" - which is
  // exactly why the spec needs a counter in the first place, rather
  // than relying on a caller noticing a non-error return.
  let _ = trace.shutdown();

  let encoded = metrics.encode().expect("must encode");
  assert!(
    encoded.contains(&format!("{} 1", names::OTLP_EXPORT_FAILURES_TOTAL)),
    "the OTLP export-failure counter must have been incremented exactly \
     once by a shutdown against an unreachable collector:\n{encoded}"
  );
}
