//! The ndjson tracing stack every scope's daemon starts once.
//!
//! One layer, three things decide where its output lands: the scope
//! ([`crate::rotating`] for user, `journald` for system - see the
//! `journald` module), a test harness ([`capture`]), or nothing at all
//! (a daemon with logging disabled, which is not a configuration this
//! crate offers - `info` is the floor).
//!
//! The layer always emits ndjson. [`crate::fields::TickSpan`] and
//! [`crate::fields::PhaseSpan`] declare their fields up
//! front and record them before closing; this module's job is only to
//! turn "a span closed" into one JSON line carrying those fields, via
//! [`tracing_subscriber::fmt::format::FmtSpan::CLOSE`]. A plain
//! `info!("message")` call still produces its own line the same way
//! every `tracing_subscriber::fmt` consumer expects.

use tracing_subscriber::{
  Layer as _,
  Registry,
  fmt::{self, MakeWriter, format::FmtSpan},
  layer::SubscriberExt as _,
};

use crate::config::LogLevel;

/// Build the ndjson formatting layer every scope's subscriber stacks on
/// top of a [`Registry`].
///
/// Span life cycle only ever surfaces at `CLOSE`: an "open" line would
/// double the volume of every tick for no benefit, because
/// [`crate::fields::TickSpan`] and [`crate::fields::PhaseSpan`] declare
/// their fields as [`tracing::field::Empty`] until the phase is actually done, so an
/// open-time line would carry half the field set anyway.
pub fn ndjson_layer<W>(
  writer: W,
) -> impl tracing_subscriber::Layer<Registry> + Send + Sync + 'static
where
  W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
  fmt::layer()
    .json()
    .flatten_event(true)
    .with_current_span(true)
    .with_span_list(false)
    .with_span_events(FmtSpan::CLOSE)
    .with_writer(writer)
}

/// Build the registry+ndjson subscriber for `writer`, filtered to
/// `level` and above.
///
/// Not installed as the process default here: [`init_with_writer`]
/// does that. This is the piece a scope-specific `init_*` (user,
/// system, or a test) stacks further layers onto, such as the
/// `journald` layer which has its own writer and does not go through
/// [`MakeWriter`] at all.
pub fn build<W>(
  level: LogLevel,
  writer: W,
) -> impl tracing::Subscriber + Send + Sync + 'static
where
  W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
  let filter =
    tracing_subscriber::filter::LevelFilter::from_level(level.to_tracing());
  Registry::default().with(ndjson_layer(writer).with_filter(filter))
}

/// RAII handle for a subscriber installed as the *thread-local* default.
///
/// Thread-local rather than global ([`tracing::subscriber::set_global_default`])
/// on purpose: a test that calls [`init_with_writer`] must not leak its
/// subscriber into a sibling test running on another thread of the same
/// `cargo test` binary. A real daemon process installs exactly one
/// subscriber for its own lifetime, so the distinction does not matter
/// there - the guard simply outlives `main`.
#[derive(Debug)]
pub struct Guard(#[expect(dead_code, reason = "held for its Drop")] tracing::subscriber::DefaultGuard);

/// Install the ndjson subscriber for `writer` as this thread's default.
///
/// # Panics
///
/// Never panics by itself; `tracing`'s own `set_default` does not
/// either. A caller wanting the *global* default (an actual daemon
/// process) should use [`tracing::subscriber::set_global_default`]
/// directly with [`build`] instead of this helper.
#[must_use]
pub fn init_with_writer<W>(level: LogLevel, writer: W) -> Guard
where
  W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
  let subscriber = build(level, writer);
  Guard(tracing::subscriber::set_default(subscriber))
}

/// An in-memory [`MakeWriter`], for tests that need to read back what
/// the ndjson layer wrote without touching a file or journald.
pub mod capture {
  use std::{
    io,
    sync::{Arc, Mutex},
  };

  /// A writer every clone shares, so the layer's per-event writer and
  /// the test's reader see the same bytes.
  #[derive(Debug, Clone, Default)]
  pub struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

  impl CaptureWriter {
    /// A fresh, empty capture buffer.
    #[must_use]
    pub fn new() -> Self {
      Self::default()
    }

    /// Every captured line, in write order, as decoded UTF-8.
    ///
    /// Trailing empty lines from the final newline are dropped, so a
    /// caller gets exactly one string per ndjson record.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
      let guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      String::from_utf8_lossy(&guard)
        .lines()
        .map(ToOwned::to_owned)
        .collect()
    }

    /// Every captured line, parsed as JSON.
    ///
    /// # Errors
    ///
    /// Returns the first parse error, which would mean the layer under
    /// test stopped emitting ndjson.
    pub fn json_lines(&self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
      self.lines().iter().map(|line| serde_json::from_str(line)).collect()
    }
  }

  /// One write borrow of the shared buffer.
  #[derive(Debug)]
  pub struct CaptureHandle(Arc<Mutex<Vec<u8>>>);

  impl io::Write for CaptureHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
      self
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend_from_slice(buf);
      Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureHandle;

    fn make_writer(&'a self) -> Self::Writer {
      CaptureHandle(Arc::clone(&self.0))
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
  use capture::CaptureWriter;
  use tracing::info;

  use super::*;
  use crate::fields::{Outcome, Phase, TickSpan};

  #[test]
  fn an_ordinary_log_line_is_one_ndjson_record() {
    let writer = CaptureWriter::new();
    let _guard = init_with_writer(LogLevel::Info, writer.clone());

    info!(sources_id = "user:x", "hello");

    let lines = writer.json_lines().expect("captured output must be ndjson");
    assert_eq!(lines.len(), 1, "got: {lines:?}");
    assert_eq!(lines[0]["message"], "hello");
    assert_eq!(lines[0]["sources_id"], "user:x");
    assert_eq!(lines[0]["level"], "INFO");
  }

  #[test]
  fn closing_a_tick_span_emits_its_recorded_fields() {
    let writer = CaptureWriter::new();
    let _guard = init_with_writer(LogLevel::Info, writer.clone());

    let tick = TickSpan::start("user:https://git.example/cfg", "origin");
    {
      let phase = tick.phase(Phase::Fetch);
      let _ = phase.finish(Outcome::Ok);
    }
    let _ = tick.finish(Outcome::Ok);

    let lines = writer.json_lines().expect("captured output must be ndjson");
    let closes: Vec<_> = lines
      .iter()
      .filter(|line| line["message"] == "close")
      .collect();
    assert_eq!(closes.len(), 2, "got: {lines:?}");

    let phase_close = &closes[0]["span"];
    assert_eq!(phase_close["name"], "xi.sources.phase");
    assert_eq!(phase_close["phase"], "fetch");
    assert_eq!(phase_close["outcome"], "ok");
    assert!(phase_close["duration_ms"].is_number());

    let tick_close = &closes[1]["span"];
    assert_eq!(tick_close["name"], "xi.sources.tick");
    assert_eq!(tick_close["sources_id"], "user:https://git.example/cfg");
    assert_eq!(tick_close["remote"], "origin");
    assert_eq!(tick_close["actor_name"], "daemon");
    assert_eq!(tick_close["outcome"], "ok");
    assert!(tick_close["duration_ms"].is_number());
  }

  #[test]
  fn a_level_below_the_threshold_is_dropped() {
    let writer = CaptureWriter::new();
    let _guard = init_with_writer(LogLevel::Warn, writer.clone());

    info!("should not appear");
    tracing::warn!("should appear");

    let lines = writer.json_lines().expect("captured output must be ndjson");
    assert_eq!(lines.len(), 1, "got: {lines:?}");
    assert_eq!(lines[0]["message"], "should appear");
  }
}
