//! BDD: a full tick emits one root span plus its five phase children.
//!
//! Mirrors `openspec/specs/xi-sources-observability/spec.md`'s "Full tick
//! emits 5 child spans" scenario:
//!
//! - GIVEN a tick that advances the sources dir and successfully activates
//! - WHEN the tick runs
//! - THEN the tracing sink receives one root `xi.sources.tick` span with
//!   five child spans in order (fetch, verify, resolve, build, activate)
//!
//! There is no tick loop to run yet (`xi-sources-gitops` has not landed
//! one), so this drives `fields::TickSpan`/`fields::PhaseSpan` - the
//! vocabulary the eventual loop will call - directly through the real
//! `subscriber` pipeline, which is exactly the boundary this capability
//! owns: it is responsible for what a tick's span shape looks like on the
//! wire, not for when a tick runs.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use tracing_subscriber::{Layer as _, fmt, layer::SubscriberExt as _};
use xi_sources_observability::{
  LogLevel,
  fields::{Outcome, Phase, TickSpan},
  subscriber::capture::CaptureWriter,
};

/// Build a subscriber that lists the full span ancestry
/// (`with_span_list(true)`) rather than [`subscriber::init_with_writer`]'s
/// current-span-only shape, so this test can assert a phase close is
/// nested under the tick, not just that it happened.
///
/// `FmtSpan::NONE`, not `CLOSE`: `fields::TickSpan`/`fields::PhaseSpan`
/// now emit their own explicit `"close"` event while still entered (see
/// `fields`'s top doc comment), so a second, `fmt`-synthesized close
/// event here would double every count this file asserts on.
fn init_with_ancestry(writer: CaptureWriter) -> tracing::subscriber::DefaultGuard {
  let layer = fmt::layer()
    .json()
    .flatten_event(true)
    .with_current_span(true)
    .with_span_list(true)
    .with_span_events(fmt::format::FmtSpan::NONE)
    .with_writer(writer)
    .with_filter(tracing_subscriber::filter::LevelFilter::from_level(
      LogLevel::Info.to_tracing(),
    ));
  let subscriber = tracing_subscriber::Registry::default().with(layer);
  tracing::subscriber::set_default(subscriber)
}

#[test]
fn a_full_tick_emits_one_root_span_and_five_ordered_phase_children() {
  let writer = CaptureWriter::new();
  let _guard = init_with_ancestry(writer.clone());

  let tick =
    TickSpan::start("user:https://git.example/cfg", "https://git.example/cfg");
  for phase in Phase::FULL_TICK {
    let child = tick.phase(*phase);
    child.record_shas(Some("aaaa111"), Some("bbbb222"));
    let _ = child.finish(Outcome::Ok);
  }
  let _ = tick.finish(Outcome::Ok);

  let lines = writer.json_lines().expect("captured output must be ndjson");
  let closes: Vec<_> = lines.iter().filter(|l| l["message"] == "close").collect();

  // Exactly 5 phase closes plus 1 tick close - no more, no fewer.
  assert_eq!(closes.len(), 6, "got: {lines:#?}");

  let phase_closes = &closes[0..5];
  let tick_close = closes[5];

  let expected_order = ["fetch", "verify", "resolve", "build", "activate"];
  for (close, expected_phase) in phase_closes.iter().zip(expected_order) {
    assert_eq!(close["span"]["name"], "xi.sources.phase");
    assert_eq!(close["span"]["phase"], expected_phase);
    assert_eq!(close["span"]["outcome"], "ok");
    assert_eq!(close["span"]["old_sha"], "aaaa111");
    assert_eq!(close["span"]["new_sha"], "bbbb222");
    assert!(close["span"]["duration_ms"].is_number());
  }

  assert_eq!(tick_close["span"]["name"], "xi.sources.tick");
  assert_eq!(
    tick_close["span"]["sources_id"],
    "user:https://git.example/cfg"
  );
  assert_eq!(
    tick_close["span"]["remote"],
    "https://git.example/cfg"
  );
  assert_eq!(tick_close["span"]["actor_name"], "daemon");
  assert_eq!(tick_close["span"]["outcome"], "ok");
  assert!(tick_close["span"]["duration_ms"].is_number());
}

/// A tick that only reaches `fetch` before degrading (no change found, no
/// build attempted) emits its root plus exactly the phases it actually
/// ran - the fixed vocabulary does not force phases that did not happen.
#[test]
fn a_tick_that_stops_early_emits_only_the_phases_it_reached() {
  let writer = CaptureWriter::new();
  let _guard = init_with_ancestry(writer.clone());

  let tick = TickSpan::start("user:https://git.example/cfg", "origin");
  let fetch = tick.phase(Phase::Fetch);
  let _ = fetch.finish(Outcome::Ok);
  let _ = tick.finish(Outcome::Ok);

  let lines = writer.json_lines().expect("captured output must be ndjson");
  let phase_closes: Vec<_> = lines
    .iter()
    .filter(|l| l["message"] == "close" && l["span"]["name"] == "xi.sources.phase")
    .collect();
  assert_eq!(phase_closes.len(), 1, "got: {lines:#?}");
  assert_eq!(phase_closes[0]["span"]["phase"], "fetch");
}

/// `TickSpan::phase` really does parent the phase under the tick, not
/// just happen to run while it is open: an ordinary event logged from
/// inside an explicitly entered phase carries the tick in its span
/// ancestry.
///
/// Uses a manually logged event (rather than inspecting `finish`'s own
/// `"close"` event, which by now is a genuine contextual event too -
/// see `fields`'s top doc comment) only because it is the clearest way
/// to isolate and prove the parenting `fields::TickSpan::phase`
/// establishes, independent of anything `finish` does.
#[test]
fn a_phase_is_really_nested_under_its_tick() {
  let writer = CaptureWriter::new();
  let _guard = init_with_ancestry(writer.clone());

  let tick = TickSpan::start("user:https://git.example/cfg", "origin");
  let phase = tick.phase(Phase::Fetch);
  {
    let _entered = phase.span().enter();
    tracing::info!("phase in progress");
  }
  let _ = phase.finish(Outcome::Ok);
  let _ = tick.finish(Outcome::Ok);

  let lines = writer.json_lines().expect("captured output must be ndjson");
  let progress = lines
    .iter()
    .find(|l| l["message"] == "phase in progress")
    .expect("the in-progress event was captured");

  assert_eq!(progress["span"]["name"], "xi.sources.phase");
  let spans = progress["spans"].as_array().expect("spans list present");
  let names: Vec<&str> =
    spans.iter().filter_map(|s| s["name"].as_str()).collect();
  assert_eq!(names, vec!["xi.sources.tick", "xi.sources.phase"]);
}
