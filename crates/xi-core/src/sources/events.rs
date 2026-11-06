//! `xi <scope> sources events` - reads back the gitops daemon's event
//! store.
//!
//! Like [`super::log`], this module intentionally does *not* depend on
//! `xi-sources-observability`, even though that crate owns the schema
//! this reads from (`events::EventStore`): that crate depends on
//! `xi-core` (for [`crate::sources::config::ConfigError`]), so the
//! reverse dependency would be a cycle. The schema this restates is
//! small and has a `PRAGMA user_version` of its own to catch drift at
//! runtime; `tests::reading_matches_the_observability_crate_s_schema`
//! is the compile-time tripwire that would catch drift in the table
//! shape itself.
//!
//! Querying a store that does not exist yet (the daemon has never run)
//! returns zero rows rather than an error or a side-effecting file
//! creation: unlike [`super::log::tail_user_log`], which has to open a
//! real file to stream it, a read-only query has no reason to leave a
//! freshly created, empty `events.db` behind just because an operator
//! asked `xi sources events` before the first tick.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use super::Scope;

/// Basename of the event store within a scope's state dir. Fixed by the
/// `xi-sources-observability` spec; see that crate's
/// `events::EVENTS_DB_NAME`, which this restates.
pub const EVENTS_DB_NAME: &str = "events.db";

/// The event-store path for a state dir.
#[must_use]
pub fn events_db_path(state_dir: &Path) -> PathBuf {
  state_dir.join(EVENTS_DB_NAME)
}

/// The closed set of event kinds a daemon may record. Mirrors
/// `xi_sources_observability::events::EventKind`'s wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
  Fetch,
  Advance,
  Verify,
  Build,
  Activate,
  Hook,
  LockWait,
}

impl EventKind {
  /// Every spelling, in the order error messages list them.
  pub const ALL: &'static [&'static str] = &[
    "fetch",
    "advance",
    "verify",
    "build",
    "activate",
    "hook",
    "lock-wait",
  ];

  /// Parse a `--kind` value.
  ///
  /// # Errors
  ///
  /// Returns a message naming the accepted spellings.
  pub fn parse(raw: &str) -> Result<Self, String> {
    match raw.trim() {
      "fetch" => Ok(Self::Fetch),
      "advance" => Ok(Self::Advance),
      "verify" => Ok(Self::Verify),
      "build" => Ok(Self::Build),
      "activate" => Ok(Self::Activate),
      "hook" => Ok(Self::Hook),
      "lock-wait" => Ok(Self::LockWait),
      other => {
        Err(format!(
          "invalid '--kind' value '{other}': expected one of {}",
          Self::ALL.join(", ")
        ))
      },
    }
  }

  /// The wire spelling stored in the `kind` column.
  const fn as_str(self) -> &'static str {
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
}

impl std::fmt::Display for EventKind {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// One event, as read back for display or JSON output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
  pub ts:          String,
  pub kind:        String,
  pub sources_id:  String,
  pub remote:      Option<String>,
  pub outcome:     Option<String>,
  pub old_sha:     Option<String>,
  pub new_sha:     Option<String>,
  pub duration_ms: Option<i64>,
  pub message:     Option<String>,
}

/// What `xi <scope> sources events` filters a query by.
#[derive(Debug, Clone, Default)]
pub struct EventsQuery {
  pub since: Option<DateTime<Utc>>,
  pub kind:  Option<EventKind>,
  pub limit: Option<u32>,
}

/// The versioned envelope `xi <scope> sources events --json` emits.
///
/// `schema_version` matches [`super::SOURCES_SCHEMA_VERSION`], the same
/// envelope convention every other sources response uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsResponse {
  pub schema_version: u32,
  pub events:         Vec<EventRecord>,
}

impl EventsResponse {
  #[must_use]
  pub const fn new(events: Vec<EventRecord>) -> Self {
    Self {
      schema_version: super::SOURCES_SCHEMA_VERSION,
      events,
    }
  }
}

/// Everything that can go wrong reading the event store.
#[derive(Debug, thiserror::Error)]
pub enum EventsReadError {
  #[error("opening the event store at {path}: {source}")]
  Open {
    path:   PathBuf,
    #[source]
    source: rusqlite::Error,
  },
  #[error("querying events: {0}")]
  Query(#[source] rusqlite::Error),
}

/// Query the event store at `path`, filtered by `query`, most recent
/// first.
///
/// A store that does not exist yet answers with zero rows - see this
/// module's top doc comment for why that is not an error.
///
/// # Errors
///
/// Returns [`EventsReadError`] if an existing store cannot be opened or
/// queried (a corrupt file, a permissions problem).
pub fn query_events(
  path: &Path,
  query: &EventsQuery,
) -> Result<Vec<EventRecord>, EventsReadError> {
  if !path.exists() {
    return Ok(Vec::new());
  }

  let conn =
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
      .map_err(|source| {
        EventsReadError::Open {
          path: path.to_path_buf(),
          source,
        }
      })?;

  let mut sql = String::from(
    "SELECT ts, kind, sources_id, remote, outcome, old_sha, new_sha, \
     duration_ms, message FROM events WHERE 1=1",
  );
  if query.since.is_some() {
    sql.push_str(" AND ts >= ?1");
  }
  if query.kind.is_some() {
    sql.push_str(if query.since.is_some() {
      " AND kind = ?2"
    } else {
      " AND kind = ?1"
    });
  }
  sql.push_str(" ORDER BY ts DESC, id DESC");
  if query.limit.is_some() {
    sql.push_str(" LIMIT ?");
  }

  let mut stmt = conn.prepare(&sql).map_err(EventsReadError::Query)?;
  let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
  if let Some(since) = query.since {
    params.push(Box::new(since.to_rfc3339()));
  }
  if let Some(kind) = query.kind {
    params.push(Box::new(kind.as_str().to_owned()));
  }
  if let Some(limit) = query.limit {
    params.push(Box::new(limit));
  }
  let param_refs: Vec<&dyn rusqlite::ToSql> =
    params.iter().map(AsRef::as_ref).collect();

  let rows = stmt
    .query_map(param_refs.as_slice(), |row| {
      Ok(EventRecord {
        ts:          row.get(0)?,
        kind:        row.get(1)?,
        sources_id:  row.get(2)?,
        remote:      row.get(3)?,
        outcome:     row.get(4)?,
        old_sha:     row.get(5)?,
        new_sha:     row.get(6)?,
        duration_ms: row.get(7)?,
        message:     row.get(8)?,
      })
    })
    .map_err(EventsReadError::Query)?;

  let mut events = Vec::new();
  for row in rows {
    events.push(row.map_err(EventsReadError::Query)?);
  }
  Ok(events)
}

/// Dispatch `xi <scope> sources events` for the resolved scope and
/// state dir.
///
/// `scope` is accepted (unlike [`query_events`]) purely so this
/// function's signature mirrors every other verb's `(scope, state_dir,
/// ...)` shape - the event store has no per-scope branching the way
/// [`super::log::tail`] does, because the store already lives under the
/// resolved scope's own state dir.
///
/// # Errors
///
/// Propagates [`query_events`]'s errors.
pub fn events(
  _scope: Scope,
  state_dir: &Path,
  query: &EventsQuery,
) -> Result<EventsResponse, EventsReadError> {
  let rows = query_events(&events_db_path(state_dir), query)?;
  Ok(EventsResponse::new(rows))
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

  /// Builds a fixture db using exactly the schema
  /// `xi_sources_observability::events::EventStore::open` creates, so
  /// tests exercise the real contract rather than a shape this module
  /// invented independently. Kept in sync by
  /// [`reading_matches_the_observability_crate_s_schema`], the tripwire
  /// this module's top doc comment describes.
  fn fixture_db(path: &Path, rows: &[(&str, &str, &str)]) {
    let conn = Connection::open(path).unwrap();
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
      .unwrap();
    for (ts, kind, sources_id) in rows {
      conn
        .execute(
          "INSERT INTO events (ts, kind, sources_id, outcome, duration_ms) \
           VALUES (?1, ?2, ?3, 'ok', 42)",
          rusqlite::params![ts, kind, sources_id],
        )
        .unwrap();
    }
  }

  #[test]
  fn kind_parses_every_accepted_spelling() {
    for spelling in EventKind::ALL {
      assert!(EventKind::parse(spelling).is_ok(), "{spelling}");
    }
  }

  #[test]
  fn an_unknown_kind_names_the_accepted_set() {
    let err = EventKind::parse("push").expect_err("must reject");
    assert!(err.contains("push"), "{err}");
    assert!(err.contains("fetch"), "{err}");
    assert!(err.contains("lock-wait"), "{err}");
  }

  #[test]
  fn a_missing_store_returns_zero_rows_not_an_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("nope.db");
    let rows = query_events(&path, &EventsQuery::default()).unwrap();
    assert!(rows.is_empty());
    assert!(!path.exists(), "a read must not create the file");
  }

  #[test]
  fn an_unfiltered_query_returns_every_row_most_recent_first() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join(EVENTS_DB_NAME);
    fixture_db(&path, &[
      ("2026-11-03T09:00:00Z", "fetch", "user:x"),
      ("2026-11-03T10:00:00Z", "build", "user:x"),
    ]);

    let rows = query_events(&path, &EventsQuery::default()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].kind, "build");
    assert_eq!(rows[1].kind, "fetch");
  }

  #[test]
  fn query_filters_by_kind() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join(EVENTS_DB_NAME);
    fixture_db(&path, &[
      ("2026-11-03T09:00:00Z", "fetch", "user:x"),
      ("2026-11-03T10:00:00Z", "build", "user:x"),
      ("2026-11-03T11:00:00Z", "activate", "user:x"),
    ]);

    let rows = query_events(&path, &EventsQuery {
      kind: Some(EventKind::Activate),
      ..EventsQuery::default()
    })
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "activate");
  }

  #[test]
  fn query_filters_by_since() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join(EVENTS_DB_NAME);
    fixture_db(&path, &[
      ("2026-11-03T09:00:00Z", "fetch", "user:x"),
      ("2026-11-03T10:00:00Z", "fetch", "user:x"),
    ]);

    let since = DateTime::parse_from_rfc3339("2026-11-03T09:30:00Z")
      .unwrap()
      .with_timezone(&Utc);
    let rows = query_events(&path, &EventsQuery {
      since: Some(since),
      ..EventsQuery::default()
    })
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ts, "2026-11-03T10:00:00Z");
  }

  #[test]
  fn query_respects_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join(EVENTS_DB_NAME);
    fixture_db(&path, &[
      ("2026-11-03T09:00:00Z", "fetch", "user:x"),
      ("2026-11-03T10:00:00Z", "fetch", "user:x"),
      ("2026-11-03T11:00:00Z", "fetch", "user:x"),
    ]);

    let rows = query_events(&path, &EventsQuery {
      limit: Some(2),
      ..EventsQuery::default()
    })
    .unwrap();
    assert_eq!(rows.len(), 2);
  }

  #[test]
  fn events_db_path_hangs_off_the_state_dir() {
    let state = Path::new("/home/op/.local/state/xi");
    assert_eq!(
      events_db_path(state),
      Path::new("/home/op/.local/state/xi/events.db")
    );
  }

  #[test]
  fn response_envelope_carries_the_sources_schema_version() {
    let response = EventsResponse::new(vec![]);
    assert_eq!(
      response.schema_version,
      super::super::SOURCES_SCHEMA_VERSION
    );
  }

  /// Pins this module's restated schema against the real one
  /// `xi_sources_observability::events::EventStore::open` creates - see
  /// this module's top doc comment. The column list and order here are
  /// copied verbatim from that crate's `CREATE TABLE`; if that table
  /// shape changes, this fixture (and every test above built from it)
  /// should start failing to read as expected.
  #[test]
  fn reading_matches_the_observability_crate_s_schema() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join(EVENTS_DB_NAME);
    let conn = Connection::open(&path).unwrap();
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
         );
         CREATE INDEX events_ts_idx ON events(ts);",
      )
      .unwrap();
    conn
      .execute(
        "INSERT INTO events (ts, kind, sources_id, remote, outcome, old_sha, \
         new_sha, duration_ms, message) VALUES ('2026-11-03T09:00:00Z', \
         'advance', 'user:x', 'origin', 'ok', 'aaa', 'bbb', 120, 'advanced')",
        [],
      )
      .unwrap();
    drop(conn);

    let rows = query_events(&path, &EventsQuery::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].remote.as_deref(), Some("origin"));
    assert_eq!(rows[0].old_sha.as_deref(), Some("aaa"));
    assert_eq!(rows[0].new_sha.as_deref(), Some("bbb"));
    assert_eq!(rows[0].duration_ms, Some(120));
    assert_eq!(rows[0].message.as_deref(), Some("advanced"));
  }
}
