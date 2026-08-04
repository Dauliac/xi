//! Fast-path query responder.
//!
//! The `StateResponder` is a lightweight thread whose sole job is to answer
//! **query** requests (never commands, never subscriptions) by reading the
//! atomic [`StateSnapshot`] published by the state machine (task 1.4).
//!
//! Design targets (see `openspec/changes/async-daemon-cqrs/design.md`):
//!
//! - **No locks on the fast path.** Every access to daemon state MUST be a
//!   single [`arc_swap::ArcSwap::load`], never a mutex or rwlock.
//! - **Under 20 ms** round-trip for prompt-hook query dispatch.
//! - **Isolated from work threads.** Nothing the work threads do can block a
//!   state read.
//!
//! ## Sibling-worker coordination
//!
//! Several types referenced by the design live in modules that are being
//! written in parallel by other workers. Until the integrator merges those in,
//! this module carries **placeholder shapes** with the *identical* type
//! signatures:
//!
//! | Placeholder here              | Owner task | Consolidation target                        |
//! | ----------------------------- | ---------- | ------------------------------------------- |
//! | [`StateSnapshot`]             | 1.4        | `state_machine::StateSnapshot`              |
//! | [`StateHandle`]               | 1.4        | `state_machine::StateHandle`                |
//! | [`Query`], [`QueryReply`]     | protocol_v3| `daemon::protocol_v3::{query,reply}`        |
//! | [`Availability`]              | protocol_v3| `daemon::protocol_v3::reply::Availability`  |
//! | [`state_meta::CATALOG`]       | 1.4        | `state_machine::state_meta::CATALOG`        |
//!
//! When the integrator lands the real types, the shape (public field names,
//! variants) is intended to stay byte-identical so this file's only churn
//! should be swapping `use` paths.

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, Sender};

use super::protocol::DaemonState;

// =============================================================================
// Placeholder types — see the "Sibling-worker coordination" table above.
// TODO(task 1.4): delete this block, import `state_machine::{StateSnapshot,
// StateHandle}` and `state_machine::state_meta::CATALOG` instead.
// TODO(protocol_v3): delete `Query`, `QueryReply`, `Availability` here and
// import from `daemon::protocol_v3` once that module lands.
// =============================================================================

/// Immutable snapshot of daemon state, published atomically by the state
/// machine.
///
/// Placeholder — task 1.4 owns the real type. Field names and layout must stay
/// stable so the integrator swap is mechanical.
#[derive(Debug, Clone)]
pub struct StateSnapshot {
  /// Coarse-grained lifecycle state.
  pub state: DaemonState,
  /// Monotonic sequence number, incremented each time the state machine
  /// publishes a new snapshot.
  pub seq: u64,
  /// Wall-clock uptime seconds at snapshot publish time.
  pub uptime_secs: u64,
  /// Number of consumers currently attached to the daemon.
  pub consumer_count: u32,
  /// Package count exposed by the current devshell (0 while no env is
  /// resolved).
  pub package_count: u32,
  /// Target of the currently active devshell, `""` if none.
  pub current_target: String,
  /// Daemon version, mirrored from `env!("CARGO_PKG_VERSION")` at publish
  /// time.
  pub version: String,
}

impl StateSnapshot {
  /// Convenience constructor for tests and bring-up.
  #[must_use]
  pub fn initial() -> Self {
    Self {
      state: DaemonState::Starting,
      seq: 0,
      uptime_secs: 0,
      consumer_count: 0,
      package_count: 0,
      current_target: String::new(),
      version: env!("CARGO_PKG_VERSION").to_string(),
    }
  }
}

/// Handle to the atomically published [`StateSnapshot`].
///
/// Placeholder alias for what will become `state_machine::StateHandle` under
/// task 1.4. The shape (`Arc<ArcSwap<StateSnapshot>>`) is fixed by the design.
pub type StateHandle = Arc<ArcSwap<StateSnapshot>>;

/// Metadata describing a single [`DaemonState`] variant, exposed via
/// [`Query::StateCatalog`].
///
/// Placeholder — task 1.4 owns the real `state_meta` module.
pub mod state_meta {
  /// A single entry in the state catalog.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct StateEntry {
    /// Machine-readable name, matches the [`DaemonState`] variant name.
    pub name: &'static str,
    /// Human-readable one-line description.
    pub description: &'static str,
  }

  /// All well-known daemon states, in lifecycle order.
  ///
  /// Kept in sync with [`super::DaemonState`]. Task 1.4 will consolidate this
  /// with the state machine's canonical catalog.
  pub const CATALOG: &[StateEntry] = &[
    StateEntry {
      name: "Starting",
      description: "Daemon is initializing, no work accepted yet.",
    },
    StateEntry {
      name: "Evaluating",
      description: "A devshell evaluation is in progress.",
    },
    StateEntry {
      name: "Ready",
      description: "Daemon is idle with a resolved devshell environment.",
    },
    StateEntry {
      name: "BuildFailed",
      description: "The last build failed; retries may be scheduled.",
    },
    StateEntry {
      name: "WatcherDegraded",
      description:
        "The filesystem watcher lost the tree; falling back to poll.",
    },
    StateEntry {
      name: "ConfigError",
      description: "Config load failed; daemon is running in degraded mode.",
    },
    StateEntry {
      name: "ShuttingDown",
      description: "Daemon is draining and will exit shortly.",
    },
  ];
}

/// Availability envelope shared by every query reply that can be pending or
/// missing.
///
/// Placeholder for `daemon::protocol_v3::reply::Availability`.
#[derive(Debug, Clone)]
pub enum Availability<T> {
  /// The resource is present and up to date.
  Ready(T),
  /// The resource is being computed; caller may retry.
  Pending {
    /// Human-readable one-line hint (e.g. `"evaluating flake"`).
    hint: String,
  },
  /// The resource is not available in this build/config.
  Missing {
    /// One-line reason for the missing value.
    reason: String,
  },
}

/// Query variants — the read-only side of the CQRS split.
///
/// Placeholder for `daemon::protocol_v3::query::Query`.
#[derive(Debug, Clone)]
pub enum Query {
  /// Coarse status — same information as legacy [`super::protocol::Status`].
  Status,
  /// Resolved devshell env, if any (task 2.3 wires the real pointer).
  Devshell,
  /// Flake outputs listing (task 2.5).
  FlakeOutputs,
  /// Round-trip heartbeat used by health probes.
  HeartBeat,
  /// Static state catalog — one entry per [`DaemonState`] variant.
  StateCatalog,
  /// Fetch snapshot at a given sequence number, or the current one.
  Snapshot(u64),
  /// Fetch a specific job (task 2.2 owns job_registry).
  GetJob(String),
}

/// Reply payload matching each [`Query`] variant.
///
/// Placeholder for `daemon::protocol_v3::reply::QueryReply`.
#[derive(Debug, Clone)]
pub enum QueryReply {
  /// Reply to [`Query::Status`].
  Status(Availability<StatusPayload>),
  /// Reply to [`Query::Devshell`].
  Devshell(Availability<DevshellPayload>),
  /// Reply to [`Query::FlakeOutputs`].
  FlakeOutputs(Availability<FlakeOutputsPayload>),
  /// Reply to [`Query::HeartBeat`].
  HeartBeat,
  /// Reply to [`Query::StateCatalog`].
  StateCatalog(Vec<state_meta::StateEntry>),
  /// Reply to [`Query::Snapshot`] when the requested sequence matches.
  Snapshot(StateSnapshot),
  /// Reply to [`Query::Snapshot`] when the requested sequence is older than
  /// what we hold — caller should re-fetch.
  Gap {
    /// Sequence the client asked for.
    requested_seq: u64,
    /// Sequence currently held by the responder.
    current_seq: u64,
  },
  /// Reply to [`Query::GetJob`].
  Job(Availability<JobPayload>),
}

/// Payload for a successful [`Query::Status`] reply.
#[derive(Debug, Clone)]
pub struct StatusPayload {
  /// Coarse daemon state at snapshot time.
  pub state: DaemonState,
  /// Uptime seconds at snapshot time.
  pub uptime_secs: u64,
  /// Number of attached consumers.
  pub consumer_count: u32,
  /// Package count of the resolved devshell (0 if none).
  pub package_count: u32,
  /// Current devshell target (`""` if none).
  pub current_target: String,
  /// Daemon version string.
  pub version: String,
  /// Sequence number of the snapshot this reply was built from.
  pub seq: u64,
}

/// Payload for [`Query::Devshell`] once task 2.3 wires the env pointer.
#[derive(Debug, Clone)]
pub struct DevshellPayload {
  /// Absolute path to the env file (`envrc` or equivalent).
  pub env_file_path: String,
  /// Store path of the resolved devshell.
  pub store_path: String,
}

/// Payload for [`Query::FlakeOutputs`] once task 2.5 wires it.
#[derive(Debug, Clone)]
pub struct FlakeOutputsPayload {
  /// JSON blob of the flake `outputs` attribute (opaque here).
  pub outputs_json: String,
}

/// Payload for [`Query::GetJob`] once task 2.2 (job_registry) lands.
#[derive(Debug, Clone)]
pub struct JobPayload {
  /// Job identifier.
  pub id: String,
  /// Human-readable status line.
  pub status: String,
}

// =============================================================================
// StateResponder — the actual fast-path worker.
// =============================================================================

/// Fast-path query responder.
///
/// Owns nothing but the state handle and its input channel. Every reply is
/// built from a single atomic [`ArcSwap::load`], so the responder can never be
/// blocked by any command-side work.
pub struct StateResponder {
  handle: StateHandle,
  rx: Receiver<QueryEnvelope>,
}

/// A query paired with a one-shot reply channel.
///
/// Task 2.4 (`server.rs` wire-up) is responsible for constructing these from
/// on-wire messages and forwarding the reply back to the client socket.
#[derive(Debug)]
pub struct QueryEnvelope {
  /// The query to answer.
  pub query: Query,
  /// One-shot reply channel. Sending is best-effort; if the peer dropped the
  /// receiver we silently discard the reply.
  pub reply_to: Sender<QueryReply>,
}

impl StateResponder {
  /// Construct a responder without spawning a thread. Useful for unit tests
  /// that want to drive [`Self::answer`] directly.
  #[must_use]
  pub fn new(handle: StateHandle, rx: Receiver<QueryEnvelope>) -> Self {
    Self { handle, rx }
  }

  /// Spawn the responder on its own OS thread.
  ///
  /// The thread exits cleanly when every [`Sender`] paired with `rx` is
  /// dropped.
  pub fn spawn(
    handle: StateHandle,
    rx: Receiver<QueryEnvelope>,
  ) -> JoinHandle<()> {
    thread::Builder::new()
      .name("xi-state-responder".into())
      .spawn(move || {
        let responder = Self::new(handle, rx);
        responder.run();
      })
      .expect("spawn xi-state-responder thread")
  }

  /// Run the responder loop until the input channel closes.
  pub fn run(self) {
    let Self { handle, rx } = self;
    while let Ok(env) = rx.recv() {
      let reply = Self::answer(&handle, env.query);
      // Best-effort delivery: if the client hung up, drop the reply.
      let _ = env.reply_to.send(reply);
    }
  }

  /// Produce a reply for a single [`Query`], performing exactly one
  /// [`ArcSwap::load`] and no other synchronization.
  ///
  /// This is the fast path — keep it lock-free and allocation-light.
  #[must_use]
  pub fn answer(handle: &StateHandle, query: Query) -> QueryReply {
    match query {
      Query::Status => {
        let snap = handle.load();
        QueryReply::Status(Availability::Ready(StatusPayload {
          state:          snap.state.clone(),
          uptime_secs:    snap.uptime_secs,
          consumer_count: snap.consumer_count,
          package_count:  snap.package_count,
          current_target: snap.current_target.clone(),
          version:        snap.version.clone(),
          seq:            snap.seq,
        }))
      }
      Query::Devshell => {
        // TODO(task 2.3 env_pointer): once the env pointer is wired into the
        // snapshot, produce Ready { env_file_path, store_path } when the
        // pointer is populated, or Pending { hint } during evaluation. For
        // now, no field exists on the snapshot yet, so surface Missing.
        QueryReply::Devshell(Availability::Missing {
          reason: "env pointer not wired (task 2.3)".to_string(),
        })
      }
      Query::FlakeOutputs => {
        // TODO(task 2.5 flake_outputs): return the cached outputs blob once
        // the flake_outputs worker publishes it via the snapshot.
        QueryReply::FlakeOutputs(Availability::Missing {
          reason: "flake outputs not wired (task 2.5)".to_string(),
        })
      }
      Query::HeartBeat => QueryReply::HeartBeat,
      Query::StateCatalog => QueryReply::StateCatalog(
        state_meta::CATALOG.iter().copied().collect(),
      ),
      Query::Snapshot(requested_seq) => {
        let snap = handle.load();
        if snap.seq == requested_seq {
          QueryReply::Snapshot(StateSnapshot::clone(&snap))
        } else {
          QueryReply::Gap {
            requested_seq,
            current_seq: snap.seq,
          }
        }
      }
      Query::GetJob(_id) => {
        // TODO(task 2.2 job_registry): once the job registry is available,
        // consult it (still lock-free — the registry is expected to publish
        // an ArcSwap of its own). Until then, always Missing.
        QueryReply::Job(Availability::Missing {
          reason: "job registry not wired (task 2.2)".to_string(),
        })
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crossbeam_channel::bounded;

  fn make_handle(snap: StateSnapshot) -> StateHandle {
    Arc::new(ArcSwap::from_pointee(snap))
  }

  fn envelope(query: Query) -> (QueryEnvelope, Receiver<QueryReply>) {
    let (tx, rx) = bounded(1);
    (
      QueryEnvelope {
        query,
        reply_to: tx,
      },
      rx,
    )
  }

  #[test]
  fn status_reflects_snapshot_state() {
    let handle = make_handle(StateSnapshot {
      state: DaemonState::Ready,
      seq: 42,
      uptime_secs: 100,
      consumer_count: 3,
      package_count: 17,
      current_target: ".#default".to_string(),
      version: "test".to_string(),
    });

    let reply = StateResponder::answer(&handle, Query::Status);
    match reply {
      QueryReply::Status(Availability::Ready(payload)) => {
        assert_eq!(payload.state, DaemonState::Ready);
        assert_eq!(payload.seq, 42);
        assert_eq!(payload.uptime_secs, 100);
        assert_eq!(payload.consumer_count, 3);
        assert_eq!(payload.package_count, 17);
        assert_eq!(payload.current_target, ".#default");
        assert_eq!(payload.version, "test");
      }
      other => panic!("expected Status(Ready), got {other:?}"),
    }
  }

  #[test]
  fn heartbeat_replies_immediately() {
    let handle = make_handle(StateSnapshot::initial());
    let reply = StateResponder::answer(&handle, Query::HeartBeat);
    assert!(matches!(reply, QueryReply::HeartBeat));
  }

  #[test]
  fn state_catalog_matches_state_meta_catalog() {
    let handle = make_handle(StateSnapshot::initial());
    let reply = StateResponder::answer(&handle, Query::StateCatalog);
    match reply {
      QueryReply::StateCatalog(entries) => {
        assert_eq!(entries.len(), state_meta::CATALOG.len());
        for (a, b) in entries.iter().zip(state_meta::CATALOG.iter()) {
          assert_eq!(a.name, b.name);
          assert_eq!(a.description, b.description);
        }
      }
      other => panic!("expected StateCatalog, got {other:?}"),
    }
  }

  #[test]
  fn snapshot_query_returns_current_when_seq_matches() {
    let snap = StateSnapshot {
      seq: 7,
      ..StateSnapshot::initial()
    };
    let handle = make_handle(snap);
    let reply = StateResponder::answer(&handle, Query::Snapshot(7));
    assert!(matches!(reply, QueryReply::Snapshot(s) if s.seq == 7));
  }

  #[test]
  fn snapshot_query_returns_gap_when_seq_stale() {
    let snap = StateSnapshot {
      seq: 10,
      ..StateSnapshot::initial()
    };
    let handle = make_handle(snap);
    let reply = StateResponder::answer(&handle, Query::Snapshot(3));
    match reply {
      QueryReply::Gap {
        requested_seq,
        current_seq,
      } => {
        assert_eq!(requested_seq, 3);
        assert_eq!(current_seq, 10);
      }
      other => panic!("expected Gap, got {other:?}"),
    }
  }

  #[test]
  fn devshell_query_reports_missing_pending_task_23() {
    let handle = make_handle(StateSnapshot::initial());
    let reply = StateResponder::answer(&handle, Query::Devshell);
    match reply {
      QueryReply::Devshell(Availability::Missing { reason }) => {
        assert!(reason.contains("2.3"), "reason should cite task 2.3");
      }
      other => panic!("expected Devshell(Missing), got {other:?}"),
    }
  }

  #[test]
  fn flake_outputs_query_reports_missing_pending_task_25() {
    let handle = make_handle(StateSnapshot::initial());
    let reply = StateResponder::answer(&handle, Query::FlakeOutputs);
    match reply {
      QueryReply::FlakeOutputs(Availability::Missing { reason }) => {
        assert!(reason.contains("2.5"), "reason should cite task 2.5");
      }
      other => panic!("expected FlakeOutputs(Missing), got {other:?}"),
    }
  }

  #[test]
  fn get_job_reports_missing_pending_task_22() {
    let handle = make_handle(StateSnapshot::initial());
    let reply =
      StateResponder::answer(&handle, Query::GetJob("job-1".to_string()));
    match reply {
      QueryReply::Job(Availability::Missing { reason }) => {
        assert!(reason.contains("2.2"), "reason should cite task 2.2");
      }
      other => panic!("expected Job(Missing), got {other:?}"),
    }
  }

  #[test]
  fn spawned_responder_answers_via_channel() {
    let handle = make_handle(StateSnapshot {
      state: DaemonState::Ready,
      seq: 1,
      ..StateSnapshot::initial()
    });
    let (tx, rx) = bounded::<QueryEnvelope>(4);
    let join = StateResponder::spawn(Arc::clone(&handle), rx);

    let (env, reply_rx) = envelope(Query::HeartBeat);
    tx.send(env).expect("send query");
    let reply = reply_rx
      .recv_timeout(std::time::Duration::from_secs(2))
      .expect("recv reply");
    assert!(matches!(reply, QueryReply::HeartBeat));

    drop(tx);
    join.join().expect("responder thread joins cleanly");
  }

  #[test]
  fn published_snapshot_update_is_observed_within_one_roundtrip() {
    let handle = make_handle(StateSnapshot {
      state: DaemonState::Starting,
      seq: 1,
      ..StateSnapshot::initial()
    });
    let (tx, rx) = bounded::<QueryEnvelope>(4);
    let join = StateResponder::spawn(Arc::clone(&handle), rx);

    // First round-trip: state is Starting.
    let (env, reply_rx) = envelope(Query::Status);
    tx.send(env).expect("send first query");
    let first = reply_rx
      .recv_timeout(std::time::Duration::from_secs(2))
      .expect("first reply");
    match first {
      QueryReply::Status(Availability::Ready(p)) => {
        assert_eq!(p.state, DaemonState::Starting);
        assert_eq!(p.seq, 1);
      }
      other => panic!("expected first Status(Ready), got {other:?}"),
    }

    // Publish a new snapshot atomically.
    handle.store(Arc::new(StateSnapshot {
      state: DaemonState::Ready,
      seq: 2,
      ..StateSnapshot::initial()
    }));

    // Second round-trip: the responder must observe the swap.
    let (env, reply_rx) = envelope(Query::Status);
    tx.send(env).expect("send second query");
    let second = reply_rx
      .recv_timeout(std::time::Duration::from_secs(2))
      .expect("second reply");
    match second {
      QueryReply::Status(Availability::Ready(p)) => {
        assert_eq!(p.state, DaemonState::Ready);
        assert_eq!(p.seq, 2);
      }
      other => panic!("expected second Status(Ready), got {other:?}"),
    }

    drop(tx);
    join.join().expect("responder thread joins cleanly");
  }
}
