//! Authoritative in-memory job registry with idempotency-based dedup and
//! attach semantics for concurrent identical commands.
//!
//! # Purpose
//!
//! When two panes fire the same `EvalDevshell` at the same time, the daemon
//! should evaluate once and let both panes observe the same [`JobId`] and
//! the same stream of progress events. That guarantee is SC-004 of the
//! `async-daemon-cqrs` change: "Opening a second and third pane while
//! evaluation is in flight triggers zero additional eval jobs; all panes
//! observe the same job id and terminal state."
//!
//! The [`JobRegistry`] is the single point where that dedup happens. Callers
//! (the command handler in `daemon/server.rs`, task 2.4) route every
//! job-spawning [`Command`][crate::daemon::job_registry::JobKind] variant
//! through [`JobRegistry::attach_or_start`]; the returned [`Attached`] flag
//! tells the caller whether a fresh execution should be spawned or whether
//! the request should simply subscribe to the existing job's event stream.
//!
//! # Persistence
//!
//! The registry is intentionally in-memory. Design § "Risks / Trade-offs":
//!
//! > In-memory job registry: a daemon crash loses the job list; clients
//! > recover via `JobLost` and re-issue. Accepted trade-off — hot-path I/O
//! > for a rare failure mode is not worth it in v1. Persistent job log is a
//! > documented future extension.
//!
//! # Concurrency
//!
//! State lives inside a `Mutex<Inner>` because the registry sits on the
//! command hot path (bursty, not sustained) rather than the query hot path
//! (that goes through `state_responder`'s `arc_swap`, task 2.1). A simple
//! mutex is fine and avoids a `dashmap` dependency.
//!
//! # Idempotency key
//!
//! The `IdempotencyKey` type in this module is a self-contained 128-bit key
//! matching the shape used on the wire by `protocol_v3::command`. Task 1.3
//! fills in the blake3 derivation that produces the key value; task 2.2
//! (this file) only needs the key to be `Copy + Eq + Hash` and to be able to
//! stand in for the wire type once protocol_v3 lands in this crate. When the
//! wire type appears, this module's `IdempotencyKey` should be replaced with
//! a `pub use protocol_v3::command::IdempotencyKey` re-export — the byte
//! layout is identical.

use std::{
  collections::HashMap,
  sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
  },
  time::{Instant, SystemTime, UNIX_EPOCH},
};

/// A blake3-derived idempotency key.
///
/// This module keeps a local copy of the type so `job_registry` compiles as
/// a standalone unit before `protocol_v3` merges into this crate. Task 1.3
/// owns the derivation; this type only cares that the key is a fixed-size
/// opaque byte array with `Copy + Eq + Hash`.
///
/// The 32-byte surface mirrors the on-wire shape verbatim so replacing this
/// type with `pub use protocol_v3::command::IdempotencyKey` is a
/// zero-behaviour-change edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(pub [u8; 32]);

impl IdempotencyKey {
  /// Construct a key from an already-derived 32-byte digest.
  ///
  /// Callers should route through task 1.3's blake3 derivation rather than
  /// hand-crafting keys — the daemon's dedup guarantee is only sound when
  /// every call site produces the key the same way.
  #[must_use]
  pub const fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }
}

/// An opaque per-daemon job identifier.
///
/// # Format
///
/// The id is a 32-character lowercase hex string encoding a 128-bit value:
///
/// - upper 64 bits: process-start nanoseconds since the Unix epoch, so ids
///   from two consecutive daemon processes never collide even if the
///   counter resets;
/// - lower 64 bits: a monotonic per-process `AtomicU64` counter.
///
/// # Why not ULID or UUID?
///
/// The registry is per-daemon-process and in-memory (design § "Risks /
/// Trade-offs"), so cross-process uniqueness is not required — a daemon
/// crash discards every id. ULID/UUID would add a workspace dependency for
/// a property we do not need. The counter+timestamp encoding gives us:
///
/// - monotonic ordering within a process (nice for debugging log tails);
/// - trivial uniqueness across restarts because the timestamp differs;
/// - zero external dependencies;
/// - a stable hex-string wire shape compatible with
///   `protocol_v3::query::JobId` (`pub type JobId = String`).
///
/// When the wire type ever migrates to a stronger id (e.g. once the daemon
/// gains persistence in v2 of the change), the switch is local to
/// [`JobRegistry::new_job_id`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(pub String);

impl JobId {
  /// Borrow the id as a `&str` for use with wire types that expect
  /// `JobId = String` in `protocol_v3::query`.
  #[must_use]
  pub fn as_str(&self) -> &str {
    self.0.as_str()
  }
}

impl std::fmt::Display for JobId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.0)
  }
}

/// The class of work a job represents.
///
/// Mirrors the `Command` variants in `protocol_v3::command` that spawn work.
/// The registry does not itself execute the work — that is the work
/// executor's job (task 2.4). It just records what kind of work a running
/// [`JobId`] corresponds to so state queries can render "evaluating" vs
/// "restarting" without a separate lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
  /// Evaluate the devshell for a target attribute.
  EvalDevshell {
    /// The devshell attribute path (typically `"default"`).
    target: String,
  },
  /// Restart the daemon.
  Restart,
  /// Invalidate one or more daemon caches.
  InvalidateCache {
    /// A free-form cache scope selector — task 2.x refines this.
    scope: String,
  },
  /// Abort a previously-registered job.
  AbortJob {
    /// The id of the job to abort.
    target: JobId,
  },
}

/// The current status of a job in the registry.
///
/// # Terminal statuses
///
/// `Succeeded`, `Failed`, and `Aborted` are terminal: no further transitions
/// are permitted, and a fresh `attach_or_start` call with the same
/// idempotency key allocates a new job rather than attaching to the
/// terminal one. `Stuck` is *not* terminal — it is a hint the daemon uses
/// to surface warnings; the underlying job is still running and further
/// callers may still attach.
///
/// See [`JobRegistry::attach_or_start`] for the full attach-vs-fresh table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
  /// The job has been registered and is running.
  Running,
  /// The job finished successfully.
  Succeeded,
  /// The job finished with an error.
  Failed {
    /// A machine-parseable error string. Taxonomy is task 2.4's problem.
    error: String,
  },
  /// The job was aborted before completion.
  Aborted,
  /// The job has not published a progress event for `stuck_ms` milliseconds.
  ///
  /// Non-terminal: the job is still running from the registry's point of
  /// view. `state_responder` (task 2.1) uses this to render a `Stuck` badge
  /// without blocking on the work path.
  Stuck {
    /// How long the job has been silent, in milliseconds.
    stuck_ms: u64,
  },
}

impl JobStatus {
  /// Whether the status is terminal.
  ///
  /// A terminal status means the job has finished for good and further
  /// commands with the same idempotency key must start a fresh job.
  #[must_use]
  pub const fn is_terminal(&self) -> bool {
    matches!(
      self,
      Self::Succeeded | Self::Failed { .. } | Self::Aborted
    )
  }
}

/// A single entry in the [`JobRegistry`].
///
/// Cloned out on read via [`JobRegistry::get`] so callers never hold the
/// registry lock past the call site.
#[derive(Debug, Clone)]
pub struct JobEntry {
  /// The registry-allocated id.
  pub id: JobId,
  /// What kind of work this job represents.
  pub kind: JobKind,
  /// When the job was first registered (used for logging + stale-scan).
  pub started_at: SystemTime,
  /// When the job last emitted a progress heartbeat.
  ///
  /// `None` on freshly-registered jobs — the work executor bumps this on
  /// every `JobProgress` event. Used by [`JobRegistry::stuck_scan`] to
  /// surface stuck jobs.
  pub last_progress: Option<Instant>,
  /// Current status.
  pub status: JobStatus,
}

/// Whether an `attach_or_start` call attached to a running job or allocated
/// a fresh one.
///
/// The caller (command handler, task 2.4) uses this to decide whether to
/// spawn work on the work pool or simply subscribe the client to the
/// existing job's event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attached {
  /// The request matched an in-flight job by idempotency key. Do not spawn.
  Yes,
  /// A fresh job was registered. The caller must spawn the underlying work.
  No,
  }

/// Authoritative in-memory table of running jobs.
///
/// See the module-level docs for the design contract this implements.
#[derive(Debug)]
pub struct JobRegistry {
  inner: Mutex<Inner>,
  id_counter: AtomicU64,
  /// Process-start nanoseconds since the Unix epoch, folded into every new
  /// job id so ids from consecutive daemon processes never collide.
  epoch_ns: u64,
}

#[derive(Debug, Default)]
struct Inner {
  /// Live index: idempotency key → job id, only populated while the job is
  /// non-terminal and therefore eligible for attach.
  attach_index: HashMap<IdempotencyKey, JobId>,
  /// Full job table keyed by id. Terminal entries remain queryable via
  /// [`JobRegistry::get`] but are absent from `attach_index`.
  jobs: HashMap<JobId, JobEntry>,
}

impl Default for JobRegistry {
  fn default() -> Self {
    Self::new()
  }
}

impl JobRegistry {
  /// Construct an empty registry.
  #[must_use]
  pub fn new() -> Self {
    let epoch_ns = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map(|d| {
        // Truncate to u64. Overflow would require the clock to be set > 500
        // years past the Unix epoch, which we treat as "call it zero".
        u64::try_from(d.as_nanos()).unwrap_or(0)
      })
      .unwrap_or(0);
    Self {
      inner:      Mutex::new(Inner::default()),
      id_counter: AtomicU64::new(0),
      epoch_ns,
    }
  }

  /// Attach to an existing job or register a fresh one.
  ///
  /// # Semantics
  ///
  /// - If a non-terminal job already exists for `key`, returns that job's
  ///   id with [`Attached::Yes`].
  /// - Otherwise allocates a new [`JobId`], stores a fresh [`JobEntry`]
  ///   with [`JobStatus::Running`], and returns [`Attached::No`].
  ///
  /// # Status-vs-attach table
  ///
  /// For a call to `attach_or_start(k, _)` where a prior job existed with
  /// key `k`:
  ///
  /// | prior status              | attach? | notes                                                                         |
  /// |---------------------------|---------|-------------------------------------------------------------------------------|
  /// | `Running`                 | Yes     | canonical multi-pane dedup — SC-004                                           |
  /// | `Stuck { .. }`            | Yes     | non-terminal; the job is still running from the registry's point of view     |
  /// | `Succeeded`               | No      | terminal — the prior entry stays queryable via `get`, not `attach_or_start` |
  /// | `Failed { .. }`           | No      | terminal — re-issue starts a fresh job                                        |
  /// | `Aborted`                 | No      | terminal — re-issue starts a fresh job                                        |
  /// | no prior entry            | No      | fresh registration                                                            |
  ///
  /// The design does not require reaping terminal entries here; they are
  /// eligible for later cleanup once client subscriptions confirm delivery.
  pub fn attach_or_start(
    &self,
    key: IdempotencyKey,
    kind: JobKind,
  ) -> (JobId, Attached) {
    let mut inner = self.lock();
    if let Some(existing) = inner.attach_index.get(&key).cloned() {
      // `attach_index` only contains non-terminal entries by construction.
      return (existing, Attached::Yes);
    }
    let id = self.mint_job_id();
    let entry = JobEntry {
      id:            id.clone(),
      kind,
      started_at:    SystemTime::now(),
      last_progress: None,
      status:        JobStatus::Running,
    };
    inner.attach_index.insert(key, id.clone());
    inner.jobs.insert(id.clone(), entry);
    (id, Attached::No)
  }

  /// Update the status of a job.
  ///
  /// Terminal statuses (`Succeeded`, `Failed`, `Aborted`) remove the entry
  /// from the attach index but keep it in the job table so subscribers can
  /// still query the outcome via [`JobRegistry::get`].
  ///
  /// A no-op if `id` is unknown, so late-arriving callbacks after registry
  /// churn cannot panic the daemon.
  pub fn update_status(&self, id: &JobId, status: JobStatus) {
    let mut inner = self.lock();
    let Some(entry) = inner.jobs.get_mut(id) else {
      return;
    };
    let now_terminal = status.is_terminal();
    entry.status = status;
    if now_terminal {
      // Drop every attach-index entry pointing at this id. We do not track
      // the reverse map because entries are cheap and the O(N) sweep runs
      // once per terminal transition, not once per progress event.
      inner.attach_index.retain(|_, v| v != id);
    }
  }

  /// Record a progress heartbeat for `id`, bumping `last_progress` to now.
  ///
  /// Used by the work executor (task 2.4) so [`JobRegistry::stuck_scan`]
  /// can surface silent jobs. A no-op if `id` is unknown.
  pub fn record_progress(&self, id: &JobId) {
    let mut inner = self.lock();
    if let Some(entry) = inner.jobs.get_mut(id) {
      entry.last_progress = Some(Instant::now());
    }
  }

  /// Snapshot a job's current entry, or `None` if the id is unknown.
  ///
  /// Cheap: clones the entry out and releases the lock immediately.
  #[must_use]
  pub fn get(&self, id: &JobId) -> Option<JobEntry> {
    self.lock().jobs.get(id).cloned()
  }

  /// Return the ids of every non-terminal job whose last progress
  /// heartbeat is older than `threshold_ms` milliseconds, or which has
  /// never emitted a progress event and was started more than
  /// `threshold_ms` ago.
  ///
  /// Used by the stuck-detector helper (task 2.x) — this function itself
  /// does not mutate the registry; the caller decides whether to promote
  /// the job's status to [`JobStatus::Stuck`].
  #[must_use]
  pub fn stuck_scan(&self, threshold_ms: u64) -> Vec<JobId> {
    let inner = self.lock();
    let now = Instant::now();
    let threshold = std::time::Duration::from_millis(threshold_ms);
    let mut stuck: Vec<JobId> = Vec::new();
    for entry in inner.jobs.values() {
      if entry.status.is_terminal() {
        continue;
      }
      let silent_for = match entry.last_progress {
        Some(last) => now.saturating_duration_since(last),
        // No heartbeat yet — fall back to wall-clock age. `elapsed` returns
        // an error only if the clock jumped backwards; treat that as "not
        // stuck yet" rather than a panic.
        None => entry
          .started_at
          .elapsed()
          .unwrap_or(std::time::Duration::ZERO),
      };
      if silent_for >= threshold {
        stuck.push(entry.id.clone());
      }
    }
    stuck
  }

  fn mint_job_id(&self) -> JobId {
    let seq = self.id_counter.fetch_add(1, Ordering::Relaxed);
    // Upper 64 bits: epoch ns; lower 64 bits: monotonic seq. 32 hex chars.
    JobId(format!("{:016x}{:016x}", self.epoch_ns, seq))
  }

  /// Acquire the inner mutex, treating a poisoned lock as if the previous
  /// panic had left the data consistent.
  ///
  /// The registry only holds owned `String`/`HashMap` state and every mutation
  /// is a single-shot update, so a poisoned guard is safe to keep using.
  /// This lets us honour the workspace lint `clippy::unwrap_used = deny`
  /// without threading an error type through the entire public surface.
  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    match self.inner.lock() {
      Ok(guard) => guard,
      Err(poisoned) => poisoned.into_inner(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn key(byte: u8) -> IdempotencyKey {
    IdempotencyKey::from_bytes([byte; 32])
  }

  fn eval_kind(target: &str) -> JobKind {
    JobKind::EvalDevshell { target: target.to_owned() }
  }

  #[test]
  fn fresh_key_returns_new_id_and_attached_no() {
    let reg = JobRegistry::new();
    let (id, attached) = reg.attach_or_start(key(1), eval_kind("default"));
    assert_eq!(attached, Attached::No);
    assert!(!id.as_str().is_empty());
    let entry = reg.get(&id).expect("registered job is queryable");
    assert_eq!(entry.status, JobStatus::Running);
    assert!(matches!(entry.kind, JobKind::EvalDevshell { .. }));
  }

  #[test]
  fn same_key_while_running_attaches_to_same_id() {
    let reg = JobRegistry::new();
    let (first, first_attached) =
      reg.attach_or_start(key(7), eval_kind("default"));
    assert_eq!(first_attached, Attached::No);
    let (second, second_attached) =
      reg.attach_or_start(key(7), eval_kind("default"));
    assert_eq!(second_attached, Attached::Yes);
    assert_eq!(first, second, "SC-004: identical concurrent commands share id");
  }

  #[test]
  fn terminal_status_frees_key_for_fresh_job() {
    let reg = JobRegistry::new();
    let (first, _) = reg.attach_or_start(key(2), eval_kind("default"));
    reg.update_status(&first, JobStatus::Succeeded);

    // The completed entry stays queryable.
    let completed = reg.get(&first).expect("completed job still queryable");
    assert_eq!(completed.status, JobStatus::Succeeded);

    // A new call with the same key allocates a fresh job.
    let (second, attached) =
      reg.attach_or_start(key(2), eval_kind("default"));
    assert_eq!(attached, Attached::No);
    assert_ne!(first, second, "terminal jobs are not attachable");
  }

  #[test]
  fn failed_and_aborted_are_also_non_attachable() {
    for status in [
      JobStatus::Failed { error: "boom".to_owned() },
      JobStatus::Aborted,
    ] {
      let reg = JobRegistry::new();
      let (first, _) = reg.attach_or_start(key(9), eval_kind("default"));
      reg.update_status(&first, status);
      let (second, attached) =
        reg.attach_or_start(key(9), eval_kind("default"));
      assert_eq!(attached, Attached::No);
      assert_ne!(first, second);
    }
  }

  #[test]
  fn stuck_status_still_attaches() {
    let reg = JobRegistry::new();
    let (first, _) = reg.attach_or_start(key(3), eval_kind("default"));
    reg.update_status(&first, JobStatus::Stuck { stuck_ms: 45_000 });
    let (second, attached) =
      reg.attach_or_start(key(3), eval_kind("default"));
    assert_eq!(attached, Attached::Yes);
    assert_eq!(first, second, "Stuck is non-terminal");
  }

  #[test]
  fn stuck_scan_returns_jobs_older_than_threshold() {
    let reg = JobRegistry::new();
    let (id, _) = reg.attach_or_start(key(4), eval_kind("default"));

    // With a zero threshold, any non-terminal job is "stuck". This is the
    // cheapest way to test the branch without sleeping.
    let stuck = reg.stuck_scan(0);
    assert!(stuck.contains(&id));

    // A very large threshold reports no stuck jobs.
    let stuck = reg.stuck_scan(u64::MAX);
    assert!(stuck.is_empty());
  }

  #[test]
  fn stuck_scan_ignores_terminal_jobs() {
    let reg = JobRegistry::new();
    let (id, _) = reg.attach_or_start(key(5), eval_kind("default"));
    reg.update_status(&id, JobStatus::Succeeded);
    let stuck = reg.stuck_scan(0);
    assert!(!stuck.contains(&id), "terminal jobs are not stuck candidates");
  }

  #[test]
  fn record_progress_updates_last_progress() {
    let reg = JobRegistry::new();
    let (id, _) = reg.attach_or_start(key(6), eval_kind("default"));
    assert!(reg.get(&id).and_then(|e| e.last_progress).is_none());
    reg.record_progress(&id);
    assert!(reg.get(&id).and_then(|e| e.last_progress).is_some());
  }

  #[test]
  fn update_status_on_unknown_id_is_noop() {
    let reg = JobRegistry::new();
    let phantom = JobId("deadbeef".to_owned());
    reg.update_status(&phantom, JobStatus::Succeeded);
    assert!(reg.get(&phantom).is_none());
  }

  #[test]
  fn different_keys_do_not_collide() {
    let reg = JobRegistry::new();
    let (a, _) = reg.attach_or_start(key(10), eval_kind("default"));
    let (b, _) = reg.attach_or_start(key(11), eval_kind("default"));
    assert_ne!(a, b);
  }

  #[test]
  fn job_ids_are_unique_across_bursts() {
    let reg = JobRegistry::new();
    let mut seen = std::collections::HashSet::new();
    for i in 0u8..64 {
      let mut bytes = [0u8; 32];
      bytes[0] = i;
      let (id, attached) = reg.attach_or_start(
        IdempotencyKey::from_bytes(bytes),
        eval_kind("default"),
      );
      assert_eq!(attached, Attached::No);
      assert!(seen.insert(id), "job ids must be unique");
    }
  }
}
