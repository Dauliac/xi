//! v3 subscriptions — first-class transport for long-lived consumers.
//!
//! A client is a registered consumer for the lifetime of an open `Subscribe`
//! socket. There is no separate `Register`/`Deregister` command in v3
//! (that pair lives only as a v2-compat shim).
//!
//! Consumers select one or more [`Topics`] on the [`Subscribe`] request; the
//! daemon streams matching events with 100 ms coalescing, 30 s `KeepAlive`
//! cadence, and `Gap { from_seq, to_seq }` on per-subscriber ring overflow.
//! On `Gap`, the consumer re-hydrates via `Query::Status`
//! (see [`super::query::Query::Status`]).

use serde::{Deserialize, Serialize};

/// Bitset of subscription topics.
///
/// Encoded on the wire as a JSON object of booleans so `jq` inspection stays
/// readable and forward-compat is easy (new topics default to `false`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topics {
  /// State transitions (`StateChanged`, `KeepAlive`, `Gap`).
  #[serde(default)]
  pub state: bool,
  /// Job lifecycle events
  /// (`JobStarted`, `JobProgress`, `JobCompleted`, `JobFailed`, `JobAborted`,
  /// `JobLost`).
  #[serde(default)]
  pub jobs: bool,
  /// Cache invalidation events (`Invalidated`, `FlakeOutputsRefreshed`).
  #[serde(default)]
  pub cache: bool,
}

impl Topics {
  /// A topic set with all topics enabled.
  #[must_use]
  pub const fn all() -> Self {
    Self { state: true, jobs: true, cache: true }
  }

  /// A topic set with only state transitions enabled (typical status-bar consumer).
  #[must_use]
  pub const fn state_only() -> Self {
    Self { state: true, jobs: false, cache: false }
  }
}

/// A subscription request. Answered by the daemon with a stream of events
/// (see [`super::event::Event`]) until the client closes the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscribe {
  /// Which topics to receive. A subscription with zero topics is
  /// well-formed — it still receives `KeepAlive` for liveness detection.
  pub topics: Topics,
  /// If set, resume from this snapshot sequence number.
  ///
  /// The daemon MAY refuse the resume (returning a `Gap` marker) if it no
  /// longer has state for that sequence; consumers re-hydrate via
  /// `Query::Status` in that case. `None` means "start from the current
  /// snapshot".
  #[serde(default)]
  pub resume_from: Option<super::query::SnapshotSeq>,
}
