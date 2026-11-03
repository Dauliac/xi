//! The single per-`SourcesId` advisory lock (`xi-sources-race`).
//!
//! Three actors can mutate one sources dir on one host at the same time: the
//! gitops daemon ticking, an interactive `xi <scope> switch`, and an inbound
//! `xi push` naming this host as the target. Every one of them takes this
//! lock before its first write. Readers never take it.
//!
//! # What is on disk
//!
//! | path                            | role                                   |
//! |---------------------------------|----------------------------------------|
//! | `<state_dir>/lock`              | the lock itself + the holder's record  |
//! | `<state_dir>/lock.alive`        | liveness ping, mtime is the signal     |
//! | `<state_dir>/lock.waiters/<pid>`| one file per waiter, body is a record  |
//! | `<state_dir>/lock.waiters/.grant`| pid the releasing holder chose next   |
//!
//! # Why `flock(2)` and not `fcntl(2)` record locks
//!
//! The proposal says "fcntl advisory locking", meaning the POSIX advisory
//! family rather than one specific syscall. This module uses `flock(2)`
//! through [`nix::fcntl::Flock`] for two reasons that matter here:
//!
//! - `flock` locks attach to the *open file description*, so two independent
//!   `open` calls contend even inside one process. `fcntl` record locks are
//!   per-process: a second `F_SETLK` from the same pid silently succeeds and
//!   replaces the first. A daemon that forked a worker, or a test that drives
//!   two actors on two threads, would see no mutual exclusion at all.
//! - Closing an unrelated fd to the same file drops every `fcntl` lock the
//!   process held on it, and a reader opening the lock file to see who holds it
//!   would be enough to do that.
//!
//! Neither syscall survives the *path* being replaced, though: both lock an
//! inode. That is why the holder record is written through the locked fd
//! rather than by the atomic rename the rest of the state dir uses - see
//! `write_record_in_place` below.
//!
//! # What "steal" means against a real kernel lock
//!
//! The spec's steal protocol reads as though a waiter can revoke a held lock.
//! It cannot: `flock` has no revocation, and forcing one would defeat the
//! mutual exclusion the lock exists for. The two cases the protocol actually
//! covers are:
//!
//! - **Crashed holder.** The kernel released its `flock` when the process died,
//!   but `<state_dir>/lock` still names it. The record is a ghost. A new
//!   acquirer waits out the grace period, confirms the record did not change,
//!   takes the (already free) kernel lock, and logs both actors. This is the
//!   case SC-E-003 describes.
//! - **Hung holder.** The pid is alive and really holds the kernel lock, but
//!   has not refreshed `lock.alive` for [`ALIVE_STALE_AFTER`]. There is nothing
//!   to steal. The acquirer warns loudly and keeps waiting; on timeout it
//!   returns [`LockError::Hung`] so the operator is told which pid to kill
//!   rather than being left to guess. It does *not* correct the misleading
//!   record: only the holder writes `<state_dir>/lock`, for the reason
//!   `write_record_in_place` gives.
//!
//! [`SourcesLock::stolen_from`] reports which, if either, happened.

use std::{
  fmt,
  fs,
  io,
  io::{Seek as _, Write as _},
  path::{Path, PathBuf},
  process,
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  thread::{self, JoinHandle},
  time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, SecondsFormat, Utc};
use nix::{
  errno::Errno,
  fcntl::{Flock, FlockArg},
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// On-disk names and the spec's timings
// ---------------------------------------------------------------------------

/// The lock file, holding the current holder's record.
pub const LOCK_FILE: &str = "lock";

/// Liveness sidecar. Only its mtime carries information.
pub const ALIVE_FILE: &str = "lock.alive";

/// Directory where waiters announce themselves, one file per pid.
pub const WAITERS_DIR: &str = "lock.waiters";

/// Marker inside [`WAITERS_DIR`] naming the pid a releasing holder picked.
///
/// Dot-prefixed so [`read_waiters`] can tell it apart from a waiter file
/// without a second naming rule.
const GRANT_FILE: &str = ".grant";

/// Default lease: how long a holder may hold before waiters may act on
/// staleness. A holder needing longer refreshes [`ALIVE_FILE`] instead.
pub const DEFAULT_LEASE: Duration = Duration::from_mins(15);

/// How often a holder touches [`ALIVE_FILE`] while it holds the lock.
pub const ALIVE_REFRESH_INTERVAL: Duration = Duration::from_mins(1);

/// An [`ALIVE_FILE`] older than this means the holder is hung.
///
/// One and a half refresh intervals, so a single missed ping is not an
/// accusation.
pub const ALIVE_STALE_AFTER: Duration = Duration::from_secs(90);

/// Grace period before a record naming a dead pid is stolen.
pub const DEAD_PID_GRACE: Duration = Duration::from_secs(10);

/// How long a grant binds the queue.
///
/// Bounded so a grantee that dies between being granted and acquiring
/// cannot wedge every other waiter.
pub const GRANT_TTL: Duration = Duration::from_secs(5);

/// How often a blocked acquirer re-checks the lock.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// ActorName  -  who is allowed to hold the lock, and in what order
// ---------------------------------------------------------------------------

/// The closed set of actors that mutate a sources dir.
///
/// Closed on purpose: the priority order below is only meaningful if every
/// contender is named here, and a new mutating actor is a spec change rather
/// than a string literal.
#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  Hash,
  PartialOrd,
  Ord,
  Serialize,
  Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ActorName {
  /// Interactive `xi <scope> switch`.
  Switch,
  /// Interactive `xi <scope> sources sync`.
  Sync,
  /// An inbound `xi push` from an operator's workstation.
  Push,
  /// First-run adoption of a fresh machine.
  Bootstrap,
  /// A gitops daemon tick.
  Daemon,
  /// Background cache warming.
  CachePrestage,
}

impl ActorName {
  /// Every actor, in priority order.
  pub const ALL: &'static [Self] = &[
    Self::Switch,
    Self::Sync,
    Self::Push,
    Self::Bootstrap,
    Self::Daemon,
    Self::CachePrestage,
  ];

  /// The wire spelling, as it appears in a lock record.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Switch => "switch",
      Self::Sync => "sync",
      Self::Push => "push",
      Self::Bootstrap => "bootstrap",
      Self::Daemon => "daemon",
      Self::CachePrestage => "cache-prestage",
    }
  }

  /// Parse a wire spelling back, or `None` for anything unrecognised.
  #[must_use]
  pub fn parse(raw: &str) -> Option<Self> {
    Self::ALL.iter().copied().find(|a| a.as_str() == raw)
  }

  /// Queue rank, lowest first.
  ///
  /// A human waiting at a terminal outranks everything; an inbound push
  /// outranks the machine's own background work; a daemon tick will come
  /// round again on its own. `cache-prestage` is below the daemon because it
  /// only warms a cache: nothing observable regresses if it waits. The spec's
  /// priority list names the first five; the sixth is placed here by that
  /// same reasoning.
  #[must_use]
  pub const fn priority(self) -> u8 {
    match self {
      Self::Switch => 0,
      Self::Sync => 1,
      Self::Push => 2,
      Self::Bootstrap => 3,
      Self::Daemon => 4,
      Self::CachePrestage => 5,
    }
  }
}

impl fmt::Display for ActorName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

// ---------------------------------------------------------------------------
// LockRecord  -  the one JSON line
// ---------------------------------------------------------------------------

/// The holder's self-description, written as one JSON line.
///
/// Exactly four fields, by spec, so a stuck lock is diagnosable with `cat`
/// and nothing else. The same shape is reused for waiter registrations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
  /// Pid of the holding (or waiting) process.
  pub pid:        u32,
  /// Which actor this is.
  pub actor_name: ActorName,
  /// When it acquired (or started waiting), ISO-8601 to the second.
  pub started_at: String,
  /// One human-readable line saying what it is doing.
  pub intent:     String,
}

impl LockRecord {
  /// A record for this process, stamped now.
  #[must_use]
  pub fn now(actor_name: ActorName, intent: impl Into<String>) -> Self {
    Self {
      pid: process::id(),
      actor_name,
      started_at: iso8601_now(),
      intent: intent.into(),
    }
  }

  /// Serialise to the single JSON line that goes on disk.
  ///
  /// # Errors
  ///
  /// Only if `intent` contains something `serde_json` cannot encode, which
  /// `String` cannot; the signature keeps the caller honest regardless.
  pub fn to_line(&self) -> io::Result<String> {
    serde_json::to_string(self).map_err(io::Error::other)
  }

  /// Parse a record, treating blank content as "no holder".
  ///
  /// # Errors
  ///
  /// [`LockError::Malformed`] when the content is neither blank nor a record.
  pub fn parse(path: &Path, raw: &str) -> Result<Option<Self>, LockError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
      return Ok(None);
    }
    serde_json::from_str(trimmed).map(Some).map_err(|source| {
      LockError::Malformed {
        path: path.to_path_buf(),
        source,
      }
    })
  }

  /// `started_at` as a timestamp, or `None` if it is not parseable.
  #[must_use]
  pub fn started_at_utc(&self) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&self.started_at)
      .ok()
      .map(|t| t.with_timezone(&Utc))
  }

  /// How long ago this record was stamped, or `None` if unparsable.
  #[must_use]
  pub fn age(&self) -> Option<Duration> {
    let started = self.started_at_utc()?;
    (Utc::now() - started).to_std().ok()
  }

  /// When the holder should be done, assuming it uses the whole lease.
  #[must_use]
  pub fn estimated_completion(&self, lease: Duration) -> Option<DateTime<Utc>> {
    let lease = chrono::Duration::from_std(lease).ok()?;
    self.started_at_utc()?.checked_add_signed(lease)
  }

  /// Seconds left on `lease`, saturating at zero.
  #[must_use]
  pub fn estimated_wait(&self, lease: Duration) -> Option<Duration> {
    let age = self.age()?;
    Some(lease.saturating_sub(age))
  }
}

impl fmt::Display for LockRecord {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{} (pid {}) since {}: {}",
      self.actor_name, self.pid, self.started_at, self.intent
    )
  }
}

/// Now, to the second, in the spelling the spec pins.
fn iso8601_now() -> String {
  Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// LockError
// ---------------------------------------------------------------------------

/// Why a lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
  /// The wait budget ran out while a live holder still held the lock.
  #[error("{}", held_message(.holder, *.waited, *.lease))]
  Held {
    /// The holder's own record.
    holder: LockRecord,
    /// How long this acquirer waited before giving up.
    waited: Duration,
    /// The lease the estimate was computed against.
    lease:  Duration,
  },

  /// The holder is alive but has stopped refreshing its liveness file.
  #[error("{}", hung_message(.holder, *.alive_age))]
  Hung {
    /// The holder's own record.
    holder:    LockRecord,
    /// How long since the liveness file was last touched.
    alive_age: Duration,
  },

  /// The lock is held but nobody wrote a record, so there is no one to name.
  #[error(
    "the sources lock is held but carries no holder record; waited {}",
    humantime::format_duration(*.waited)
  )]
  Busy {
    /// How long this acquirer waited before giving up.
    waited: Duration,
  },

  /// The lock file contains something other than a record.
  #[error("{}: not a lock record: {source}", path.display())]
  Malformed {
    /// The file that could not be parsed.
    path:   PathBuf,
    /// The underlying parse failure.
    #[source]
    source: serde_json::Error,
  },

  /// Anything the filesystem refused.
  #[error(transparent)]
  Io(#[from] io::Error),
}

impl LockError {
  /// The holder's record, when this failure has one.
  #[must_use]
  pub const fn holder(&self) -> Option<&LockRecord> {
    match self {
      Self::Held { holder, .. } | Self::Hung { holder, .. } => Some(holder),
      Self::Busy { .. } | Self::Malformed { .. } | Self::Io(_) => None,
    }
  }

  /// The short machine-readable reason a mutating command reports.
  ///
  /// SC-E-001 asks a blocked `switch` to say `lock-held-by-daemon` rather
  /// than a bare "busy", so the reason names the actor it lost to.
  #[must_use]
  pub fn reason(&self) -> String {
    self.holder().map_or_else(
      || "lock-unavailable".to_owned(),
      |holder| format!("lock-held-by-{}", holder.actor_name),
    )
  }
}

/// SC-E-007: name the holder and the wait, never just "busy".
fn held_message(
  holder: &LockRecord,
  waited: Duration,
  lease: Duration,
) -> String {
  let estimate =
    holder
      .estimated_completion(lease)
      .map_or_else(String::new, |done| {
        format!(
          "\n  estimated completion: {} (lease {})",
          done.to_rfc3339_opts(SecondsFormat::Secs, true),
          humantime::format_duration(lease)
        )
      });
  format!(
    "the sources lock is held by another actor; waited {}.\n  held by: {}\n  \
     pid:     {}\n  since:   {}\n  intent:  {}{estimate}\n\nRe-run when it \
     finishes, or inspect it with `xi <scope> sources status`.",
    humantime::format_duration(waited),
    holder.actor_name,
    holder.pid,
    holder.started_at,
    holder.intent,
  )
}

/// A hung holder is an operator action, so say which pid to look at.
fn hung_message(holder: &LockRecord, alive_age: Duration) -> String {
  format!(
    "the sources lock holder is hung: pid {} ({}) has not refreshed \
     `{ALIVE_FILE}` for {}.\n  since:  {}\n  intent: {}\n\nThe kernel lock is \
     still held, so it cannot be taken away. Check the process and kill it if \
     it is wedged.",
    holder.pid,
    holder.actor_name,
    humantime::format_duration(alive_age),
    holder.started_at,
    holder.intent,
  )
}

// ---------------------------------------------------------------------------
// Liveness
// ---------------------------------------------------------------------------

/// What a waiter concluded about the current holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Liveness {
  /// Pid is running and the liveness file is fresh.
  Alive,
  /// The pid in the record no longer exists.
  DeadPid,
  /// Pid exists but has stopped refreshing [`ALIVE_FILE`].
  Hung {
    /// Age of the liveness signal that triggered this.
    alive_age: Duration,
  },
}

/// Does `pid` name a running process?
///
/// `/proc/<pid>` on Linux, `kill(pid, 0)` elsewhere, as the spec's steal
/// protocol spells it. A zombie still has a `/proc` entry, which is correct:
/// an unreaped child has not released its `flock` responsibilities to anyone
/// else either.
#[must_use]
pub fn pid_is_running(pid: u32) -> bool {
  #[cfg(target_os = "linux")]
  {
    Path::new("/proc").join(pid.to_string()).exists()
  }
  #[cfg(all(unix, not(target_os = "linux")))]
  {
    let Ok(raw) = i32::try_from(pid) else {
      return false;
    };
    // SAFETY: `kill` with signal 0 performs the permission and existence
    // check without delivering anything. No memory is touched.
    if unsafe { nix::libc::kill(raw, 0) } == 0 {
      return true;
    }
    // EPERM means "exists, but not yours" - still running.
    Errno::last() == Errno::EPERM
  }
  #[cfg(not(unix))]
  {
    let _ = pid;
    true
  }
}

/// Age of the liveness file, or `None` when there is not one.
#[must_use]
pub fn alive_age(state_dir: &Path) -> Option<Duration> {
  let mtime = fs::metadata(state_dir.join(ALIVE_FILE))
    .and_then(|m| m.modified())
    .ok()?;
  SystemTime::now().duration_since(mtime).ok()
}

/// Classify the current holder.
///
/// When the liveness file is missing altogether the record's own age stands
/// in for it, so a holder that died before its first ping is not immune to
/// the staleness check.
#[must_use]
pub fn holder_liveness(state_dir: &Path, holder: &LockRecord) -> Liveness {
  if !pid_is_running(holder.pid) {
    return Liveness::DeadPid;
  }
  let age = alive_age(state_dir)
    .or_else(|| holder.age())
    .unwrap_or_default();
  if age > ALIVE_STALE_AFTER {
    Liveness::Hung { alive_age: age }
  } else {
    Liveness::Alive
  }
}

// ---------------------------------------------------------------------------
// Waiter queue
// ---------------------------------------------------------------------------

/// Every live waiter registration, unparsable files skipped.
///
/// # Errors
///
/// Propagates I/O errors other than a missing waiters directory, which is
/// simply an empty queue.
pub fn read_waiters(waiters_dir: &Path) -> io::Result<Vec<LockRecord>> {
  let entries = match fs::read_dir(waiters_dir) {
    Ok(entries) => entries,
    Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(err) => return Err(err),
  };

  let mut out = Vec::new();
  for entry in entries {
    let entry = entry?;
    let name = entry.file_name().to_string_lossy().into_owned();
    if name.starts_with('.') {
      continue;
    }
    let raw = match fs::read_to_string(entry.path()) {
      Ok(raw) => raw,
      // A waiter that deregistered mid-scan is not an error.
      Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
      Err(err) => return Err(err),
    };
    match serde_json::from_str::<LockRecord>(raw.trim()) {
      Ok(record) => out.push(record),
      Err(err) => debug!("ignoring unparsable waiter `{name}`: {err}"),
    }
  }
  Ok(out)
}

/// The waiter that should go next.
///
/// Priority first, then FIFO by `started_at`. `started_at` is second-
/// granular and the spelling is ISO-8601, so a plain string compare is
/// already chronological; pid breaks the remaining ties so two waiters that
/// registered in the same second still get a total order rather than
/// whichever `read_dir` happened to yield first.
#[must_use]
pub fn next_waiter(waiters: &[LockRecord]) -> Option<&LockRecord> {
  waiters.iter().min_by(|a, b| {
    a.actor_name
      .priority()
      .cmp(&b.actor_name.priority())
      .then_with(|| a.started_at.cmp(&b.started_at))
      .then_with(|| a.pid.cmp(&b.pid))
  })
}

/// A waiter registration that deregisters itself on drop.
struct WaiterGuard {
  path: PathBuf,
}

impl WaiterGuard {
  fn register(waiters_dir: &Path, record: &LockRecord) -> io::Result<Self> {
    let path = waiters_dir.join(record.pid.to_string());
    fs::write(&path, record.to_line()?)?;
    super::harden_file(&path)?;
    Ok(Self { path })
  }
}

impl Drop for WaiterGuard {
  fn drop(&mut self) {
    if let Err(err) = fs::remove_file(&self.path)
      && err.kind() != io::ErrorKind::NotFound
    {
      debug!("could not deregister waiter {}: {err}", self.path.display());
    }
  }
}

/// May this waiter attempt the kernel lock on this poll?
///
/// `flock` wakes whichever blocked waiter the kernel feels like. This gate is
/// what turns that into the spec's priority order: a releasing holder names
/// its chosen successor in [`GRANT_FILE`], and while that grant is fresh and
/// names somebody else who is still alive, everyone else stands down.
fn grant_allows(waiters_dir: &Path, me: u32) -> bool {
  let path = waiters_dir.join(GRANT_FILE);
  let Ok(raw) = fs::read_to_string(&path) else {
    return true;
  };
  let Ok(granted) = raw.trim().parse::<u32>() else {
    return true;
  };
  if granted == me {
    return true;
  }
  let expired = fs::metadata(&path)
    .and_then(|m| m.modified())
    .ok()
    .and_then(|m| SystemTime::now().duration_since(m).ok())
    .is_none_or(|age| age > GRANT_TTL);
  if expired {
    return true;
  }
  // A grantee that died before acquiring must not wedge the queue.
  !pid_is_running(granted)
}

/// Name the highest-priority waiter as the next holder.
///
/// `exclude` drops the releasing holder's own pid: in production it already
/// removed its waiter file on acquisition, but a process driving two actors
/// on two threads shares one pid and would otherwise grant the lock to
/// itself.
fn grant_next(waiters_dir: &Path, exclude: u32) -> io::Result<Option<u32>> {
  let live: Vec<LockRecord> = read_waiters(waiters_dir)?
    .into_iter()
    .filter(|w| w.pid != exclude && pid_is_running(w.pid))
    .collect();

  let path = waiters_dir.join(GRANT_FILE);
  match next_waiter(&live) {
    Some(chosen) => {
      debug!("granting the sources lock to {chosen}");
      fs::write(&path, chosen.pid.to_string())?;
      Ok(Some(chosen.pid))
    },
    None => {
      // Nobody waiting: clear the grant so the next arrival is not gated on
      // a stale name for up to GRANT_TTL.
      match fs::remove_file(&path) {
        Ok(()) => Ok(None),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
      }
    },
  }
}

// ---------------------------------------------------------------------------
// Acquisition options
// ---------------------------------------------------------------------------

/// How one actor wants to take the lock.
#[derive(Debug, Clone)]
pub struct LockOptions {
  /// Which actor is asking.
  pub actor:          ActorName,
  /// One line describing what it is about to do.
  pub intent:         String,
  /// How long to block before giving up.
  pub wait:           Duration,
  /// Lease this actor claims; drives the wait estimate it reports.
  pub lease:          Duration,
  /// Grace before a record naming a dead pid is stolen.
  pub dead_pid_grace: Duration,
  /// Keep [`ALIVE_FILE`] fresh on a background thread while held.
  pub refresh:        bool,
}

impl LockOptions {
  /// Spec defaults: wait up to one lease, 15-minute lease, 10-second grace,
  /// liveness refreshed for you.
  #[must_use]
  pub fn new(actor: ActorName, intent: impl Into<String>) -> Self {
    Self {
      actor,
      intent: intent.into(),
      wait: DEFAULT_LEASE,
      lease: DEFAULT_LEASE,
      dead_pid_grace: DEAD_PID_GRACE,
      refresh: true,
    }
  }

  /// Give up after `wait` instead of after a full lease.
  #[must_use]
  pub const fn wait_for(mut self, wait: Duration) -> Self {
    self.wait = wait;
    self
  }

  /// Claim a different lease.
  #[must_use]
  pub const fn lease(mut self, lease: Duration) -> Self {
    self.lease = lease;
    self
  }

  /// Shorten the dead-pid grace. Tests do this; production should not.
  #[must_use]
  pub const fn dead_pid_grace(mut self, grace: Duration) -> Self {
    self.dead_pid_grace = grace;
    self
  }

  /// Take responsibility for calling [`SourcesLock::ping`] by hand.
  #[must_use]
  pub const fn without_refresh(mut self) -> Self {
    self.refresh = false;
    self
  }
}

// ---------------------------------------------------------------------------
// The liveness keeper
// ---------------------------------------------------------------------------

/// Background thread touching [`ALIVE_FILE`] every
/// [`ALIVE_REFRESH_INTERVAL`].
///
/// A thread rather than a "remember to ping" contract because the operations
/// under this lock are `git fetch` and `nix build`: a single blocking call
/// that routinely outlasts the staleness window with nowhere to hook a ping.
struct AliveKeeper {
  stop:   Arc<AtomicBool>,
  handle: Option<JoinHandle<()>>,
}

impl AliveKeeper {
  fn spawn(state_dir: PathBuf) -> Self {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    // Short tick so release is prompt; the touch itself is rate-limited to
    // the refresh interval.
    let tick = Duration::from_millis(200);
    let handle = thread::Builder::new()
      .name("xi-sources-lock-alive".to_owned())
      .spawn(move || {
        let mut last = Instant::now();
        while !flag.load(Ordering::Relaxed) {
          thread::sleep(tick);
          if last.elapsed() >= ALIVE_REFRESH_INTERVAL {
            if let Err(err) = touch_alive(&state_dir) {
              warn!("could not refresh `{ALIVE_FILE}`: {err}");
            }
            last = Instant::now();
          }
        }
      })
      .ok();
    Self { stop, handle }
  }
}

impl Drop for AliveKeeper {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Relaxed);
    if let Some(handle) = self.handle.take() {
      drop(handle.join());
    }
  }
}

/// Stamp [`ALIVE_FILE`] with the current time.
fn touch_alive(state_dir: &Path) -> io::Result<()> {
  super::write_state_file(state_dir, ALIVE_FILE, "")
}

// ---------------------------------------------------------------------------
// SourcesLock
// ---------------------------------------------------------------------------

/// A held sources lock. Releases on drop.
///
/// Holding this value is the only permission to write to the sources dir or
/// to a non-metadata part of the state dir.
pub struct SourcesLock {
  /// `None` only between an explicit [`SourcesLock::release`] and drop.
  flock:       Option<Flock<fs::File>>,
  state_dir:   PathBuf,
  record:      LockRecord,
  lease:       Duration,
  stolen_from: Option<LockRecord>,
  keeper:      Option<AliveKeeper>,
}

impl fmt::Debug for SourcesLock {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SourcesLock")
      .field("state_dir", &self.state_dir)
      .field("record", &self.record)
      .field("stolen_from", &self.stolen_from)
      .finish_non_exhaustive()
  }
}

impl SourcesLock {
  /// Take the lock, blocking up to `opts.wait`.
  ///
  /// The acquirer registers itself in [`WAITERS_DIR`] for the whole wait, so
  /// the current holder can see it and grant the lock in priority order on
  /// release.
  ///
  /// # Errors
  ///
  /// [`LockError::Held`] when a live holder outlasted the wait budget,
  /// [`LockError::Hung`] when the holder stopped refreshing its liveness
  /// file, [`LockError::Busy`] when the lock is held with no record to name,
  /// [`LockError::Malformed`] for an unparsable record, and
  /// [`LockError::Io`] for anything the filesystem refused.
  pub fn acquire(
    state_dir: &Path,
    opts: &LockOptions,
  ) -> Result<Self, LockError> {
    fs::create_dir_all(state_dir)?;
    super::harden_dir(state_dir)?;
    let waiters_dir = state_dir.join(WAITERS_DIR);
    fs::create_dir_all(&waiters_dir)?;
    super::harden_dir(&waiters_dir)?;

    let lock_path = state_dir.join(LOCK_FILE);
    let record = LockRecord::now(opts.actor, opts.intent.clone());
    let waiter = WaiterGuard::register(&waiters_dir, &record)?;

    let started = Instant::now();
    let mut stale: Option<LockRecord> = None;

    loop {
      if !grant_allows(&waiters_dir, record.pid) {
        if started.elapsed() >= opts.wait {
          return Err(Self::give_up(state_dir, &lock_path, started, opts));
        }
        thread::sleep(POLL_INTERVAL);
        continue;
      }

      // Who claims it right now, and is that claim credible?
      if let Some(holder) = read_record(&lock_path)? {
        match holder_liveness(state_dir, &holder) {
          Liveness::Alive => {},
          Liveness::DeadPid => {
            debug!(
              "sources lock names pid {} which is gone; waiting {} before \
               stealing",
              holder.pid,
              humantime::format_duration(opts.dead_pid_grace)
            );
            // The grace exists because a holder mid-release has already
            // dropped the kernel lock but may not have truncated the record
            // yet. Re-reading after the pause distinguishes the two.
            thread::sleep(opts.dead_pid_grace);
            if read_record(&lock_path)?.as_ref() == Some(&holder) {
              stale = Some(holder);
            }
          },
          Liveness::Hung { alive_age } => {
            warn!(
              "sources lock holder {holder} has not refreshed `{ALIVE_FILE}` \
               for {}; treating the record as stale",
              humantime::format_duration(alive_age)
            );
            stale = Some(holder);
          },
        }
      }

      if let Some(flock) = try_flock(&lock_path)? {
        if let Some(previous) = stale.as_ref() {
          info!(
            "stealing the sources lock from {previous}; new holder {record}"
          );
        }
        return Self::claim(
          flock,
          state_dir,
          &waiters_dir,
          record,
          opts,
          stale,
          waiter,
        )
        .map_err(LockError::Io);
      }

      // The record is a ghost but somebody really holds the kernel lock,
      // so this process is not the one to tidy it: a non-holder must
      // never write `state_dir/lock`. The record is only a report, and
      // correcting a misleading report is not worth detaching the real
      // holder's lock from its inode, which is what any write here
      // would risk. The holder clears it on release.
      if let Some(previous) = stale.take() {
        warn!(
          "the sources lock record ({previous}) names a process that is gone, \
           but the kernel lock is still held by some other open handle; \
           leaving the record for its holder to clear"
        );
      }
      if started.elapsed() >= opts.wait {
        return Err(Self::give_up(state_dir, &lock_path, started, opts));
      }
      thread::sleep(POLL_INTERVAL);
    }
  }

  /// Take the lock or fail immediately.
  ///
  /// A dead-pid record still costs `opts.dead_pid_grace`: the spec makes the
  /// grace part of stealing, not part of waiting.
  ///
  /// # Errors
  ///
  /// As [`SourcesLock::acquire`].
  pub fn try_acquire(
    state_dir: &Path,
    opts: &LockOptions,
  ) -> Result<Self, LockError> {
    Self::acquire(state_dir, &opts.clone().wait_for(Duration::ZERO))
  }

  /// Finish acquisition: record, liveness, queue cleanup.
  fn claim(
    flock: Flock<fs::File>,
    state_dir: &Path,
    waiters_dir: &Path,
    record: LockRecord,
    opts: &LockOptions,
    stolen_from: Option<LockRecord>,
    waiter: WaiterGuard,
  ) -> io::Result<Self> {
    write_record_in_place(&flock, &record.to_line()?)?;
    touch_alive(state_dir)?;

    // We are no longer waiting, and the grant that let us through has been
    // consumed.
    drop(waiter);
    let grant = waiters_dir.join(GRANT_FILE);
    if fs::read_to_string(&grant)
      .is_ok_and(|raw| raw.trim() == record.pid.to_string())
    {
      drop(fs::remove_file(&grant));
    }

    debug!("acquired the sources lock as {record}");
    Ok(Self {
      flock: Some(flock),
      state_dir: state_dir.to_path_buf(),
      record,
      lease: opts.lease,
      stolen_from,
      keeper: opts
        .refresh
        .then(|| AliveKeeper::spawn(state_dir.to_owned())),
    })
  }

  /// Build the error for a wait that ran out, naming whoever is to blame.
  fn give_up(
    state_dir: &Path,
    lock_path: &Path,
    started: Instant,
    opts: &LockOptions,
  ) -> LockError {
    let waited = started.elapsed();
    let holder = match read_record(lock_path) {
      Ok(Some(holder)) => holder,
      Ok(None) => return LockError::Busy { waited },
      Err(err) => return err,
    };
    match holder_liveness(state_dir, &holder) {
      Liveness::Hung { alive_age } => LockError::Hung { holder, alive_age },
      Liveness::Alive | Liveness::DeadPid => {
        LockError::Held {
          holder,
          waited,
          lease: opts.lease,
        }
      },
    }
  }

  /// This holder's record.
  #[must_use]
  pub const fn record(&self) -> &LockRecord {
    &self.record
  }

  /// The record this acquisition stole from, if it stole one.
  #[must_use]
  pub const fn stolen_from(&self) -> Option<&LockRecord> {
    self.stolen_from.as_ref()
  }

  /// The lease this holder claimed.
  ///
  /// Not written to disk: the record carries exactly the four spec'd fields.
  /// A holder that wants to outlive the lease refreshes [`ALIVE_FILE`], which
  /// is the signal waiters actually act on.
  #[must_use]
  pub const fn lease(&self) -> Duration {
    self.lease
  }

  /// Refresh [`ALIVE_FILE`] now.
  ///
  /// Only needed under [`LockOptions::without_refresh`]; the keeper thread
  /// does this otherwise.
  ///
  /// # Errors
  ///
  /// Propagates the write failure.
  pub fn ping(&self) -> io::Result<()> {
    touch_alive(&self.state_dir)
  }

  /// Release explicitly, so the caller sees any failure.
  ///
  /// # Errors
  ///
  /// Propagates failures truncating the record or granting the next waiter.
  /// The kernel lock is dropped either way.
  pub fn release(mut self) -> io::Result<()> {
    self.release_inner()
  }

  fn release_inner(&mut self) -> io::Result<()> {
    // Stop the keeper first: a ping landing after the removal below would
    // resurrect the liveness file with no holder behind it.
    self.keeper = None;

    let result = (|| -> io::Result<()> {
      // Through the still-held fd, for the reason
      // `write_record_in_place` documents. `self.flock` is `Some` here:
      // the only caller that reaches this checks it first.
      if let Some(flock) = self.flock.as_ref() {
        write_record_in_place(flock, "")?;
      }
      match fs::remove_file(self.state_dir.join(ALIVE_FILE)) {
        Ok(()) => {},
        Err(err) if err.kind() == io::ErrorKind::NotFound => {},
        Err(err) => return Err(err),
      }
      // Name the successor *before* dropping the kernel lock, so the grant is
      // already in place when the kernel wakes somebody up.
      grant_next(&self.state_dir.join(WAITERS_DIR), self.record.pid)?;
      Ok(())
    })();

    self.flock = None;
    result
  }
}

impl Drop for SourcesLock {
  fn drop(&mut self) {
    if self.flock.is_some()
      && let Err(err) = self.release_inner()
    {
      warn!("could not cleanly release the sources lock: {err}");
    }
  }
}

/// Read the holder record, or `None` when the lock is free.
fn read_record(lock_path: &Path) -> Result<Option<LockRecord>, LockError> {
  match fs::read_to_string(lock_path) {
    Ok(raw) => LockRecord::parse(lock_path, &raw),
    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(err) => Err(LockError::Io(err)),
  }
}

/// Write the holder record through the fd that holds the kernel lock.
///
/// Deliberately *not* `write_state_file`: that writes atomically by
/// renaming a temp file over the target, and `flock(2)` locks an inode
/// rather than a path. A rename would leave this holder's lock attached
/// to an unlinked inode, so the next acquirer would open the new one and
/// be granted the lock immediately. The spec makes `state_dir/lock`
/// both the lock and the record, so the record has to be written in
/// place.
///
/// An empty `line` is how "free" is spelled.
///
/// The bytes go down before the truncation so a concurrent reader -
/// `status` takes no lock - never observes an empty file and so never
/// reports a held lock as free. It can briefly see the new line
/// followed by a stale tail, which parses as
/// [`LockError::Malformed`]; that is a reporting blip, and exclusion
/// does not depend on it.
fn write_record_in_place(
  flock: &Flock<fs::File>,
  line: &str,
) -> io::Result<()> {
  let mut file: &fs::File = flock;
  file.seek(io::SeekFrom::Start(0))?;
  file.write_all(line.as_bytes())?;
  file.flush()?;
  flock.set_len(u64::try_from(line.len()).unwrap_or(u64::MAX))?;
  Ok(())
}

/// Try the kernel lock without blocking.
///
/// `Ok(None)` means somebody else holds it, which is not an error.
fn try_flock(lock_path: &Path) -> Result<Option<Flock<fs::File>>, LockError> {
  let file = fs::OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .open(lock_path)?;
  super::harden_file(lock_path)?;

  match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
    Ok(flock) => Ok(Some(flock)),
    // `EAGAIN` is deliberately absent: POSIX allows it to be the same value
    // as `EWOULDBLOCK`, and on Linux it is, so naming both is an unreachable
    // pattern rather than extra portability.
    Err((_, Errno::EWOULDBLOCK | Errno::EINTR)) => Ok(None),
    Err((_, errno)) => Err(LockError::Io(io::Error::from(errno))),
  }
}

// ---------------------------------------------------------------------------
// Status surface
// ---------------------------------------------------------------------------

/// A waiter, as `xi <scope> sources status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaiterStatus {
  /// Which actor is waiting.
  pub actor_name: ActorName,
  /// Its pid.
  pub pid:        u32,
  /// When it started waiting.
  pub since:      String,
}

/// The lock half of the sources status envelope.
///
/// Absent from the envelope entirely when the lock is free, so the shape
/// `xi-sources-model` pinned is unchanged on the common path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockStatus {
  /// Which actor holds it.
  pub held_by:             ActorName,
  /// The holder's pid.
  pub pid:                 u32,
  /// When it acquired.
  pub held_since:          String,
  /// What it said it was doing.
  pub intent:              String,
  /// Whether the holder is still credible.
  pub holder:              Liveness,
  /// Seconds left on the default lease, when that can be computed.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub estimated_wait_secs: Option<u64>,
  /// Who is queued behind it, in the order they will be granted.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub waiting:             Vec<WaiterStatus>,
}

/// Read the lock state without taking it.
///
/// This is the reader path: no `flock` call, no blocking, no writes. A free
/// lock reports `Ok(None)`.
///
/// # Errors
///
/// [`LockError::Malformed`] for an unparsable record, [`LockError::Io`] for
/// anything the filesystem refused.
pub fn status(state_dir: &Path) -> Result<Option<LockStatus>, LockError> {
  let Some(holder) = read_record(&state_dir.join(LOCK_FILE))? else {
    return Ok(None);
  };

  let mut queued = read_waiters(&state_dir.join(WAITERS_DIR))?;
  queued.retain(|w| w.pid != holder.pid);
  queued.sort_by(|a, b| {
    a.actor_name
      .priority()
      .cmp(&b.actor_name.priority())
      .then_with(|| a.started_at.cmp(&b.started_at))
      .then_with(|| a.pid.cmp(&b.pid))
  });

  Ok(Some(LockStatus {
    held_by:             holder.actor_name,
    pid:                 holder.pid,
    held_since:          holder.started_at.clone(),
    intent:              holder.intent.clone(),
    holder:              holder_liveness(state_dir, &holder),
    // The holder's own lease is not on disk - only the four spec'd fields
    // are - so the estimate is against the default. A holder that claimed a
    // longer lease keeps refreshing `lock.alive`, which is the signal that
    // actually governs stealing.
    estimated_wait_secs: holder
      .estimated_wait(DEFAULT_LEASE)
      .map(|d| d.as_secs()),
    waiting:             queued
      .into_iter()
      .map(|w| {
        WaiterStatus {
          actor_name: w.actor_name,
          pid:        w.pid,
          since:      w.started_at,
        }
      })
      .collect(),
  }))
}
