//! v3 queries — read-only requests.
//!
//! Queries MUST return fast without ever blocking on work. Every value-bearing
//! query returns an [`Availability`] shape: `Ready(value)`, `Pending(job_id)`,
//! `Missing`, or `Degraded(reason)`. This is the type-level enforcement of the
//! xi-devshell-daemon spec requirement "Protocol — commands vs queries
//! distinguished at the type level".
//!
//! Small deltas embed the full payload; large payloads
//! (e.g. `FlakeOutputs`, `Snapshot`) are notify-and-fetch: an event carries
//! only `snapshot_seq` and the consumer fetches the body via the matching
//! query when it needs it.

use serde::{Deserialize, Serialize};

/// Monotonic snapshot sequence number.
///
/// Emitted in events for notify-and-fetch large payloads and carried by
/// `Query::Snapshot` when a consumer re-hydrates after a `Gap`.
pub type SnapshotSeq = u64;

/// Job id assigned by the daemon's job registry.
///
/// Opaque to the client. Returned by commands and by queries whose value is
/// not yet computed (see [`Availability::Pending`]).
pub type JobId = String;

/// The generic availability wrapper for query replies whose value may not be
/// computed yet.
///
/// Encodes the four possible read outcomes documented in the design:
/// - `Ready(value)` — the value is available now (fast path).
/// - `Pending(job_id)` — a job is producing this value; observe it via
///   `GetJob(id)` or subscribe.
/// - `Missing` — the value does not exist in the current state (e.g. no
///   devshell configured).
/// - `Degraded { reason }` — the daemon cannot produce this value in the
///   current state (e.g. watcher degraded, config error).
///
/// A query MUST NOT wait on work: the responder always answers within the
/// state-query budget with one of these four shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum Availability<T> {
  /// The value is available now.
  Ready { value: T },
  /// A job is producing this value; caller can attach via `GetJob(job_id)`.
  Pending { job_id: JobId },
  /// The value does not exist in the current state.
  Missing,
  /// The daemon cannot answer this query in the current state.
  Degraded { reason: String },
}

/// A read-only query targeted at the daemon.
///
/// All variants MUST return within the state-query budget (default 20 ms)
/// regardless of ongoing work. See design.md § "Threading model" and
/// § "Single-writer state machine": the state responder reads a lock-free
/// `arc_swap<StateSnapshot>` and never coordinates with the work executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "query", content = "args", rename_all = "snake_case")]
pub enum Query {
  /// Fetch the current daemon status (state, uptime, consumer count, etc).
  Status,
  /// Look up a specific job in the registry.
  GetJob {
    /// The job id previously returned by a command or in `Availability::Pending`.
    job_id: JobId,
  },
  /// Fetch the current devshell env pointer.
  ///
  /// Answered from the `current_env` pointer (last-good) even when the
  /// daemon is in `BuildFailed`; the pointer only moves on successful re-eval.
  Devshell,
  /// Fetch cached `nix flake show` output. Large payload; consumers should
  /// prefer `snapshot_seq`-driven fetch (see the notify-and-fetch pattern).
  FlakeOutputs,
  /// Lightweight liveness probe. Used as the first message a client sends
  /// during version negotiation (a v2 daemon has no `"v"` field to route on).
  HeartBeat,
  /// The compile-time state metadata catalog.
  ///
  /// Returned as `{ state_name → { display, color, category, phase, terminal } }`.
  /// A parity test in the daemon build fails if any `DaemonState` variant is
  /// missing from the catalog.
  StateCatalog,
  /// Fetch a specific snapshot by sequence number.
  ///
  /// Used by subscribers to re-hydrate after a `Gap { from_seq, to_seq }`
  /// marker; passing `seq` lets the daemon return a consistent snapshot even
  /// if newer snapshots have already been published.
  Snapshot {
    /// The snapshot sequence number to fetch.
    seq: SnapshotSeq,
  },
}

/// The reply body returned by the daemon for a preceding [`Query`].
///
/// Variants correspond one-to-one to [`Query`] variants. Every value-bearing
/// reply wraps its value in an [`Availability`] so callers see a uniform
/// `Ready|Pending|Missing|Degraded` shape at the type level.
///
/// Concrete state / snapshot payload types are defined in later tasks
/// (`state_machine.rs`, `snapshot.rs`); this module uses `serde_json::Value`
/// as a forward-compatible placeholder so the wire shape can be exercised in
/// tests before those types land.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "query_reply", content = "value", rename_all = "snake_case")]
pub enum QueryReply {
  /// Reply to [`Query::Status`].
  Status(Availability<serde_json::Value>),
  /// Reply to [`Query::GetJob`].
  GetJob(Availability<serde_json::Value>),
  /// Reply to [`Query::Devshell`].
  Devshell(Availability<serde_json::Value>),
  /// Reply to [`Query::FlakeOutputs`]. Prefer notify-and-fetch for large payloads.
  FlakeOutputs(Availability<serde_json::Value>),
  /// Reply to [`Query::HeartBeat`]. Presence of this variant confirms the
  /// daemon speaks v3; a v2 daemon replies via the legacy `DaemonResponse`.
  HeartBeat,
  /// Reply to [`Query::StateCatalog`].
  StateCatalog(Availability<serde_json::Value>),
  /// Reply to [`Query::Snapshot`].
  Snapshot(Availability<serde_json::Value>),
}
