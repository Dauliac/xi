//! Persistent per-flake JSONL queue for deferred hook work.
//!
//! Every prompt tick that could not dispatch a query or command within the
//! prompt budget appends the payload here as a JSON line. A subsequent tick
//! reloads, refines (dedups + drops stale entries), atomically rewrites the
//! queue, and dispatches the refined list.
//!
//! On-disk layout: `$XDG_STATE_HOME/xi/hook-queue/<flake_id>.jsonl`.
//!
//! Append writes are `O_APPEND` + a single `write_all` call so writes from
//! independent shells interleave at line boundaries (Linux guarantees atomic
//! `write()` up to `PIPE_BUF` for `O_APPEND` files; entries here are well
//! under that ceiling).
//!
//! Refinement is a *pure function* over a `Vec<QueueEntry>` — callers load,
//! refine, rewrite. The rewrite is `.tmp` + `rename` for crash safety.
//!
//! A malformed line quarantines the whole file to `.corrupt.<epoch_ms>` and
//! the caller sees an empty queue: the spec says "queue corruption is
//! quarantined; no data loss beyond the corrupted queue is acceptable".

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::Result;
use color_eyre::eyre::eyre;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::daemon::protocol_v3::{Command, IdempotencyKey, Query};

/// Maximum entries retained per flake queue. Overflow drops the oldest
/// entries silently (per design § "Client prompt queue is per-flake JSONL":
/// "Bounded at 128 entries per flake").
pub const MAX_ENTRIES: usize = 128;

/// Hard ingress cap on the queue file size in bytes. Beyond this the
/// [`Queue::append`] path drops entries silently. At typical entry size
/// ~500 bytes this leaves >4× headroom over [`MAX_ENTRIES`] before
/// refinement kicks in.
const INGRESS_MAX_BYTES: u64 = 256 * 1024;

/// Entries older than this are dropped during refinement. Matches the
/// bounded-lifetime clause of the spec (`Client — persistent prompt queue
/// with refinement`).
pub const STALE_AFTER: Duration = Duration::from_mins(5);

/// Serialised payload of a single queued request.
///
/// A tagged sum so a human eyeballing the JSONL under `less` can tell a
/// deferred command from a deferred query at a glance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum QueuePayload {
  /// A read-only query the hook could not fit in the last tick.
  Query(Query),
  /// A mutating command the hook could not fit in the last tick.
  Command(Command),
}

/// One durable queue entry.
///
/// `id` is a ULID so entries retain a monotonic ordering when the queue is
/// interleaved from multiple shells; `created_at` is the wall-clock at
/// enqueue time and drives the staleness eviction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEntry {
  /// ULID; unique per enqueue call.
  pub id: String,
  /// Wall-clock at which the entry was created.
  pub created_at: SystemTime,
  /// The deferred request.
  pub payload: QueuePayload,
}

impl QueueEntry {
  /// Construct a fresh entry with a new ULID and `SystemTime::now()`.
  #[must_use]
  pub fn new(payload: QueuePayload) -> Self {
    Self {
      id: ulid::Ulid::new().to_string(),
      created_at: SystemTime::now(),
      payload,
    }
  }
}

/// A handle to one flake's on-disk queue file.
///
/// Cheap to construct; opens no file descriptors up front (each append/load
/// call opens fresh). The `path` is fully derived from the flake id + XDG.
#[derive(Debug, Clone)]
pub struct Queue {
  path: PathBuf,
}

impl Queue {
  /// Open (or lazily create) the queue file for `flake_id`.
  ///
  /// The parent directory is created eagerly so subsequent `append` calls
  /// see a stable target. The file itself is created on first append.
  ///
  /// # Errors
  ///
  /// Returns an error only when the queue directory cannot be created.
  pub fn open(flake_id: &str) -> Result<Self> {
    let dir = queue_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{flake_id}.jsonl"));
    Ok(Self { path })
  }

  /// Path of the on-disk queue file. Exposed for tests and diagnostics.
  #[must_use]
  pub fn path(&self) -> &std::path::Path {
    &self.path
  }

  /// Append one entry to the queue file.
  ///
  /// Uses `O_APPEND` + one `write_all` of `serialized || '\n'` so parallel
  /// shells interleave at line boundaries.
  ///
  /// If the queue file has already grown past `INGRESS_MAX_BYTES` the
  /// append is dropped silently. That guards a hostile edge case where a
  /// stuck client fails every tick without ever running `refine_and_flush`,
  /// letting the JSONL grow unboundedly — the refine-time cap on
  /// [`MAX_ENTRIES`] alone doesn't cover ingress.
  ///
  /// # Errors
  ///
  /// Returns an error if serialisation or the write fails.
  pub fn append(&self, entry: &QueueEntry) -> Result<()> {
    if let Ok(meta) = fs::metadata(&self.path)
      && meta.len() > INGRESS_MAX_BYTES
    {
      return Ok(());
    }

    let mut line = serde_json::to_string(entry)?;
    line.push('\n');

    let mut f = OpenOptions::new()
      .create(true)
      .append(true)
      .open(&self.path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
  }

  /// Load and parse every entry from the queue file.
  ///
  /// If any line fails to parse the whole file is quarantined by renaming
  /// to `<path>.corrupt.<epoch_ms>` and an empty vec is returned. That
  /// matches the "queue corruption is quarantined" spec clause: no data loss
  /// beyond the corrupted file is acceptable.
  ///
  /// # Errors
  ///
  /// Returns an error only for I/O beyond a missing file (a missing file
  /// yields an empty vec).
  pub fn load(&self) -> Result<Vec<QueueEntry>> {
    let raw = match fs::read_to_string(&self.path) {
      Ok(s) => s,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
        return Ok(Vec::new());
      },
      Err(e) => return Err(e.into()),
    };

    let mut out = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
      if line.trim().is_empty() {
        continue;
      }
      match serde_json::from_str::<QueueEntry>(line) {
        Ok(e) => out.push(e),
        Err(err) => {
          warn!(
            path = %self.path.display(),
            line_no = idx + 1,
            error = %err,
            "queue line failed to parse; quarantining file"
          );
          if let Err(e) = self.quarantine() {
            warn!(error = %e, "failed to quarantine corrupt queue file");
          }
          return Ok(Vec::new());
        },
      }
    }
    Ok(out)
  }

  /// Rename the queue file to a `.corrupt.<epoch_ms>` sibling.
  ///
  /// Best-effort: if the file no longer exists (raced with another shell
  /// that already quarantined it) the call succeeds silently.
  ///
  /// # Errors
  ///
  /// Returns an error only when the rename itself fails for a reason other
  /// than the source being absent.
  pub fn quarantine(&self) -> Result<()> {
    let ts = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |d| d.as_millis());
    let mut target = self.path.clone();
    let name = self
      .path
      .file_name()
      .ok_or_else(|| eyre!("queue path has no file name"))?
      .to_string_lossy()
      .into_owned();
    target.set_file_name(format!("{name}.corrupt.{ts}"));
    match fs::rename(&self.path, &target) {
      Ok(()) => {
        debug!(
          from = %self.path.display(),
          to = %target.display(),
          "queue quarantined"
        );
        Ok(())
      },
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(e) => Err(e.into()),
    }
  }

  /// Refine a queue: dedup queries + commands and drop stale entries.
  ///
  /// Pure function; does not touch the filesystem. Semantics:
  ///
  /// * Duplicate `Query` values collapse to the *most recent* entry
  ///   (highest `created_at`).
  /// * Duplicate `Command`s (matched by the command's
  ///   [`IdempotencyKey`]) collapse to the *earliest* entry — that is the
  ///   request that has been waiting longest and whose id is echoed to
  ///   any external observer.
  /// * Entries older than [`STALE_AFTER`] are dropped.
  /// * On overflow past [`MAX_ENTRIES`] the *oldest* entries are dropped.
  ///
  /// Ordering of the returned vec is by `created_at` ascending so callers
  /// dispatch in FIFO order.
  #[must_use]
  pub fn refine(entries: Vec<QueueEntry>) -> Vec<QueueEntry> {
    let now = SystemTime::now();

    // Drop stale entries first.
    let mut fresh: Vec<QueueEntry> = entries
      .into_iter()
      .filter(|e| {
        now
          .duration_since(e.created_at)
          .map_or(true, |age| age < STALE_AFTER) // clock skew — keep the entry
      })
      .collect();

    // Sort by created_at ascending so "earliest" and "most recent"
    // semantics below are unambiguous.
    fresh.sort_by_key(|e| e.created_at);

    // Two-pass dedup: track the winning entry per key, then rebuild.
    let mut query_pick: std::collections::HashMap<String, QueueEntry> =
      std::collections::HashMap::new();
    let mut command_pick: std::collections::HashMap<
      IdempotencyKey,
      QueueEntry,
    > = std::collections::HashMap::new();

    for entry in fresh {
      match &entry.payload {
        QueuePayload::Query(q) => {
          let key = query_dedup_key(q);
          // Keep the *most recent* — later iteration wins because we
          // sorted ascending.
          query_pick.insert(key, entry);
        },
        QueuePayload::Command(c) => {
          let key = command_key(c);
          // Keep the *earliest* — only insert if the slot is empty.
          command_pick.entry(key).or_insert(entry);
        },
      }
    }

    let mut refined: Vec<QueueEntry> = query_pick
      .into_values()
      .chain(command_pick.into_values())
      .collect();
    refined.sort_by_key(|e| e.created_at);

    // Bound: keep the newest MAX_ENTRIES, drop the oldest overflow.
    if refined.len() > MAX_ENTRIES {
      let drop = refined.len() - MAX_ENTRIES;
      refined.drain(0..drop);
    }

    refined
  }

  /// Atomically rewrite the queue with `entries`.
  ///
  /// Writes to `<path>.tmp` then renames over the target so a mid-write
  /// crash never leaves a truncated JSONL for another shell to read.
  ///
  /// # Errors
  ///
  /// Returns an error if serialisation, the temp write, or the rename fails.
  pub fn atomic_rewrite(&self, entries: &[QueueEntry]) -> Result<()> {
    if let Some(parent) = self.path.parent() {
      fs::create_dir_all(parent)?;
    }
    let mut tmp = self.path.clone();
    let name = self
      .path
      .file_name()
      .ok_or_else(|| eyre!("queue path has no file name"))?
      .to_string_lossy()
      .into_owned();
    tmp.set_file_name(format!("{name}.tmp"));

    let mut buf = String::new();
    for entry in entries {
      let line = serde_json::to_string(entry)?;
      buf.push_str(&line);
      buf.push('\n');
    }

    {
      let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)?;
      f.write_all(buf.as_bytes())?;
      // No `sync_all`: the queue is best-effort deferred work whose
      // corruption path is already quarantined on read (see `load`); a
      // fdatasync per rewrite adds up to 30 ms on rotational disks and
      // would blow the 100 ms prompt-tick budget.
    }
    fs::rename(&tmp, &self.path)?;
    Ok(())
  }
}

/// Base directory holding per-flake hook queue files.
fn queue_dir() -> PathBuf {
  crate::dirs::xi_state_root().join("hook-queue")
}

/// Dedup key for a query.
///
/// We serialise via `serde_json` and use the JSON text as the map key. That
/// avoids leaking `Query`-specific match ladders here — any future variant
/// automatically dedups on its wire shape.
fn query_dedup_key(q: &Query) -> String {
  serde_json::to_string(q)
    .unwrap_or_else(|_| String::from("__unserializable__"))
}

/// Idempotency key of a command.
///
/// The daemon's own dedup is driven by [`IdempotencyKey`]; matching that
/// here means "the same command that would attach on the daemon" also
/// collapses in the client queue.
const fn command_key(c: &Command) -> IdempotencyKey {
  match c {
    Command::EvalDevshell { key, .. }
    | Command::Restart { key }
    | Command::AbortJob { key, .. }
    | Command::InvalidateCache { key, .. } => *key,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::MutexGuard;
  use tempfile::TempDir;

  /// Guard bundling the temp dir with the env-lock guard so both drop at
  /// end of scope.
  struct StateHomeGuard {
    _td: TempDir,
    _lock: MutexGuard<'static, ()>,
  }

  /// Redirect `state_base()` (and therefore `queue_dir()`) into a temp dir
  /// by setting `XDG_STATE_HOME`. Serialises against every test in this
  /// crate that touches the same env var.
  fn with_state_home() -> StateHomeGuard {
    let lock = crate::hook::test_env::XDG_LOCK
      .lock()
      .unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().expect("mktemp");
    // SAFETY: guarded by hook::test_env::XDG_LOCK for the duration.
    unsafe {
      std::env::set_var("XDG_STATE_HOME", td.path());
    }
    StateHomeGuard {
      _td: td,
      _lock: lock,
    }
  }

  fn sample_query() -> Query {
    Query::HeartBeat
  }

  fn sample_status_query() -> Query {
    Query::Status
  }

  fn sample_command() -> Command {
    Command::EvalDevshell {
      target: "default".to_owned(),
      key: IdempotencyKey::placeholder(),
    }
  }

  fn sample_command_key(bytes: [u8; 32]) -> Command {
    Command::EvalDevshell {
      target: "default".to_owned(),
      key: IdempotencyKey::from_bytes(bytes),
    }
  }

  #[test]
  fn append_load_roundtrip() {
    let _td = with_state_home();
    let q = Queue::open("fid1").expect("open");
    let e1 = QueueEntry::new(QueuePayload::Query(sample_query()));
    let e2 = QueueEntry::new(QueuePayload::Command(sample_command()));
    q.append(&e1).expect("append 1");
    q.append(&e2).expect("append 2");

    let loaded = q.load().expect("load");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].id, e1.id);
    assert_eq!(loaded[1].id, e2.id);
  }

  #[test]
  fn corruption_quarantines_and_yields_empty() {
    let _td = with_state_home();
    let q = Queue::open("fid2").expect("open");
    // Write one good line + one malformed line.
    let good = QueueEntry::new(QueuePayload::Query(sample_query()));
    q.append(&good).expect("append good");
    {
      let mut f = OpenOptions::new()
        .append(true)
        .open(q.path())
        .expect("open for corrupt append");
      f.write_all(b"{not json\n").expect("write corrupt");
    }

    let loaded = q.load().expect("load");
    assert!(loaded.is_empty(), "corrupt load must yield empty");

    // The original file was renamed away; a fresh load should still yield
    // empty (no file present).
    assert!(!q.path().exists());
    let siblings: Vec<_> = fs::read_dir(q.path().parent().expect("parent"))
      .expect("readdir")
      .filter_map(std::result::Result::ok)
      .map(|e| e.file_name().to_string_lossy().into_owned())
      .filter(|n| n.contains(".corrupt."))
      .collect();
    assert_eq!(siblings.len(), 1, "expected exactly one quarantined file");
  }

  #[test]
  fn refine_dedups_duplicate_queries_keeping_newest() {
    // Two identical queries; the newest one (later created_at) wins.
    // Pin timestamps relative to `now` so nothing gets filtered as stale.
    let now = SystemTime::now();
    let old = QueueEntry {
      id: "01".to_owned(),
      created_at: now - Duration::from_secs(30),
      payload: QueuePayload::Query(sample_query()),
    };
    let new = QueueEntry {
      id: "02".to_owned(),
      created_at: now - Duration::from_secs(10),
      payload: QueuePayload::Query(sample_query()),
    };
    let other = QueueEntry {
      id: "03".to_owned(),
      created_at: now - Duration::from_secs(20),
      payload: QueuePayload::Query(sample_status_query()),
    };

    let refined = Queue::refine(vec![old, new.clone(), other.clone()]);
    assert_eq!(refined.len(), 2);
    // Ordered ascending by created_at: other (older) then new (newer).
    assert_eq!(refined[0].id, other.id);
    assert_eq!(refined[1].id, new.id);
  }

  #[test]
  fn refine_dedups_duplicate_commands_keeping_earliest() {
    let key_a = [0xAA; 32];
    let now = SystemTime::now();
    let earliest = QueueEntry {
      id: "e".to_owned(),
      created_at: now - Duration::from_secs(30),
      payload: QueuePayload::Command(sample_command_key(key_a)),
    };
    let later = QueueEntry {
      id: "l".to_owned(),
      created_at: now - Duration::from_secs(10),
      payload: QueuePayload::Command(sample_command_key(key_a)),
    };
    let refined = Queue::refine(vec![earliest.clone(), later]);
    assert_eq!(refined.len(), 1);
    assert_eq!(refined[0].id, earliest.id);
  }

  #[test]
  fn refine_drops_stale_entries() {
    let ancient = QueueEntry {
      id: "ancient".to_owned(),
      created_at: SystemTime::now() - (STALE_AFTER + Duration::from_secs(10)),
      payload: QueuePayload::Query(sample_query()),
    };
    let fresh = QueueEntry::new(QueuePayload::Query(sample_status_query()));
    let refined = Queue::refine(vec![ancient, fresh.clone()]);
    assert_eq!(refined.len(), 1);
    assert_eq!(refined[0].id, fresh.id);
  }

  #[test]
  fn refine_bounds_at_max_entries_dropping_oldest() {
    // Build MAX_ENTRIES + 3 distinct commands (distinct keys so nothing
    // dedups), each with a monotonically increasing timestamp.
    let mut entries = Vec::new();
    for i in 0..(MAX_ENTRIES + 3) {
      let mut key = [0u8; 32];
      key[0] = u8::try_from(i & 0xFF).unwrap_or(0);
      key[1] = u8::try_from((i >> 8) & 0xFF).unwrap_or(0);
      entries.push(QueueEntry {
        id: format!("cmd-{i}"),
        created_at: SystemTime::now()
          - Duration::from_secs(
            u64::try_from(MAX_ENTRIES + 3 - i).unwrap_or(0),
          ),
        payload: QueuePayload::Command(sample_command_key(key)),
      });
    }
    let refined = Queue::refine(entries);
    assert_eq!(refined.len(), MAX_ENTRIES);
    // The three oldest (cmd-0, cmd-1, cmd-2) must have been dropped.
    for e in &refined {
      assert!(!e.id.starts_with("cmd-0"));
      assert_ne!(e.id, "cmd-1");
      assert_ne!(e.id, "cmd-2");
    }
  }

  #[test]
  fn atomic_rewrite_is_observable() {
    let _td = with_state_home();
    let q = Queue::open("fid3").expect("open");

    // Append two entries.
    let e1 = QueueEntry::new(QueuePayload::Query(sample_query()));
    let e2 = QueueEntry::new(QueuePayload::Query(sample_status_query()));
    q.append(&e1).expect("append 1");
    q.append(&e2).expect("append 2");
    assert_eq!(q.load().expect("load").len(), 2);

    // Rewrite with a single entry.
    let only = QueueEntry::new(QueuePayload::Query(sample_query()));
    q.atomic_rewrite(&[only.clone()]).expect("rewrite");
    let after = q.load().expect("load after");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, only.id);
  }
}
