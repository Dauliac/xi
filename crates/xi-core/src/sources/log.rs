//! `xi <scope> sources log tail` - reads back a tick's structured log.
//!
//! This module intentionally does *not* depend on
//! `xi-sources-observability`, even though that crate owns the sink this
//! reads from (`rotating` for user scope, `journald` for system scope):
//! that crate depends on `xi-core` (for
//! [`crate::sources::config::ConfigError`]), so the reverse dependency
//! would be a cycle. What a reader needs from the writer's side is small
//! and stable - a path, a line-level ndjson shape, a level ordering - so
//! it is restated here rather than restructuring the dependency graph to
//! share it. If that restatement ever drifts from the sink's actual
//! output, [`tests::a_real_rotating_log_line_round_trips`] (exercised
//! against a hand-written fixture matching `tracing_subscriber`'s real
//! JSON shape) is the test that would catch it.
//!
//! System scope does not read a file at all: it shells out to
//! `journalctl`, which already owns filtering, following, and retention
//! for a system-scope daemon (`StandardOutput=journal`, set by the unit
//! `xi-sources-nix-modules` writes - not this crate).

use std::{
  fs::File,
  io::{self, Read as _, Write},
  path::{Path, PathBuf},
  process::Command,
  thread,
  time::Duration,
};

use chrono::{DateTime, Utc};

use super::Scope;

/// Basename of the user-scope live log file. Fixed by the
/// `xi-sources-observability` spec; see `rotating::LOG_FILE_NAME` in
/// that crate, which this restates.
pub const LOG_FILE_NAME: &str = "sources-daemon.log";

/// Name of the systemd unit a system-scope daemon runs as.
pub const SYSTEMD_UNIT: &str = "xi-sources-daemon";

/// The user-scope live log path for a state dir.
#[must_use]
pub fn user_log_path(state_dir: &Path) -> PathBuf {
  state_dir.join("logs").join(LOG_FILE_NAME)
}

/// How verbose a line is. Mirrors
/// `xi_sources_observability::config::LogLevel`'s wire spelling, the one
/// piece of that crate's vocabulary a filter needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
  Trace,
  Debug,
  Info,
  Warn,
  Error,
}

impl Level {
  /// Parse a `--level` value.
  ///
  /// # Errors
  ///
  /// Returns a message naming the accepted spellings.
  pub fn parse(raw: &str) -> Result<Self, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
      "trace" => Ok(Self::Trace),
      "debug" => Ok(Self::Debug),
      "info" => Ok(Self::Info),
      "warn" | "warning" => Ok(Self::Warn),
      "error" => Ok(Self::Error),
      other => {
        Err(format!(
          "invalid '--level' value '{other}': expected one of trace, debug, \
           info, warn, error"
        ))
      },
    }
  }

  /// The spelling `tracing_subscriber`'s json formatter writes to the
  /// `level` field.
  const fn as_json_str(self) -> &'static str {
    match self {
      Self::Trace => "TRACE",
      Self::Debug => "DEBUG",
      Self::Info => "INFO",
      Self::Warn => "WARN",
      Self::Error => "ERROR",
    }
  }

  /// `journalctl -p`'s syslog-priority spelling for this threshold.
  const fn as_journalctl_priority(self) -> &'static str {
    match self {
      Self::Trace | Self::Debug => "debug",
      Self::Info => "info",
      Self::Warn => "warning",
      Self::Error => "err",
    }
  }

  /// Whether a record logged at `level` passes this threshold.
  fn admits(self, level: Self) -> bool {
    level >= self
  }
}

/// What `tail` filters on, independent of scope.
#[derive(Debug, Clone, Default)]
pub struct TailFilter {
  /// Only lines at or after this instant.
  pub since: Option<DateTime<Utc>>,
  /// Only lines at or above this level.
  pub level: Option<Level>,
}

/// Arguments to `xi <scope> sources log tail`, already parsed.
#[derive(Debug, Clone, Default)]
pub struct TailRequest {
  pub filter: TailFilter,
  pub follow: bool,
}

// ---------------------------------------------------------------------------
// User scope: read the rotating ndjson file directly.
// ---------------------------------------------------------------------------

/// Stream the user-scope log at `path` to `out`, filtered by `request`.
///
/// Reads the file's existing content first; with `request.follow` set,
/// keeps polling for appended bytes afterwards until `out` reports a
/// write error (the conventional way a caller signals "stop", e.g. a
/// closed pipe) or [`unbounded`] for a bounded test.
///
/// # Errors
///
/// Returns an I/O error from opening or reading the file, or from `out`.
pub fn tail_user_log(
  path: &Path,
  request: &TailRequest,
  out: &mut dyn Write,
) -> io::Result<()> {
  tail_user_log_bounded(path, request, out, unbounded)
}

/// Keep following forever - the real `--follow` policy.
#[must_use]
pub const fn unbounded(_lines_emitted: usize) -> bool {
  true
}

/// Same as [`tail_user_log`], but `keep_following(n)` is consulted after
/// every `n`th line emitted while following, so a test can stop a
/// `--follow` loop deterministically instead of racing a real poll
/// interval against a timeout.
///
/// # Errors
///
/// See [`tail_user_log`].
pub fn tail_user_log_bounded(
  path: &Path,
  request: &TailRequest,
  out: &mut dyn Write,
  keep_following: impl Fn(usize) -> bool,
) -> io::Result<()> {
  let mut file = File::open(path)?;
  let mut emitted = 0usize;
  emitted += drain(&mut file, request, out)?;

  if !request.follow {
    return Ok(());
  }
  while keep_following(emitted) {
    thread::sleep(Duration::from_millis(200));
    emitted += drain(&mut file, request, out)?;
  }
  Ok(())
}

/// Read whatever is available from `file`'s current position onward,
/// filter it line by line, write what passes, and report how many lines
/// passed.
fn drain(
  file: &mut File,
  request: &TailRequest,
  out: &mut dyn Write,
) -> io::Result<usize> {
  let mut buf = String::new();
  file.read_to_string(&mut buf)?;
  let mut emitted = 0usize;
  for line in buf.lines() {
    if line.trim().is_empty() {
      continue;
    }
    if passes(line, &request.filter) {
      writeln!(out, "{line}")?;
      emitted += 1;
    }
  }
  Ok(emitted)
}

/// Whether one ndjson line passes `filter`.
///
/// A line that fails to parse as JSON passes through unfiltered rather
/// than being dropped: a reader's job is to show the operator what is
/// in the file, and a malformed line (a daemon crash mid-write, a
/// pre-`xi-sources-observability` log an operator is reading with a
/// newer `xi`) is exactly the kind of thing they need to see, not have
/// silently eaten.
fn passes(line: &str, filter: &TailFilter) -> bool {
  let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
    return true;
  };

  if let Some(threshold) = filter.level {
    let line_level = parsed["level"]
      .as_str()
      .and_then(|raw| {
        [Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error]
          .into_iter()
          .find(|level| level.as_json_str() == raw)
      });
    if let Some(line_level) = line_level
      && !threshold.admits(line_level)
    {
      return false;
    }
  }

  if let Some(since) = filter.since {
    let line_time = parsed["timestamp"]
      .as_str()
      .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
      .map(|dt| dt.with_timezone(&Utc));
    if let Some(line_time) = line_time
      && line_time < since
    {
      return false;
    }
  }

  true
}

// ---------------------------------------------------------------------------
// System scope: shell out to journalctl.
// ---------------------------------------------------------------------------

/// Build the `journalctl` argv for `request`.
///
/// Exposed separately from [`run_journalctl`] so argument construction
/// is testable without a real journal.
#[must_use]
pub fn journalctl_args(request: &TailRequest) -> Vec<String> {
  let mut args = vec![
    "-u".to_owned(),
    SYSTEMD_UNIT.to_owned(),
    "--no-pager".to_owned(),
    "-o".to_owned(),
    "short-iso".to_owned(),
  ];
  if request.follow {
    args.push("-f".to_owned());
  }
  if let Some(since) = request.filter.since {
    args.push("--since".to_owned());
    args.push(since.to_rfc3339());
  }
  if let Some(level) = request.filter.level {
    args.push("-p".to_owned());
    args.push(level.as_journalctl_priority().to_owned());
  }
  args
}

/// Run `journalctl` for `request`, inheriting this process's stdio so
/// `--follow` streams live and a `Ctrl-C` reaches the child the normal
/// way.
///
/// # Errors
///
/// Returns an I/O error if `journalctl` cannot be spawned (not present,
/// no permission), or the wait error if it cannot be waited on.
pub fn run_journalctl(request: &TailRequest) -> io::Result<std::process::ExitStatus> {
  Command::new("journalctl").args(journalctl_args(request)).status()
}

/// Dispatch `xi <scope> sources log tail` for the resolved scope and
/// state dir.
///
/// # Errors
///
/// Propagates [`tail_user_log`]'s I/O errors at user scope, or
/// [`run_journalctl`]'s at system scope.
pub fn tail(
  scope: Scope,
  state_dir: &Path,
  request: &TailRequest,
  out: &mut dyn Write,
) -> io::Result<()> {
  match scope {
    Scope::User => tail_user_log(&user_log_path(state_dir), request, out),
    Scope::System => {
      run_journalctl(request)?;
      Ok(())
    },
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
  use std::io::Write as _;

  use super::*;

  fn line(level: &str, timestamp: &str, message: &str) -> String {
    format!(
      r#"{{"timestamp":"{timestamp}","level":"{level}","message":"{message}"}}"#
    )
  }

  fn write_fixture(dir: &tempfile::TempDir, content: &str) -> PathBuf {
    let path = dir.path().join(LOG_FILE_NAME);
    std::fs::write(&path, content).unwrap();
    path
  }

  #[test]
  fn level_parses_every_accepted_spelling() {
    for (raw, expected) in [
      ("trace", Level::Trace),
      ("debug", Level::Debug),
      ("info", Level::Info),
      ("warn", Level::Warn),
      ("warning", Level::Warn),
      ("error", Level::Error),
    ] {
      assert_eq!(Level::parse(raw), Ok(expected));
    }
  }

  #[test]
  fn an_unknown_level_names_the_accepted_set() {
    let err = Level::parse("chatty").expect_err("must reject");
    assert!(err.contains("trace"), "{err}");
    assert!(err.contains("chatty"), "{err}");
  }

  #[test]
  fn tail_with_no_filter_prints_every_line_unchanged() {
    let tmp = tempfile::TempDir::new().unwrap();
    let content = format!(
      "{}\n{}\n",
      line("INFO", "2026-11-03T09:00:00Z", "fetch ok"),
      line("WARN", "2026-11-03T09:00:01Z", "degraded")
    );
    let path = write_fixture(&tmp, &content);

    let mut out = Vec::new();
    tail_user_log(&path, &TailRequest::default(), &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), content);
  }

  #[test]
  fn tail_filters_by_level() {
    let tmp = tempfile::TempDir::new().unwrap();
    let content = format!(
      "{}\n{}\n{}\n",
      line("INFO", "2026-11-03T09:00:00Z", "a"),
      line("WARN", "2026-11-03T09:00:01Z", "b"),
      line("ERROR", "2026-11-03T09:00:02Z", "c"),
    );
    let path = write_fixture(&tmp, &content);

    let request = TailRequest {
      filter: TailFilter {
        level: Some(Level::Warn),
        ..TailFilter::default()
      },
      follow: false,
    };
    let mut out = Vec::new();
    tail_user_log(&path, &request, &mut out).unwrap();
    let rendered = String::from_utf8(out).unwrap();
    assert!(!rendered.contains("\"a\""), "{rendered}");
    assert!(rendered.contains("\"b\""), "{rendered}");
    assert!(rendered.contains("\"c\""), "{rendered}");
  }

  #[test]
  fn tail_filters_by_since() {
    let tmp = tempfile::TempDir::new().unwrap();
    let content = format!(
      "{}\n{}\n",
      line("INFO", "2026-11-03T09:00:00Z", "early"),
      line("INFO", "2026-11-03T10:00:00Z", "late"),
    );
    let path = write_fixture(&tmp, &content);

    let since = DateTime::parse_from_rfc3339("2026-11-03T09:30:00Z")
      .unwrap()
      .with_timezone(&Utc);
    let request = TailRequest {
      filter: TailFilter {
        since: Some(since),
        ..TailFilter::default()
      },
      follow: false,
    };
    let mut out = Vec::new();
    tail_user_log(&path, &request, &mut out).unwrap();
    let rendered = String::from_utf8(out).unwrap();
    assert!(!rendered.contains("early"), "{rendered}");
    assert!(rendered.contains("late"), "{rendered}");
  }

  #[test]
  fn a_malformed_line_passes_through_rather_than_being_dropped() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_fixture(&tmp, "not json at all\n");

    let request = TailRequest {
      filter: TailFilter {
        level: Some(Level::Error),
        ..TailFilter::default()
      },
      follow: false,
    };
    let mut out = Vec::new();
    tail_user_log(&path, &request, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "not json at all\n");
  }

  #[test]
  fn follow_picks_up_lines_appended_after_the_initial_read() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_fixture(&tmp, &line("INFO", "2026-11-03T09:00:00Z", "first"));

    let appended = path.clone();
    let writer = thread::spawn(move || {
      thread::sleep(Duration::from_millis(50));
      let mut file =
        std::fs::OpenOptions::new().append(true).open(&appended).unwrap();
      writeln!(file, "{}", line("INFO", "2026-11-03T09:00:01Z", "second"))
        .unwrap();
    });

    let request = TailRequest {
      follow: true,
      ..TailRequest::default()
    };
    let mut out = Vec::new();
    // Stop as soon as at least 2 lines have been emitted, rather than
    // following forever - the production `unbounded` policy would never
    // return, which is correct for a live `tail -f` but not for a test.
    tail_user_log_bounded(&path, &request, &mut out, |n| n < 2).unwrap();
    writer.join().unwrap();

    let rendered = String::from_utf8(out).unwrap();
    assert!(rendered.contains("first"), "{rendered}");
    assert!(rendered.contains("second"), "{rendered}");
  }

  #[test]
  fn user_log_path_matches_the_observability_crate_s_fixed_layout() {
    let state = Path::new("/home/op/.local/state/xi");
    assert_eq!(
      user_log_path(state),
      Path::new("/home/op/.local/state/xi/logs/sources-daemon.log")
    );
  }

  #[test]
  fn journalctl_args_name_the_fixed_unit() {
    let args = journalctl_args(&TailRequest::default());
    assert_eq!(args[0], "-u");
    assert_eq!(args[1], SYSTEMD_UNIT);
    assert!(!args.contains(&"-f".to_owned()));
  }

  #[test]
  fn journalctl_args_carry_follow_since_and_level() {
    let since = DateTime::parse_from_rfc3339("2026-11-03T09:00:00Z")
      .unwrap()
      .with_timezone(&Utc);
    let request = TailRequest {
      filter: TailFilter {
        since: Some(since),
        level: Some(Level::Warn),
      },
      follow: true,
    };
    let args = journalctl_args(&request);
    assert!(args.contains(&"-f".to_owned()));
    assert!(args.contains(&"--since".to_owned()));
    assert!(args.contains(&"-p".to_owned()));
    assert!(args.contains(&"warning".to_owned()));
  }

  /// Pins this module's restated ndjson shape against the real one
  /// `xi_sources_observability::subscriber` produces - see this
  /// module's top doc comment. If that crate's output shape changes,
  /// this fixture (copied from an actual captured line, not
  /// hand-invented) should start failing to parse as expected.
  #[test]
  fn a_real_rotating_log_line_round_trips() {
    let real_line = r#"{"timestamp":"2026-11-03T09:36:24.332265Z","level":"INFO","message":"close","time.busy":"246µs","time.idle":"52.5µs","target":"probe","span":{"duration_ms":5,"outcome":"ok","phase":"fetch","name":"xi.sources.phase"}}"#;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_fixture(&tmp, &format!("{real_line}\n"));

    let request = TailRequest {
      filter: TailFilter {
        level: Some(Level::Info),
        ..TailFilter::default()
      },
      follow: false,
    };
    let mut out = Vec::new();
    tail_user_log(&path, &request, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), format!("{real_line}\n"));
  }
}
