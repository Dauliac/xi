//! BDD: `xi <scope> sources events`, the operator readout verb from
//! `xi-sources-observability` task 4.2.
//!
//! Drives the built `xi` binary, matching `bdd_sources_log.rs`'s
//! rationale: the thing under test is the command surface (where the
//! verb mounts, how it reads the event store, how scope resolves it),
//! which a library-level test cannot see.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{path::PathBuf, process::Command};

/// Path to the binary under test, as cargo built it for this run.
fn xi() -> PathBuf {
  let mut p = std::env::current_exe().expect("test exe path");
  p.pop(); // deps/
  p.pop(); // debug/
  p.push("xi");
  p
}

/// A fresh `$XDG_STATE_HOME/xi/events.db` fixture with three rows
/// matching the schema `xi_sources_observability::events::EventStore`
/// creates.
fn fixture() -> tempfile::TempDir {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let dir = tmp.path().join("xi");
  std::fs::create_dir_all(&dir).expect("mkdir");
  let conn =
    rusqlite::Connection::open(dir.join("events.db")).expect("open fixture db");
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
    .expect("create schema");
  conn
    .execute(
      "INSERT INTO events (ts, kind, sources_id, remote, outcome, old_sha, \
       new_sha, duration_ms, message) VALUES ('2026-11-03T09:00:00Z', \
       'fetch', 'user:x', 'origin', 'ok', NULL, NULL, 120, NULL)",
      [],
    )
    .expect("insert fetch");
  conn
    .execute(
      "INSERT INTO events (ts, kind, sources_id, remote, outcome, old_sha, \
       new_sha, duration_ms, message) VALUES ('2026-11-03T10:00:00Z', \
       'advance', 'user:x', 'origin', 'ok', 'aaa111', 'bbb222', 5, 'advanced')",
      [],
    )
    .expect("insert advance");
  conn
    .execute(
      "INSERT INTO events (ts, kind, sources_id, remote, outcome, old_sha, \
       new_sha, duration_ms, message) VALUES ('2026-11-03T11:00:00Z', \
       'activate', 'user:x', NULL, 'degraded', NULL, NULL, NULL, 'activation \
       needed a rebuild')",
      [],
    )
    .expect("insert activate");
  tmp
}

fn run(xdg_state_home: &std::path::Path, args: &[&str]) -> (String, bool) {
  let out = Command::new(xi())
    .args(args)
    .env("XI_NO_CHECKS", "1")
    .env("XDG_STATE_HOME", xdg_state_home)
    .output()
    .expect("spawn xi");
  let merged = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  (merged, out.status.success())
}

// -- Mounting: the verb lives under `sources`, scoped like its siblings ---

#[test]
fn events_is_mounted_under_sources_for_every_scope_verb() {
  let tmp = fixture();
  for verb in ["home", "system", "nixos", "darwin"] {
    let (out, ok) = run(tmp.path(), &[verb, "sources", "events", "--help"]);
    assert!(ok, "`xi {verb} sources events --help` failed: {out}");
    assert!(out.contains("--since"), "{verb}: {out}");
    assert!(out.contains("--kind"), "{verb}: {out}");
    assert!(out.contains("--limit"), "{verb}: {out}");
    assert!(out.contains("--json"), "{verb}: {out}");
  }
}

#[test]
fn events_does_not_create_a_top_level_command_either() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &["sources", "events"]);
  assert!(!ok, "a top-level `xi sources events` must not exist");
  assert!(out.contains("unrecognized subcommand"), "got: {out}");
}

// -- Requirement: Operator readout verbs -----------------------------------

#[test]
fn events_lists_every_row_most_recent_first() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &["home", "sources", "events"]);
  assert!(ok, "events failed: {out}");

  let activate_at = out.find("activate").expect("activate row present");
  let advance_at = out.find("advance").expect("advance row present");
  let fetch_at = out.find("fetch").expect("fetch row present");
  assert!(
    activate_at < advance_at && advance_at < fetch_at,
    "rows must be most-recent-first: {out}"
  );
}

#[test]
fn events_filters_by_kind() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &[
    "home", "sources", "events", "--kind", "advance",
  ]);
  assert!(ok, "events failed: {out}");
  assert!(out.contains("advance"), "got: {out}");
  assert!(!out.contains("fetch"), "got: {out}");
  assert!(!out.contains("activate"), "got: {out}");
}

#[test]
fn events_filters_by_since() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &[
    "home",
    "sources",
    "events",
    "--since",
    "2026-11-03T09:30:00Z",
  ]);
  assert!(ok, "events failed: {out}");
  assert!(!out.contains("fetch"), "got: {out}");
  assert!(out.contains("advance"), "got: {out}");
  assert!(out.contains("activate"), "got: {out}");
}

#[test]
fn events_respects_limit() {
  let tmp = fixture();
  let (out, ok) =
    run(tmp.path(), &["home", "sources", "events", "--limit", "1"]);
  assert!(ok, "events failed: {out}");
  assert!(out.contains("activate"), "got: {out}");
  assert!(!out.contains("advance"), "got: {out}");
  assert!(!out.contains("fetch"), "got: {out}");
}

#[test]
fn an_invalid_since_value_is_rejected_with_a_clear_message() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &[
    "home",
    "sources",
    "events",
    "--since",
    "not-a-timestamp",
  ]);
  assert!(!ok, "a malformed --since must be rejected");
  assert!(out.contains("--since"), "got: {out}");
}

#[test]
fn an_invalid_kind_value_is_rejected_with_a_clear_message() {
  let tmp = fixture();
  let (out, ok) =
    run(tmp.path(), &["home", "sources", "events", "--kind", "push"]);
  assert!(!ok, "a malformed --kind must be rejected");
  assert!(out.contains("--kind"), "got: {out}");
  assert!(out.contains("push"), "got: {out}");
}

/// No events yet (the daemon has never run) is a clear, successful
/// "nothing to show" rather than an error or a side-effecting file
/// creation - see `xi_core::sources::events`'s top doc comment.
#[test]
fn no_event_store_yet_is_a_clear_message_not_an_error() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let (out, ok) = run(tmp.path(), &["home", "sources", "events"]);
  assert!(ok, "events must succeed even with no store yet: {out}");
  assert!(out.contains("no events recorded"), "got: {out}");
}

// -- `--json` emits the versioned envelope ---------------------------------

#[test]
fn json_mode_emits_the_schema_versioned_envelope() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &["home", "sources", "events", "--json"]);
  assert!(ok, "events --json failed: {out}");
  let parsed: serde_json::Value =
    serde_json::from_str(&out).expect("valid JSON");
  assert_eq!(parsed["schema_version"], 1);
  assert_eq!(parsed["events"].as_array().expect("events array").len(), 3);
}

// -- System scope resolves against its own state dir -----------------------

/// System scope has its own state dir, so a user-scope fixture must not
/// leak into it.
#[test]
fn system_scope_does_not_read_the_user_scope_store() {
  let tmp = fixture(); // a user-scope fixture that must be ignored
  let (out, ok) = run(tmp.path(), &["system", "sources", "events", "--json"]);
  assert!(ok, "events failed: {out}");
  let parsed: serde_json::Value =
    serde_json::from_str(&out).expect("valid JSON");
  assert_eq!(
    parsed["events"].as_array().expect("events array").len(),
    0,
    "system scope must not see the user-scope fixture: {out}"
  );
}
