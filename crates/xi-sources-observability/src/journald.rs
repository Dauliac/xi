//! The system-scope log sink: `tracing-journald`, not a file.
//!
//! A system-scope daemon runs under systemd, and systemd already owns
//! log retention, rotation, and the `journalctl -u xi-sources-daemon`
//! readout an operator reaches for - duplicating that with
//! [`crate::rotating`] would fight the platform rather than use it. The
//! unit this module pairs with sets `StandardOutput=journal` (the Nix
//! module that writes that unit is `xi-sources-nix-modules`, not this
//! crate), and [`layer`] additionally writes each span's and event's
//! fields as *structured* journal fields via the native socket, so
//! `journalctl -o json` exposes `SOURCES_ID=`, `PHASE=`, `OUTCOME=` and
//! so on as queryable keys rather than text an operator has to grep.
//!
//! # A gap this module used to have
//!
//! [`tracing_journald::Layer`] has no `on_close` hook: unlike
//! [`crate::subscriber`]'s `fmt` layer (which could synthesize a "close"
//! event from [`FmtSpan::CLOSE`](tracing_subscriber::fmt::format::FmtSpan::CLOSE)
//! and feed it to *itself*, never to a sibling layer - see
//! `tracing-subscriber`'s `fmt_layer.rs`, `on_close`), journald only
//! ever logs a real [`tracing::Event`]. It *does* attach a span's
//! recorded fields (including ones recorded after the span opened, such
//! as [`crate::fields::PhaseSpan::finish`]'s `outcome` and
//! `duration_ms`) to any event logged while that span is still entered,
//! because `on_record` mutates the span's field buffer in place.
//!
//! [`crate::fields::TickSpan::finish`] and [`crate::fields::PhaseSpan::finish`]
//! now emit an explicit `"close"` event while still entered (task 2.2;
//! see `fields`'s top doc comment), closing the gap: a real event, not a
//! layer-private synthesis, reaches `journald::layer` the same way it
//! reaches [`crate::subscriber::ndjson_layer`]. This module's test suite
//! (see `a_span_field_recorded_before_an_in_scope_event_reaches_journald`
//! and `finish_s_close_event_carries_its_recorded_fields_to_a_sibling_layer`)
//! demonstrates the mechanism and proves it against `fields` directly.

use std::io;

use tracing_journald::Layer;

use crate::config::LogLevel;

/// Build the journald layer, filtered to `level` and above.
///
/// # Errors
///
/// Returns the I/O error from opening journald's native socket -
/// typically "no such file or directory" on a machine with no
/// `systemd-journald` running, which is any non-systemd host and most
/// containers. A caller that cannot open this layer has no system-scope
/// sink and should fail startup rather than silently drop every log
/// line; it is not this module's place to decide that policy, so the
/// error is simply returned.
pub fn layer<S>(
  level: LogLevel,
) -> io::Result<
  tracing_subscriber::filter::Filtered<
    Layer,
    tracing_subscriber::filter::LevelFilter,
    S,
  >,
>
where
  S: tracing::Subscriber
    + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
  use tracing_subscriber::Layer as _;

  let filter =
    tracing_subscriber::filter::LevelFilter::from_level(level.to_tracing());
  Ok(tracing_journald::layer()?.with_filter(filter))
}

/// RAII handle for a journald subscriber installed as this thread's
/// default. See [`crate::subscriber::Guard`] for why thread-local
/// rather than global.
#[derive(Debug)]
pub struct Guard(
  #[expect(dead_code, reason = "held for its Drop")]
  tracing::subscriber::DefaultGuard,
);

/// Install the journald subscriber for `level` as this thread's
/// default.
///
/// # Errors
///
/// Propagates [`layer`]'s socket error.
pub fn init(level: LogLevel) -> io::Result<Guard> {
  use tracing_subscriber::layer::SubscriberExt as _;

  let subscriber =
    tracing_subscriber::Registry::default().with(layer(level)?);
  Ok(Guard(tracing::subscriber::set_default(subscriber)))
}

/// Whether this host has a running `systemd-journald` to write to.
///
/// Tests gate on this rather than asserting a socket always exists:
/// most CI sandboxes and every non-systemd machine do not have one, and
/// a test that required it would not be a test anyone could run.
#[must_use]
pub fn journald_available() -> bool {
  std::path::Path::new("/run/systemd/journal/socket").exists()
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use tracing::info;
  use tracing_subscriber::layer::SubscriberExt as _;

  use super::*;

  #[test]
  fn layer_construction_fails_cleanly_without_a_journald_socket() {
    if journald_available() {
      // This host can actually open the socket; that path is covered
      // by `init_succeeds_against_a_real_journald` instead.
      return;
    }
    let err = tracing_journald::layer()
      .err()
      .expect("no journald socket on this host, so opening it must fail");
    // Any I/O error is acceptable; the point is that it is `Err`, not
    // a specific kind, since the failure mode varies by platform
    // (missing socket file vs. connection refused vs. permission).
    drop(err);
  }

  #[test]
  fn init_succeeds_against_a_real_journald() {
    if !journald_available() {
      return;
    }
    let guard = init(LogLevel::Info).expect("journald socket is present");
    tracing::info!(sources_id = "user:x", "xi-sources-observability journald smoke test");
    drop(guard);
  }

  /// Proves the mechanism `journald::layer` depends on: a field
  /// recorded on a span *after* it opened is visible to an event
  /// logged while that span is still entered. This is the shape
  /// `fields::TickSpan`/`PhaseSpan` will need to adopt (an in-scope
  /// terminal event) for their `outcome`/`duration_ms` to reach
  /// journald - see this module's doc comment.
  ///
  /// Exercised against the in-memory capture layer rather than real
  /// journald, so it runs everywhere `journald_available` is false.
  #[test]
  fn a_span_field_recorded_before_an_in_scope_event_reaches_journald() {
    use crate::subscriber::capture::CaptureWriter;

    let writer = CaptureWriter::new();
    let fmt_layer = tracing_subscriber::fmt::layer()
      .json()
      .flatten_event(true)
      .with_current_span(true)
      .with_span_list(false)
      .with_writer(writer.clone());
    let subscriber = tracing_subscriber::Registry::default().with(fmt_layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    let span = tracing::info_span!(
      "xi.sources.phase",
      phase = "fetch",
      outcome = tracing::field::Empty
    );
    {
      let _entered = span.enter();
      span.record("outcome", "ok");
      // Logged while still inside the span: a real journald layer's
      // `on_event` would see `outcome` merged in from the span's
      // extension buffer, exactly like this capture does.
      info!("phase finished");
    }

    let lines = writer.json_lines().expect("captured output must be ndjson");
    assert_eq!(lines.len(), 1, "got: {lines:?}");
    assert_eq!(lines[0]["message"], "phase finished");
    assert_eq!(lines[0]["span"]["outcome"], "ok");
    assert_eq!(lines[0]["span"]["phase"], "fetch");
  }

  /// The gap this module's doc comment used to describe, closed: a
  /// real [`crate::fields::PhaseSpan::finish`] call (not a hand-rolled
  /// event standing in for it) reaches a layer with no `on_close` hook,
  /// carrying `outcome` and `duration_ms` - the exact fields a
  /// journald-backed system-scope daemon would otherwise have lost.
  ///
  /// Exercised against the in-memory capture layer (same reasoning as
  /// the test above: this must run everywhere, not only where a real
  /// journald socket exists), stacked through
  /// [`crate::subscriber::ndjson_layer`] so it is the real production
  /// layer composition, not a hand-rolled approximation of it.
  #[test]
  fn finish_s_close_event_carries_its_recorded_fields_to_a_sibling_layer() {
    use crate::{
      fields::{Outcome, Phase, PhaseSpan},
      subscriber::{capture::CaptureWriter, ndjson_layer},
    };

    let writer = CaptureWriter::new();
    let subscriber =
      tracing_subscriber::Registry::default().with(ndjson_layer(writer.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);

    let phase = PhaseSpan::start(Phase::Verify);
    let _ = phase.finish(Outcome::Ok);

    let lines = writer.json_lines().expect("captured output must be ndjson");
    let closes: Vec<_> =
      lines.iter().filter(|l| l["message"] == "close").collect();
    assert_eq!(closes.len(), 1, "got: {lines:?}");
    assert_eq!(closes[0]["span"]["phase"], "verify");
    assert_eq!(closes[0]["span"]["outcome"], "ok");
    assert!(closes[0]["span"]["duration_ms"].is_number());
  }
}
