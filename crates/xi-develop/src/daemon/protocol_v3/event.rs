//! v3 events — broadcaster payloads streamed to subscribers.
//!
//! Small deltas embed the full payload in-band (state transitions, job
//! lifecycle, keepalive, gap markers, cache invalidations). Large payloads
//! (`FlakeOutputsRefreshed`, `SnapshotRefreshed`) carry only `snapshot_seq`;
//! the consumer fetches the body via a query on demand (notify-and-fetch).
//!
//! Every delivered event carries a monotonic `seq` so consumers can detect
//! and recover from ring-overflow gaps.

use serde::{Deserialize, Serialize};

use super::query::{JobId, SnapshotSeq};

/// A broadcaster event delivered on an open subscription.
///
/// The `seq` field is monotonic per daemon session and lets consumers detect
/// gaps and re-hydrate. See design § "Broadcaster with 100 ms coalescing,
/// keepalive, drop-oldest on overflow".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
  /// Monotonic sequence number for this delivery.
  pub seq: SnapshotSeq,
  /// The typed payload.
  #[serde(flatten)]
  pub payload: EventPayload,
}

/// A gap marker delivered when the per-subscriber ring overflowed.
///
/// Consumers MUST treat events between `from_seq` and `to_seq` as lost and
/// re-hydrate via `Query::Status`. There is no persistent event log in v1;
/// history inside a gap is irrecoverable (design § Risks / Trade-offs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
  /// First sequence number that was dropped (inclusive).
  pub from_seq: SnapshotSeq,
  /// Last sequence number that was dropped (inclusive).
  pub to_seq: SnapshotSeq,
}

/// The typed payload of a broadcaster [`Event`].
///
/// Variant coverage is derived from the design § "notify-and-fetch" and the
/// spec requirements on state, job, cache, and keepalive events. State and
/// snapshot payload types are placeholders (`serde_json::Value`); the real
/// shapes land with `state_machine.rs` / `snapshot.rs` (tasks 2.x).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
  /// A state transition. Small delta; embedded in-band.
  StateChanged {
    /// The previous daemon state (opaque here; concrete shape in task 2.x).
    from: serde_json::Value,
    /// The new daemon state.
    to: serde_json::Value,
    /// The snapshot sequence number this transition produced.
    snapshot_seq: SnapshotSeq,
  },
  /// A job was started (new work; not an attach). Small delta.
  JobStarted {
    /// The new job id.
    job_id: JobId,
    /// The command that spawned it, encoded opaquely.
    command: serde_json::Value,
  },
  /// A job made observable progress. Small delta; coalesced within the 100 ms
  /// window (a burst of ≥ 20 progress events in 200 ms collapses to at most
  /// one per subscriber for that window — SC-013).
  JobProgress {
    /// The job id.
    job_id: JobId,
    /// Progress payload (task 2.x refines the shape).
    progress: serde_json::Value,
  },
  /// A job finished successfully.
  JobCompleted {
    /// The job id.
    job_id: JobId,
    /// Result payload; large results should be fetched via `GetJob(id)`.
    result: serde_json::Value,
  },
  /// A job failed.
  JobFailed {
    /// The job id.
    job_id: JobId,
    /// Failure reason.
    reason: String,
  },
  /// A job was aborted by an explicit `AbortJob` or a `Restart`.
  JobAborted {
    /// The job id.
    job_id: JobId,
  },
  /// A previously-running job was lost (e.g. daemon crash + recovery).
  ///
  /// The client should re-issue with a fresh idempotency key derivation.
  JobLost {
    /// The job id that was lost.
    job_id: JobId,
  },
  /// One of the daemon's caches was invalidated. Small delta.
  Invalidated {
    /// A free-form cache scope selector (task 2.x refines).
    scope: String,
  },
  /// The cached flake outputs were refreshed. Notify-only: consumer that
  /// needs the body fetches it via `Query::FlakeOutputs`.
  FlakeOutputsRefreshed {
    /// Sequence number of the new snapshot; pass to `Query::Snapshot(seq)`.
    snapshot_seq: SnapshotSeq,
  },
  /// The state snapshot was refreshed. Notify-only counterpart of
  /// `FlakeOutputsRefreshed`. Consumers fetch via `Query::Snapshot(seq)`.
  SnapshotRefreshed {
    /// The new snapshot sequence number.
    snapshot_seq: SnapshotSeq,
  },
  /// Liveness ping. Emitted every 30 s (default) on every open subscription
  /// when no other event has been delivered in that interval.
  ///
  /// Consumers time out on 2 missed keepalives (SC-012).
  KeepAlive,
  /// Ring-overflow marker (see [`Gap`]).
  Gap(Gap),
}
