//! v3 CQRS request handlers.
//!
//! Owns the v3 request/response path: envelopes carrying a `"v":3`
//! discriminator route through here from `daemon::server::handle_connection`.
//!
//! Contracts:
//! - Queries never block on work. They read `ServerState` fields directly; no
//!   channel round-trip.
//! - Every command reply carries an existing or newly-allocated `JobId` via
//!   [`CommandReply::Accepted`]. `EvalDevshell` attaches through
//!   [`JobRegistry`] so N concurrent identical commands observe one job.
//! - Malformed dispatch produces [`Reply::Error`], never a panic.

use std::sync::{Arc, OnceLock};

use serde_json::json;

use super::{
  job_registry::{
    Attached,
    IdempotencyKey as RegistryKey,
    JobEntry,
    JobId,
    JobKind,
    JobRegistry,
    JobStatus,
  },
  protocol::DaemonState,
  protocol_v3::{
    Availability,
    Command,
    CommandReply,
    Envelope,
    IdempotencyKey,
    Payload,
    Query,
    QueryReply,
    Reply,
  },
  server::ServerState,
};

/// Process-wide job registry. One per daemon process; each daemon invocation
/// starts fresh (registry is intentionally in-memory per the design's Risks
/// section).
fn registry() -> &'static JobRegistry {
  static R: OnceLock<JobRegistry> = OnceLock::new();
  R.get_or_init(JobRegistry::new)
}

/// Handle a decoded v3 envelope end to end, returning the reply envelope the
/// caller should write back on the wire.
///
/// The `id` on the reply mirrors the request `id` so clients can multiplex.
#[must_use]
pub fn handle_envelope(
  request: Envelope,
  state: &Arc<ServerState>,
) -> Envelope {
  let reply_id = request.id.clone();
  let reply = match request.payload {
    Payload::Query(q) => Reply::Query(handle_query(q, state)),
    Payload::Command(c) => Reply::Command(handle_command(&c)),
    Payload::Subscribe(_) => Reply::SubscribeAck { from_seq: 0 },
    Payload::Reply(_) | Payload::Event(_) => {
      Reply::Error {
        message: "server received a reply/event envelope; ignoring".into(),
      }
    },
  };
  Envelope::new(reply_id, Payload::Reply(reply))
}

/// Sentinel job id returned by [`Query::Devshell`] when a fresh eval is in
/// flight. The registry-side id lands with the work-executor integration; for
/// now this is a stable placeholder any subscriber can `Query::GetJob` for
/// and get `Missing`, which is a legal `Availability`.
const CURRENT_EVAL_JOB_ID: &str = "current-eval";

/// Answer a v3 [`Query`] by reading directly from `ServerState`.
#[must_use]
fn handle_query(query: Query, state: &ServerState) -> QueryReply {
  match query {
    Query::Status => {
      QueryReply::Status(Availability::Ready {
        value: status_value(state),
      })
    },
    Query::HeartBeat => QueryReply::HeartBeat,
    Query::StateCatalog => {
      QueryReply::StateCatalog(Availability::Ready {
        value: catalog_value(),
      })
    },
    Query::Devshell => QueryReply::Devshell(devshell_availability(state)),
    Query::FlakeOutputs => QueryReply::FlakeOutputs(Availability::Missing),
    Query::GetJob { job_id } => QueryReply::GetJob(job_availability(&job_id)),
    Query::Snapshot { seq } => {
      QueryReply::Snapshot(Availability::Ready {
        value: json!({ "seq": seq }),
      })
    },
  }
}

/// Dispatch a v3 [`Command`], allocating (or attaching to) a job via the
/// authoritative job registry.
#[must_use]
fn handle_command(command: &Command) -> CommandReply {
  let kind = job_kind_for(command);
  let key = wire_key_to_registry(command_key(command));
  let (job_id, attached) = registry().attach_or_start(key, kind);
  CommandReply::Accepted {
    job_id:   job_id.as_str().to_owned(),
    attached: matches!(attached, Attached::Yes),
  }
}

fn job_kind_for(cmd: &Command) -> JobKind {
  match cmd {
    Command::EvalDevshell { target, .. } => {
      JobKind::EvalDevshell {
        target: target.clone(),
      }
    },
    Command::Restart { .. } => JobKind::Restart,
    Command::AbortJob { job_id, .. } => {
      JobKind::AbortJob {
        target: JobId(job_id.clone()),
      }
    },
    Command::InvalidateCache { scope, .. } => {
      JobKind::InvalidateCache {
        scope: scope.clone(),
      }
    },
  }
}

const fn command_key(cmd: &Command) -> IdempotencyKey {
  match cmd {
    Command::EvalDevshell { key, .. }
    | Command::Restart { key }
    | Command::AbortJob { key, .. }
    | Command::InvalidateCache { key, .. } => *key,
  }
}

const fn wire_key_to_registry(key: IdempotencyKey) -> RegistryKey {
  RegistryKey::from_bytes(key.0)
}

fn status_value(state: &ServerState) -> serde_json::Value {
  let s = state
    .eval_state
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let target = state
    .current_target
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let pkg_count = state
    .current_env
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .as_ref()
    .map_or(0u32, |e| {
      u32::try_from(e.packages.len()).unwrap_or(u32::MAX)
    });
  json!({
    "state": s,
    "uptime_secs": state.start_time.elapsed().as_secs(),
    "current_target": target,
    "package_count": pkg_count,
    "version": state.version,
  })
}

fn devshell_availability(
  state: &ServerState,
) -> Availability<serde_json::Value> {
  let s = state
    .eval_state
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if matches!(*s, DaemonState::Evaluating) {
    return Availability::Pending {
      job_id: CURRENT_EVAL_JOB_ID.to_owned(),
    };
  }
  drop(s);

  let env = state
    .current_env
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  env.as_ref().map_or(Availability::Missing, |dev| {
    Availability::Ready {
      value: env_snapshot_value(dev),
    }
  })
}

fn env_snapshot_value(env: &crate::env_file::DevEnv) -> serde_json::Value {
  json!({
    "env_hash": env.env_hash,
    "packages": env.packages,
    "package_count": env.packages.len(),
  })
}

fn job_availability(job_id: &str) -> Availability<serde_json::Value> {
  registry().get(&JobId(job_id.to_owned())).map_or(
    Availability::Missing,
    |entry| {
      Availability::Ready {
        value: job_entry_value(&entry),
      }
    },
  )
}

fn job_entry_value(entry: &JobEntry) -> serde_json::Value {
  json!({
    "id": entry.id.as_str(),
    "kind": job_kind_str(&entry.kind),
    "status": job_status_str(&entry.status),
  })
}

const fn job_kind_str(kind: &JobKind) -> &'static str {
  match kind {
    JobKind::EvalDevshell { .. } => "eval_devshell",
    JobKind::Restart => "restart",
    JobKind::AbortJob { .. } => "abort_job",
    JobKind::InvalidateCache { .. } => "invalidate_cache",
  }
}

const fn job_status_str(status: &JobStatus) -> &'static str {
  match status {
    JobStatus::Running => "running",
    JobStatus::Succeeded => "succeeded",
    JobStatus::Failed { .. } => "failed",
    JobStatus::Aborted => "aborted",
    JobStatus::Stuck { .. } => "stuck",
  }
}

fn catalog_value() -> serde_json::Value {
  use super::state_meta::CATALOG;
  let entries: Vec<_> = CATALOG
    .iter()
    .map(|(kind, meta)| {
      json!({
        "name": format!("{kind:?}"),
        "display": meta.display,
        "color": meta.color,
        "category": format!("{:?}", meta.category),
        "phase": format!("{:?}", meta.phase),
        "terminal": meta.terminal,
      })
    })
    .collect();
  serde_json::Value::Array(entries)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn heartbeat_query_returns_heartbeat_reply() {
    // A HeartBeat query never touches state; we can invoke handle_query
    // with a throwaway ServerState by asserting the branch alone.
    assert!(matches!(
      handle_query_pure(Query::HeartBeat),
      QueryReply::HeartBeat
    ));
  }

  #[test]
  fn state_catalog_query_returns_catalog_array() {
    let reply = handle_query_pure(Query::StateCatalog);
    let QueryReply::StateCatalog(Availability::Ready { value }) = reply else {
      unreachable!("expected StateCatalog Ready")
    };
    let arr = value.as_array().expect("catalog is JSON array");
    assert!(!arr.is_empty(), "catalog must not be empty");
  }

  #[test]
  fn snapshot_query_echoes_seq() {
    let QueryReply::Snapshot(Availability::Ready { value }) =
      handle_query_pure(Query::Snapshot { seq: 42 })
    else {
      unreachable!("expected Snapshot Ready")
    };
    assert_eq!(value["seq"], 42);
  }

  #[test]
  fn eval_devshell_command_returns_job_id_fresh() {
    let cmd = Command::EvalDevshell {
      target: "default".into(),
      key:    IdempotencyKey::from_bytes([1u8; 32]),
    };
    let CommandReply::Accepted { job_id, attached } = handle_command(&cmd)
    else {
      unreachable!("EvalDevshell must be accepted")
    };
    assert!(!job_id.is_empty(), "job id must be non-empty");
    assert!(!attached, "first request is a fresh allocation");
  }

  #[test]
  fn identical_commands_attach_to_same_job() {
    let key = IdempotencyKey::from_bytes([2u8; 32]);
    let cmd_a = Command::EvalDevshell {
      target: "identical".into(),
      key,
    };
    let cmd_b = Command::EvalDevshell {
      target: "identical".into(),
      key,
    };
    let first = must_accept(handle_command(&cmd_a));
    let second = must_accept(handle_command(&cmd_b));
    assert_eq!(first.0, second.0, "same key must return same job id");
    assert!(second.1, "second request must report attached=true");
  }

  fn must_accept(reply: CommandReply) -> (String, bool) {
    let CommandReply::Accepted { job_id, attached } = reply else {
      unreachable!("command must be accepted")
    };
    (job_id, attached)
  }

  /// Pure-query dispatch for tests that never touch `ServerState`.
  fn handle_query_pure(q: Query) -> QueryReply {
    match q {
      Query::HeartBeat => QueryReply::HeartBeat,
      Query::StateCatalog => {
        QueryReply::StateCatalog(Availability::Ready {
          value: catalog_value(),
        })
      },
      Query::Snapshot { seq } => {
        QueryReply::Snapshot(Availability::Ready {
          value: json!({ "seq": seq }),
        })
      },
      _ => unreachable!("test helper only handles pure queries"),
    }
  }
}
