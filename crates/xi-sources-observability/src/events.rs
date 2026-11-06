//! The event store: `<state_dir>/events.db`.
//!
//! Structured logs ([`crate::rotating`]/[`crate::journald`]) are a
//! stream an operator tails; the event store is the same underlying
//! occurrences kept queryable after the fact - "what happened to this
//! `sources_id` in the last hour" without grepping ndjson. One row per
//! event, written by the daemon alongside (not instead of) its log line
//! for the same occurrence, read back by `xi <scope> sources events`.
//!
//! Unlike [`crate::fields`]'s span vocabulary (which exists to describe
//! one tick's shape on the wire while it is in flight), [`EventKind`] is
//! its own closed set, fixed by spec.md's "Operator readout verbs"
//! requirement: `fetch`, `advance`, `verify`, `build`, `activate`,
//! `hook`, `lock-wait`. It overlaps [`crate::fields::Phase`] without
//! matching it exactly - `advance` and `lock-wait` have no phase span of
//! their own, and `resolve` is a phase but not a persisted event kind -
//! because a span is "this phase is currently running" and an event is
//! "this specific thing happened", and the two vocabularies answer
//! different questions.
//!
//! This module is *not* gated behind the `daemon` feature: unlike
//! `subscriber`/`journald`/`metrics`/`server`, a reader (the `xi` CLI,
//! built with `default-features = false`) needs [`EventStore::query`]
//! just as much as a daemon needs [`EventStore::record`] - see the
//! crate's top doc comment, "Crate layout and features".

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};

/// Basename of the event store within a scope's state dir.
pub const EVENTS_DB_NAME: &str = "events.db";

/// The event-store path for a state dir.
#[must_use]
pub fn events_db_path(state_dir: &Path) -> PathBuf {
  state_dir.join(EVENTS_DB_NAME)
}

/// `PRAGMA user_version` this module writes and expects.
///
/// Embedded in the database file itself (not just this crate's own
/// version) so a reader built against a different schema generation can
/// tell from the file alone, without needing to parse a row first.
pub const SCHEMA_VERSION: i64 = 1;

/// The closed set of event kinds a daemon may record.
///
/// Fixed by spec.md, in the order it lists them there.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
  /// Talked to the remote.
  Fetch,
  /// The sources dir moved to a new sha.
  Advance,
  /// Checked a tip's signature.
  Verify,
  /// Realised a generation.
  Build,
  /// Made a generation current.
  Activate,
  /// Ran an operator-supplied hook.
  Hook,
  /// Waited to acquire the sources lock.
  LockWait,
}

impl EventKind {
  /// Every kind, in the order metric labels and `--kind` error messages
  /// enumerate them.
  pub const ALL: &'static [Self] = &[
    Self::Fetch,
    Self::Advance,
    Self::Verify,
    Self::Build,
    Self::Activate,
    Self::Hook,
    Self::LockWait,
  ];

  /// The wire spelling, matching the serde representation and the value
  /// stored in the `kind` column.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Fetch => "fetch",
      Self::Advance => "advance",
      Self::Verify => "verify",
      Self::Build => "build",
      Self::Activate => "activate",
      Self::Hook => "hook",
      Self::LockWait => "lock-wait",
    }
  }

  /// Parse from the wire spelling.
  #[must_use]
  pub fn from_str_opt(raw: &str) -> Option<Self> {
    Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
  }
}

impl std::fmt::Display for EventKind {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// One persisted event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
  /// When the event happened.
  pub ts:          DateTime<Utc>,
  /// What kind of event this is.
  pub kind:        EventKind,
  /// Identity of the source this event is for.
  pub sources_id:  String,
  /// The remote involved, when the event has one.
  pub remote:      Option<String>,
  /// `ok`/`degraded`/`failed`, matching [`crate::fields::Outcome`]'s
  /// wire spelling - restated as a plain string rather than importing
  /// that type, because an event row outlives any one build and must
  /// stay readable even if that enum grows a variant.
  pub outcome:     Option<String>,
  /// The sha this event moved from, for `advance` events.
  pub old_sha:     Option<String>,
  /// The sha this event moved to, for `advance` events.
  pub new_sha:     Option<String>,
  /// How long the underlying operation took.
  pub duration_ms: Option<i64>,
  /// Free-text detail - a hook's name, a failure reason. Never a
  /// credential: the same redaction boundary
  /// ([`crate::redact::Scrubber`]) that guards the log guards this
  /// field before a daemon ever calls [`EventStore::record`].
  pub message:     Option<String>,
}

/// What `xi <scope> sources events` filters a query by.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
  /// Only events at or after this instant.
  pub since: Option<DateTime<Utc>>,
  /// Only events of this kind.
  pub kind:  Option<EventKind>,
  /// At most this many rows, most recent first.
  pub limit: Option<u32>,
}

/// Everything that can go wrong opening or using the event store.
#[derive(Debug, thiserror::Error)]
pub enum EventsError {
  /// The database file could not be opened.
  #[error("opening the event store at {path}: {source}")]
  Open {
    /// The path that failed to open.
    path:   PathBuf,
    /// What `rusqlite` said.
    #[source]
    source: rusqlite::Error,
  },
  /// The schema could not be created or migrated.
  #[error("preparing the event store schema: {0}")]
  Schema(#[source] rusqlite::Error),
  /// Writing an event failed.
  #[error("recording an event: {0}")]
  Write(#[source] rusqlite::Error),
  /// Reading events back failed.
  #[error("querying events: {0}")]
  Query(#[source] rusqlite::Error),
}

/// The daemon's event store - open, write, query.
///
/// Opening creates the schema if it is missing (`CREATE TABLE IF NOT
/// EXISTS`), so the first call from either a daemon (about to
/// [`EventStore::record`]) or a reader (about to
/// [`EventStore::query`] an as-yet-nonexistent store, which then
/// correctly reports zero rows rather than an error) behaves the same
/// way.
#[derive(Debug)]
pub struct EventStore {
  conn: Connection,
}

impl EventStore {
  /// Open (creating if needed) the event store at `path`.
  ///
  /// # Errors
  ///
  /// Returns [`EventsError::Open`] if the file cannot be opened, or
  /// [`EventsError::Schema`] if the table cannot be created.
  pub fn open(path: &Path) -> Result<Self, EventsError> {
    let conn = Connection::open(path).map_err(|source| {
      EventsError::Open {
        path: path.to_path_buf(),
        source,
      }
    })?;
    conn
      .execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           ts TEXT NOT NULL,
           kind TEXT NOT NULL,
           sources_id TEXT NOT NULL,
           remote TEXT,
           outcome TEXT,
           old_sha TEXT,
           new_sha TEXT,
           duration_ms INTEGER,
           message TEXT
         );
         CREATE INDEX IF NOT EXISTS events_ts_idx ON events(ts);",
      )
      .map_err(EventsError::Schema)?;
    conn
      .pragma_update(None, "user_version", SCHEMA_VERSION)
      .map_err(EventsError::Schema)?;
    Ok(Self { conn })
  }

  /// Open an in-memory store, for tests that do not want a temp file.
  ///
  /// # Errors
  ///
  /// Returns [`EventsError::Schema`] if the table cannot be created.
  pub fn open_in_memory() -> Result<Self, EventsError> {
    let conn = Connection::open_in_memory().map_err(|source| {
      EventsError::Open {
        path: PathBuf::from(":memory:"),
        source,
      }
    })?;
    conn
      .execute_batch(
        "CREATE TABLE events (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           ts TEXT NOT NULL,
           kind TEXT NOT NULL,
           sources_id TEXT NOT NULL,
           remote TEXT,
           outcome TEXT,
           old_sha TEXT,
           new_sha TEXT,
           duration_ms INTEGER,
           message TEXT
         )",
      )
      .map_err(EventsError::Schema)?;
    Ok(Self { conn })
  }

  /// Record one event.
  ///
  /// # Errors
  ///
  /// Returns [`EventsError::Write`] if the insert fails.
  pub fn record(&self, event: &Event) -> Result<(), EventsError> {
    self
      .conn
      .execute(
        "INSERT INTO events (ts, kind, sources_id, remote, outcome, old_sha, \
         new_sha, duration_ms, message) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, \
         ?8, ?9)",
        params![
          event.ts.to_rfc3339(),
          event.kind.as_str(),
          event.sources_id,
          event.remote,
          event.outcome,
          event.old_sha,
          event.new_sha,
          event.duration_ms,
          event.message,
        ],
      )
      .map_err(EventsError::Write)?;
    Ok(())
  }

  /// Query events matching `filter`, most recent first.
  ///
  /// A `limit` of `None` returns every matching row - the CLI default is
  /// the spec's own unbounded "last N" reading, where N is unset unless
  /// the operator asks.
  ///
  /// # Errors
  ///
  /// Returns [`EventsError::Query`] if the statement cannot be prepared
  /// or run.
  pub fn query(&self, filter: &EventFilter) -> Result<Vec<Event>, EventsError> {
    let mut sql = String::from(
      "SELECT ts, kind, sources_id, remote, outcome, old_sha, new_sha, \
       duration_ms, message FROM events WHERE 1=1",
    );
    if filter.since.is_some() {
      sql.push_str(" AND ts >= ?1");
    }
    if filter.kind.is_some() {
      sql.push_str(if filter.since.is_some() {
        " AND kind = ?2"
      } else {
        " AND kind = ?1"
      });
    }
    sql.push_str(" ORDER BY ts DESC, id DESC");
    if filter.limit.is_some() {
      sql.push_str(" LIMIT ?");
    }

    let mut stmt = self.conn.prepare(&sql).map_err(EventsError::Query)?;
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(since) = filter.since {
      params.push(Box::new(since.to_rfc3339()));
    }
    if let Some(kind) = filter.kind {
      params.push(Box::new(kind.as_str().to_owned()));
    }
    if let Some(limit) = filter.limit {
      params.push(Box::new(limit));
    }
    let param_refs: Vec<&dyn rusqlite::ToSql> =
      params.iter().map(AsRef::as_ref).collect();

    let mapped = stmt
      .query_map(param_refs.as_slice(), row_to_event)
      .map_err(EventsError::Query)?;

    let mut events = Vec::new();
    for event in mapped {
      events.push(event.map_err(EventsError::Query)?);
    }
    Ok(events)
  }

  /// How many events are stored, ignoring any filter.
  ///
  /// Mostly for tests that want to assert a write landed without
  /// round-tripping through [`EventStore::query`]'s own filtering.
  ///
  /// # Errors
  ///
  /// Returns [`EventsError::Query`] if the count cannot be read.
  pub fn count(&self) -> Result<u64, EventsError> {
    self
      .conn
      .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
      .optional()
      .map_err(EventsError::Query)
      .map(|count: Option<i64>| count.unwrap_or(0).max(0).cast_unsigned())
  }
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
  let ts_text: String = row.get(0)?;
  let kind_text: String = row.get(1)?;
  let ts = DateTime::parse_from_rfc3339(&ts_text)
    .map(|dt| dt.with_timezone(&Utc))
    .map_err(|err| {
      rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(err),
      )
    })?;
  let kind = EventKind::from_str_opt(&kind_text).ok_or_else(|| {
    rusqlite::Error::FromSqlConversionFailure(
      1,
      rusqlite::types::Type::Text,
      format!("unknown event kind '{kind_text}'").into(),
    )
  })?;
  Ok(Event {
    ts,
    kind,
    sources_id: row.get(2)?,
    remote: row.get(3)?,
    outcome: row.get(4)?,
    old_sha: row.get(5)?,
    new_sha: row.get(6)?,
    duration_ms: row.get(7)?,
    message: row.get(8)?,
  })
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use chrono::Duration as ChronoDuration;

  use super::*;

  fn event(kind: EventKind, ts: DateTime<Utc>) -> Event {
    Event {
      ts,
      kind,
      sources_id: "user:https://git.example/cfg".to_owned(),
      remote: Some("origin".to_owned()),
      outcome: Some("ok".to_owned()),
      old_sha: None,
      new_sha: None,
      duration_ms: Some(42),
      message: None,
    }
  }

  #[test]
  fn kind_round_trips_through_its_wire_spelling() {
    for kind in EventKind::ALL {
      assert_eq!(EventKind::from_str_opt(kind.as_str()), Some(*kind));
    }
  }

  #[test]
  fn unknown_kind_spelling_does_not_parse() {
    assert_eq!(EventKind::from_str_opt("push"), None);
  }

  #[test]
  fn events_db_path_hangs_off_the_state_dir() {
    let state = Path::new("/var/lib/xi/state");
    assert_eq!(
      events_db_path(state),
      Path::new("/var/lib/xi/state/events.db")
    );
  }

  #[test]
  fn a_fresh_store_has_no_events() {
    let store = EventStore::open_in_memory().unwrap();
    assert_eq!(store.count().unwrap(), 0);
    assert!(store.query(&EventFilter::default()).unwrap().is_empty());
  }

  #[test]
  fn a_recorded_event_round_trips() {
    let store = EventStore::open_in_memory().unwrap();
    let now = Utc::now();
    store.record(&event(EventKind::Fetch, now)).unwrap();

    assert_eq!(store.count().unwrap(), 1);
    let events = store.query(&EventFilter::default()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Fetch);
    assert_eq!(events[0].sources_id, "user:https://git.example/cfg");
    assert_eq!(events[0].duration_ms, Some(42));
  }

  #[test]
  fn query_filters_by_kind() {
    let store = EventStore::open_in_memory().unwrap();
    let now = Utc::now();
    store.record(&event(EventKind::Fetch, now)).unwrap();
    store.record(&event(EventKind::Build, now)).unwrap();
    store.record(&event(EventKind::Activate, now)).unwrap();

    let events = store
      .query(&EventFilter {
        kind: Some(EventKind::Activate),
        ..EventFilter::default()
      })
      .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Activate);
  }

  #[test]
  fn query_filters_by_since() {
    let store = EventStore::open_in_memory().unwrap();
    let now = Utc::now();
    store
      .record(&event(EventKind::Fetch, now - ChronoDuration::hours(2)))
      .unwrap();
    store.record(&event(EventKind::Fetch, now)).unwrap();

    let events = store
      .query(&EventFilter {
        since: Some(now - ChronoDuration::minutes(5)),
        ..EventFilter::default()
      })
      .unwrap();
    assert_eq!(events.len(), 1);
  }

  #[test]
  fn query_respects_limit_and_orders_most_recent_first() {
    let store = EventStore::open_in_memory().unwrap();
    let now = Utc::now();
    for offset in 0..5 {
      store
        .record(&event(
          EventKind::Fetch,
          now - ChronoDuration::minutes(offset),
        ))
        .unwrap();
    }

    let events = store
      .query(&EventFilter {
        limit: Some(2),
        ..EventFilter::default()
      })
      .unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].ts >= events[1].ts, "must be most-recent-first");
  }

  #[test]
  fn kind_and_since_combine() {
    let store = EventStore::open_in_memory().unwrap();
    let now = Utc::now();
    store.record(&event(EventKind::Fetch, now)).unwrap();
    store.record(&event(EventKind::Build, now)).unwrap();
    store
      .record(&event(EventKind::Build, now - ChronoDuration::hours(2)))
      .unwrap();

    let events = store
      .query(&EventFilter {
        since: Some(now - ChronoDuration::minutes(5)),
        kind: Some(EventKind::Build),
        ..EventFilter::default()
      })
      .unwrap();
    assert_eq!(events.len(), 1);
  }

  #[test]
  fn opening_an_existing_store_twice_does_not_lose_data() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("events.db");
    {
      let store = EventStore::open(&path).unwrap();
      store.record(&event(EventKind::Fetch, Utc::now())).unwrap();
    }
    let reopened = EventStore::open(&path).unwrap();
    assert_eq!(reopened.count().unwrap(), 1);
  }

  #[test]
  fn opening_a_missing_store_creates_it_rather_than_erroring() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("nested").join("events.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let store = EventStore::open(&path).unwrap();
    assert_eq!(store.count().unwrap(), 0);
    assert!(path.exists());
  }
}
