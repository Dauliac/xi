//! The user-scope log sink: one file, rotated at local midnight.
//!
//! `tracing-appender`'s rolling appender would be the obvious choice,
//! but it names the *live* file with the date too
//! (`sources-daemon.2026-11-02.log`), and the spec pins the live file at
//! `sources-daemon.log` so `tail -F` and the readout verbs have a stable
//! path. So this is a small appender of its own:
//!
//! - writes always land in `<dir>/sources-daemon.log`,
//! - the first write after the local date changes renames the live file to
//!   `sources-daemon.log.<yesterday>` and starts a fresh one,
//! - rotated files older than the retention window are deleted on the same
//!   pass.
//!
//! Rotation is checked on write rather than on a timer. A daemon that
//! was asleep over midnight rotates on its next tick, which is the
//! behaviour an operator wants: no file is cut while nothing is
//! happening, and no thread exists purely to wait for midnight.
//!
//! The clock is injected ([`Clock`]) so the midnight boundary is
//! testable without waiting for one.

use std::{
  fs::{self, File, OpenOptions},
  io::{self, Write},
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
};

use chrono::{Duration as ChronoDuration, Local, NaiveDate};

/// Basename of the live log file. Stable by spec.
pub const LOG_FILE_NAME: &str = "sources-daemon.log";

/// Subdirectory of the state dir the logs live in.
pub const LOG_DIR_NAME: &str = "logs";

/// Days of rotated logs kept when no layer says otherwise.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;

/// The log directory for a scope's state dir.
#[must_use]
pub fn log_dir(state_dir: &Path) -> PathBuf {
  state_dir.join(LOG_DIR_NAME)
}

/// The live log file for a scope's state dir.
#[must_use]
pub fn log_path(state_dir: &Path) -> PathBuf {
  log_dir(state_dir).join(LOG_FILE_NAME)
}

/// Source of "what day is it", injected so rotation is testable.
pub trait Clock: Send + Sync + std::fmt::Debug {
  /// Today's date in the local timezone.
  fn today(&self) -> NaiveDate;
}

/// The real clock: local time, as the spec's "midnight local time"
/// requires.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalClock;

impl Clock for LocalClock {
  fn today(&self) -> NaiveDate {
    Local::now().date_naive()
  }
}

/// A fixed clock, for tests and for replaying a rotation.
#[derive(Debug, Clone)]
pub struct FixedClock(Arc<Mutex<NaiveDate>>);

impl FixedClock {
  /// A clock stopped at `date`.
  #[must_use]
  pub fn new(date: NaiveDate) -> Self {
    Self(Arc::new(Mutex::new(date)))
  }

  /// Move the clock to `date`, as if time had passed.
  ///
  /// # Errors
  ///
  /// Returns an error if a previous holder of the lock panicked.
  pub fn set(&self, date: NaiveDate) -> io::Result<()> {
    let mut guard = self.0.lock().map_err(poisoned)?;
    *guard = date;
    // Released before the `Ok`, so a reader calling `today` does not
    // wait on a lock this function is finished with.
    drop(guard);
    Ok(())
  }
}

impl Clock for FixedClock {
  fn today(&self) -> NaiveDate {
    self
      .0
      .lock()
      .map_or_else(|poisoned| *poisoned.into_inner(), |guard| *guard)
  }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
  io::Error::other("the rotating log writer's lock is poisoned")
}

/// How long rotated files are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
  /// Days of history to keep, not counting the live file.
  pub days: u32,
}

impl Default for Retention {
  fn default() -> Self {
    Self {
      days: DEFAULT_RETENTION_DAYS,
    }
  }
}

#[derive(Debug)]
struct Inner {
  file: File,
  /// The date the live file's content belongs to.
  date: NaiveDate,
}

/// A write sink that rotates `sources-daemon.log` daily.
///
/// Cheap to clone-by-reference: the tracing layer holds an [`Arc`] and
/// every write takes the same lock, so lines from concurrent phases
/// cannot interleave mid-line.
#[derive(Debug)]
pub struct DailyRotating {
  dir:       PathBuf,
  retention: Retention,
  clock:     Box<dyn Clock>,
  inner:     Mutex<Inner>,
}

impl DailyRotating {
  /// Open (creating if needed) the live log under `dir`.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from creating the directory or opening the
  /// file.
  pub fn open(
    dir: impl Into<PathBuf>,
    retention: Retention,
  ) -> io::Result<Self> {
    Self::open_with_clock(dir, retention, Box::new(LocalClock))
  }

  /// Open the live log against an arbitrary clock.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from creating the directory or opening the
  /// file.
  pub fn open_with_clock(
    dir: impl Into<PathBuf>,
    retention: Retention,
    clock: Box<dyn Clock>,
  ) -> io::Result<Self> {
    let dir = dir.into();
    fs::create_dir_all(&dir)?;
    harden_dir(&dir)?;

    let today = clock.today();
    let path = dir.join(LOG_FILE_NAME);
    // An existing live file whose content predates today is rotated on
    // open rather than appended to, so a daemon restarted the next
    // morning does not glue two days into one file.
    let date = existing_content_date(&path)?.unwrap_or(today);
    let file = open_append(&path)?;
    let sink = Self {
      dir,
      retention,
      clock,
      inner: Mutex::new(Inner { file, date }),
    };
    if date != today {
      sink.rotate_to(today)?;
    }
    Ok(sink)
  }

  /// Path of the live log file.
  #[must_use]
  pub fn live_path(&self) -> PathBuf {
    self.dir.join(LOG_FILE_NAME)
  }

  /// Path a file rotated out on `date` is renamed to.
  #[must_use]
  pub fn rotated_path(&self, date: NaiveDate) -> PathBuf {
    self.dir.join(format!("{LOG_FILE_NAME}.{date}"))
  }

  /// Rotate if the local date has moved on, then prune old files.
  ///
  /// Called before every write; a no-op on the overwhelmingly common
  /// path where the date has not changed.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from the rename, the reopen, or the prune.
  pub fn rotate_if_due(&self) -> io::Result<bool> {
    let today = self.clock.today();
    let current = self.inner.lock().map_err(poisoned)?.date;
    if current == today {
      return Ok(false);
    }
    self.rotate_to(today)?;
    Ok(true)
  }

  fn rotate_to(&self, today: NaiveDate) -> io::Result<()> {
    {
      let mut inner = self.inner.lock().map_err(poisoned)?;
      inner.file.flush()?;

      let live = self.live_path();
      if live.exists() {
        let target = self.rotated_path(inner.date);
        // A same-day restart that already rotated leaves the target in
        // place; appending keeps both halves of the day.
        if target.exists() {
          append_file_into(&live, &target)?;
          fs::remove_file(&live)?;
        } else {
          fs::rename(&live, &target)?;
        }
        harden_file(&target)?;
      }

      inner.file = open_append(&live)?;
      inner.date = today;
    }
    self.prune(today)
  }

  /// Delete rotated files older than the retention window.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from reading the directory or unlinking a
  /// file.
  pub fn prune(&self, today: NaiveDate) -> io::Result<()> {
    let keep_days = i64::from(self.retention.days);
    let Some(cutoff) =
      today.checked_sub_signed(ChronoDuration::days(keep_days))
    else {
      return Ok(());
    };

    for entry in fs::read_dir(&self.dir)? {
      let path = entry?.path();
      if let Some(date) = rotated_date_of(&path)
        && date < cutoff
      {
        fs::remove_file(&path)?;
      }
    }
    Ok(())
  }

  /// Flush buffered bytes to disk.
  ///
  /// # Errors
  ///
  /// Propagates the underlying flush error.
  pub fn flush_now(&self) -> io::Result<()> {
    self.inner.lock().map_err(poisoned)?.file.flush()
  }
}

impl Write for &DailyRotating {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.rotate_if_due()?;
    let mut inner = self.inner.lock().map_err(poisoned)?;
    inner.file.write(buf)
  }

  fn flush(&mut self) -> io::Result<()> {
    self.inner.lock().map_err(poisoned)?.file.flush()
  }
}

/// A rotating sink shared with a tracing layer.
///
/// `MakeWriter` hands out a writer per event; the `Arc` is what lets
/// every one of them reach the same file and the same lock.
#[derive(Debug, Clone)]
pub struct SharedRotating(Arc<DailyRotating>);

impl SharedRotating {
  /// Wrap an opened sink for use as a tracing writer.
  #[must_use]
  pub fn new(sink: DailyRotating) -> Self {
    Self(Arc::new(sink))
  }

  /// The sink itself, for paths, rotation and flushing.
  #[must_use]
  pub fn sink(&self) -> &DailyRotating {
    &self.0
  }
}

/// A single write borrow of the shared sink.
#[derive(Debug)]
pub struct RotatingWriter(Arc<DailyRotating>);

impl Write for RotatingWriter {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let mut sink: &DailyRotating = &self.0;
    sink.write(buf)
  }

  fn flush(&mut self) -> io::Result<()> {
    let mut sink: &DailyRotating = &self.0;
    sink.flush()
  }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedRotating {
  type Writer = RotatingWriter;

  fn make_writer(&'a self) -> Self::Writer {
    RotatingWriter(Arc::clone(&self.0))
  }
}

fn open_append(path: &Path) -> io::Result<File> {
  let file = OpenOptions::new().create(true).append(true).open(path)?;
  harden_file(path)?;
  Ok(file)
}

/// Append `from`'s bytes onto the end of `to`.
fn append_file_into(from: &Path, to: &Path) -> io::Result<()> {
  let bytes = fs::read(from)?;
  let mut target = OpenOptions::new().append(true).open(to)?;
  target.write_all(&bytes)?;
  target.flush()
}

/// The date the existing live file's content belongs to.
///
/// Taken from the file's mtime rather than from its contents: parsing
/// the last line would mean the log format and the rotation policy are
/// coupled, and an interrupted write would make the file unrotatable.
fn existing_content_date(path: &Path) -> io::Result<Option<NaiveDate>> {
  let metadata = match fs::metadata(path) {
    Ok(metadata) => metadata,
    Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(err) => return Err(err),
  };
  if metadata.len() == 0 {
    return Ok(None);
  }
  let modified: chrono::DateTime<Local> = metadata.modified()?.into();
  Ok(Some(modified.date_naive()))
}

/// The date encoded in a rotated filename, if this is one.
fn rotated_date_of(path: &Path) -> Option<NaiveDate> {
  let name = path.file_name()?.to_str()?;
  let suffix = name.strip_prefix(LOG_FILE_NAME)?.strip_prefix('.')?;
  NaiveDate::parse_from_str(suffix, "%Y-%m-%d").ok()
}

#[cfg(unix)]
fn harden_dir(dir: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt as _;
  fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(unix)]
fn harden_file(path: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt as _;
  fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn harden_dir(_dir: &Path) -> io::Result<()> {
  Ok(())
}

#[cfg(not(unix))]
fn harden_file(_path: &Path) -> io::Result<()> {
  Ok(())
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

  fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
  }

  fn sink(dir: &Path, clock: &FixedClock, days: u32) -> DailyRotating {
    DailyRotating::open_with_clock(
      dir,
      Retention { days },
      Box::new(clock.clone()),
    )
    .unwrap()
  }

  fn write_line(sink: &DailyRotating, text: &str) {
    let mut handle: &DailyRotating = sink;
    handle.write_all(text.as_bytes()).unwrap();
    handle.write_all(b"\n").unwrap();
    handle.flush().unwrap();
  }

  #[test]
  fn writes_land_in_the_stable_filename() {
    let tmp = tempfile::TempDir::new().unwrap();
    let clock = FixedClock::new(date("2026-11-02"));
    let sink = sink(tmp.path(), &clock, 30);

    write_line(&sink, "first");

    let live = tmp.path().join(LOG_FILE_NAME);
    assert!(live.exists(), "live file missing");
    assert_eq!(fs::read_to_string(&live).unwrap(), "first\n");
  }

  #[test]
  fn log_paths_hang_off_the_state_dir() {
    let state = Path::new("/var/lib/xi/state");
    assert_eq!(log_dir(state), Path::new("/var/lib/xi/state/logs"));
    assert_eq!(
      log_path(state),
      Path::new("/var/lib/xi/state/logs/sources-daemon.log")
    );
  }

  #[test]
  fn midnight_rotates_and_keeps_the_live_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let clock = FixedClock::new(date("2026-11-02"));
    let sink = sink(tmp.path(), &clock, 30);

    write_line(&sink, "monday");
    clock.set(date("2026-11-03")).unwrap();
    write_line(&sink, "tuesday");

    let live = tmp.path().join(LOG_FILE_NAME);
    let rotated = tmp.path().join(format!("{LOG_FILE_NAME}.2026-11-02"));
    assert_eq!(fs::read_to_string(&live).unwrap(), "tuesday\n");
    assert_eq!(fs::read_to_string(&rotated).unwrap(), "monday\n");
  }

  #[test]
  fn no_rotation_happens_within_a_day() {
    let tmp = tempfile::TempDir::new().unwrap();
    let clock = FixedClock::new(date("2026-11-02"));
    let sink = sink(tmp.path(), &clock, 30);

    write_line(&sink, "one");
    write_line(&sink, "two");

    assert!(!sink.rotate_if_due().unwrap());
    assert_eq!(
      fs::read_to_string(tmp.path().join(LOG_FILE_NAME)).unwrap(),
      "one\ntwo\n"
    );
    let rotated: Vec<_> = fs::read_dir(tmp.path())
      .unwrap()
      .filter_map(|e| rotated_date_of(&e.unwrap().path()))
      .collect();
    assert!(rotated.is_empty(), "unexpected rotation: {rotated:?}");
  }

  #[test]
  fn retention_deletes_the_oldest_file_on_day_thirty_one() {
    let tmp = tempfile::TempDir::new().unwrap();
    let clock = FixedClock::new(date("2026-01-01"));
    let sink = sink(tmp.path(), &clock, 30);
    write_line(&sink, "day one");

    // Roll forward 31 days, writing one line per day, landing on
    // 2026-02-01. Stepping with date arithmetic rather than formatting a
    // day-of-month keeps the walk on real dates: January runs out at 31,
    // and the whole point of this case is to cross the month boundary.
    let start = date("2026-01-01");
    for offset in 1..=31 {
      clock.set(start + ChronoDuration::days(offset)).unwrap();
      write_line(&sink, &format!("day {}", offset + 1));
    }

    let oldest = tmp.path().join(format!("{LOG_FILE_NAME}.2026-01-01"));
    assert!(!oldest.exists(), "oldest file was not pruned");
    assert!(tmp.path().join(LOG_FILE_NAME).exists());
    assert!(
      tmp
        .path()
        .join(format!("{LOG_FILE_NAME}.2026-01-31"))
        .exists(),
      "a file inside the window was pruned"
    );
  }

  #[test]
  fn rotated_filenames_round_trip_their_date() {
    let tmp = tempfile::TempDir::new().unwrap();
    let clock = FixedClock::new(date("2026-11-02"));
    let sink = sink(tmp.path(), &clock, 30);
    let rotated = sink.rotated_path(date("2026-11-01"));
    assert_eq!(rotated_date_of(&rotated), Some(date("2026-11-01")));
    assert_eq!(rotated_date_of(&sink.live_path()), None);
  }

  #[test]
  fn a_second_rotation_onto_the_same_day_appends() {
    let tmp = tempfile::TempDir::new().unwrap();
    let clock = FixedClock::new(date("2026-11-02"));
    let sink = sink(tmp.path(), &clock, 30);

    write_line(&sink, "before");
    clock.set(date("2026-11-03")).unwrap();
    write_line(&sink, "after");

    // Pretend the daemon restarted and wrote more content stamped on
    // the 3rd, then crossed midnight again into the 4th.
    clock.set(date("2026-11-03")).unwrap();
    write_line(&sink, "more");
    clock.set(date("2026-11-04")).unwrap();
    write_line(&sink, "next");

    let rotated = tmp.path().join(format!("{LOG_FILE_NAME}.2026-11-03"));
    assert_eq!(fs::read_to_string(&rotated).unwrap(), "after\nmore\n");
  }

  #[cfg(unix)]
  #[test]
  fn log_dir_and_file_are_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("logs");
    let clock = FixedClock::new(date("2026-11-02"));
    let sink = sink(&dir, &clock, 30);
    write_line(&sink, "line");

    let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    let file_mode =
      fs::metadata(sink.live_path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    assert_eq!(file_mode, 0o600);
  }
}
