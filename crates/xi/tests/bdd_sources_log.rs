//! BDD: `xi <scope> sources log tail`, the operator readout verb from
//! `xi-sources-observability` task 4.1.
//!
//! Drives the built `xi` binary, matching `bdd_sources_entrypoint.rs`'s
//! rationale: the thing under test is the command surface (where the
//! verb mounts, what it reads, how scope resolves it), which a
//! library-level test cannot see.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{
  path::PathBuf,
  process::{Command, Stdio},
};

/// Path to the binary under test, as cargo built it for this run.
fn xi() -> PathBuf {
  let mut p = std::env::current_exe().expect("test exe path");
  p.pop(); // deps/
  p.pop(); // debug/
  p.push("xi");
  p
}

/// A fresh `$XDG_STATE_HOME/xi/logs/sources-daemon.log` fixture with
/// two lines, one at each of two levels.
fn fixture() -> tempfile::TempDir {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let logs = tmp.path().join("xi").join("logs");
  std::fs::create_dir_all(&logs).expect("mkdir");
  std::fs::write(
    logs.join("sources-daemon.log"),
    "{\"timestamp\":\"2026-11-03T09:00:00Z\",\"level\":\"INFO\",\"message\":\"fetch \
     ok\"}\n{\"timestamp\":\"2026-11-03T09:00:01Z\",\"level\":\"WARN\",\"message\":\
     \"degraded\"}\n",
  )
  .expect("write fixture");
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
fn log_tail_is_mounted_under_sources_for_every_scope_verb() {
  let tmp = fixture();
  for verb in ["home", "system", "nixos", "darwin"] {
    let (out, ok) = run(tmp.path(), &[verb, "sources", "log", "tail", "--help"]);
    assert!(ok, "`xi {verb} sources log tail --help` failed: {out}");
    assert!(out.contains("--follow"), "{verb}: {out}");
    assert!(out.contains("--since"), "{verb}: {out}");
    assert!(out.contains("--level"), "{verb}: {out}");
  }
}

#[test]
fn log_tail_does_not_create_a_top_level_command_either() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &["sources", "log", "tail"]);
  assert!(!ok, "a top-level `xi sources log tail` must not exist");
  assert!(out.contains("unrecognized subcommand"), "got: {out}");
}

// -- Requirement: Operator readout verbs -----------------------------------

/// Scenario (adapted): user-scope tail reads the XDG log path.
#[test]
fn user_scope_tail_reads_the_xdg_log_path() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &["home", "sources", "log", "tail"]);
  assert!(ok, "tail failed: {out}");
  assert!(out.contains("fetch ok"), "got: {out}");
  assert!(out.contains("degraded"), "got: {out}");
}

#[test]
fn user_scope_tail_filters_by_level() {
  let tmp = fixture();
  let (out, ok) =
    run(tmp.path(), &["home", "sources", "log", "tail", "--level", "warn"]);
  assert!(ok, "tail failed: {out}");
  assert!(!out.contains("fetch ok"), "got: {out}");
  assert!(out.contains("degraded"), "got: {out}");
}

#[test]
fn user_scope_tail_filters_by_since() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &[
    "home",
    "sources",
    "log",
    "tail",
    "--since",
    "2026-11-03T09:00:01Z",
  ]);
  assert!(ok, "tail failed: {out}");
  assert!(!out.contains("fetch ok"), "got: {out}");
  assert!(out.contains("degraded"), "got: {out}");
}

#[test]
fn an_invalid_since_value_is_rejected_with_a_clear_message() {
  let tmp = fixture();
  let (out, ok) = run(tmp.path(), &[
    "home",
    "sources",
    "log",
    "tail",
    "--since",
    "not-a-timestamp",
  ]);
  assert!(!ok, "a malformed --since must be rejected");
  assert!(out.contains("--since"), "got: {out}");
}

#[test]
fn an_invalid_level_value_is_rejected_with_a_clear_message() {
  let tmp = fixture();
  let (out, ok) =
    run(tmp.path(), &["home", "sources", "log", "tail", "--level", "chatty"]);
  assert!(!ok, "a malformed --level must be rejected");
  assert!(out.contains("--level"), "got: {out}");
  assert!(out.contains("chatty"), "got: {out}");
}

/// No log yet (the daemon has never run) is a clear error, not a panic
/// or a silent empty success.
#[test]
fn a_missing_log_is_a_clear_error_naming_the_daemon() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let (out, ok) = run(tmp.path(), &["home", "sources", "log", "tail"]);
  assert!(!ok, "tailing a log that was never written must fail");
  assert!(out.contains("sources log"), "got: {out}");
}

// -- System scope resolves to journalctl, not a file -----------------------

/// System scope has no file to read at all - it resolves to
/// `journalctl -u xi-sources-daemon`. This machine's sandbox has a real
/// journald (see `xi-sources-observability`'s `journald` module tests),
/// so this exercises the real dispatch rather than skipping; on a
/// machine without one the daemon unit simply would not exist, which
/// `journalctl` itself reports rather than this command inventing its
/// own error for.
#[test]
fn system_scope_tail_does_not_read_the_user_scope_file() {
  let tmp = fixture(); // a user-scope fixture that must be ignored
  let (out, ok) = run(tmp.path(), &["system", "sources", "log", "tail"]);
  // Whatever journalctl says about a unit that was never installed,
  // it must not be the user-scope fixture's content.
  assert!(!out.contains("fetch ok"), "got: {out}");
  let _ = ok; // journalctl's own exit code, not this command's to assert on
}

// -- `--follow` is accepted and streams rather than exiting immediately ---

#[test]
fn follow_keeps_the_process_alive_instead_of_exiting() {
  let tmp = fixture();
  let mut child = Command::new(xi())
    .args(["home", "sources", "log", "tail", "--follow"])
    .env("XI_NO_CHECKS", "1")
    .env("XDG_STATE_HOME", tmp.path())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn xi");

  std::thread::sleep(std::time::Duration::from_millis(300));
  assert!(
    child.try_wait().expect("try_wait").is_none(),
    "--follow must not exit on its own with no new lines appended"
  );
  child.kill().expect("kill");
  let _ = child.wait();
}
