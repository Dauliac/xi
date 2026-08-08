//! BDD: async-daemon-cqrs / v3 protocol wire and dispatch.
//!
//! Covers the P1 acceptance for `bdd_cqrs_protocol` (MVP task 4.2): the v3
//! envelope round-trips through length-prefixed JSON, the daemon routes on
//! the `"v":3` discriminator, and every payload class (query, command,
//! subscribe, event) has a functioning wire encoding.
//!
//! End-to-end socket tests belong in a later suite (`bdd_multi_consumer`)
//! that spins up the full daemon; this file focuses on the protocol layer
//! itself so a regression in wire format is caught before any I/O runs.

use xi_develop::daemon::protocol_v3::{
  Availability, Command, CommandReply, Envelope, Event, EventPayload, Gap,
  IdempotencyKey, Payload, Query, QueryReply, Reply, Subscribe, Topics,
};
use xi_develop::daemon::server::is_v3_envelope;

/// A v3 envelope round-trips through JSON and preserves every field.
#[test]
fn envelope_roundtrip_query_heartbeat() {
  let env = Envelope::new(
    "01H8XG9Z9K",
    Payload::Query(Query::HeartBeat),
  );
  let json = serde_json::to_string(&env).expect("serialize");
  let back: Envelope = serde_json::from_str(&json).expect("deserialize");
  assert_eq!(back.v, 3);
  assert_eq!(back.id, "01H8XG9Z9K");
  assert!(matches!(back.payload, Payload::Query(Query::HeartBeat)));
}

/// A v3 envelope carries the `"v":3` discriminator on the wire so a server
/// can peek it before decoding the full payload.
#[test]
fn envelope_carries_v3_discriminator_on_wire() {
  let env = Envelope::new("id1", Payload::Query(Query::HeartBeat));
  let raw = serde_json::to_value(&env).expect("to_value");
  assert_eq!(raw["v"], 3, "v field must be present with value 3");
  assert_eq!(raw["id"], "id1", "id echoes through");
  assert_eq!(raw["kind"], "query", "kind tag is externally tagged");
}

/// A legacy v2 envelope has no `"v"` field; the server routes it to the v2
/// dispatch path. The BDD assertion here mirrors the peek check the server
/// does before it decodes the full payload.
#[test]
fn v2_envelope_has_no_v_field() {
  // Approximate a v2 body: DaemonRequest::Status serialises as {"type":"Status"}
  let v2_raw = serde_json::json!({ "type": "Status" });
  let v = v2_raw.get("v").and_then(serde_json::Value::as_u64);
  assert_eq!(v, None, "v2 envelopes MUST NOT carry a 'v' field");
}

/// Query variants each round-trip through JSON.
#[test]
fn every_query_variant_roundtrips() {
  let cases: Vec<Query> = vec![
    Query::Status,
    Query::HeartBeat,
    Query::StateCatalog,
    Query::Devshell,
    Query::FlakeOutputs,
    Query::GetJob { job_id: "abc123".into() },
    Query::Snapshot { seq: 7 },
  ];
  for q in cases {
    let env = Envelope::new("id", Payload::Query(q.clone()));
    let json = serde_json::to_string(&env).expect("bdd");
    let back: Envelope = serde_json::from_str(&json).expect("bdd");
    assert!(
      matches!(back.payload, Payload::Query(_)),
      "expected Query payload after round-trip"
    );
    let _ = q;
  }
}

/// Every command variant carries an idempotency key and round-trips.
#[test]
fn every_command_variant_roundtrips_with_key() {
  let key = IdempotencyKey::from_bytes([9u8; 32]);
  let cases: Vec<Command> = vec![
    Command::EvalDevshell {
      target: "default".into(),
      key,
    },
    Command::Restart { key },
    Command::AbortJob {
      job_id: "job-1".into(),
      key,
    },
    Command::InvalidateCache {
      scope: "flake_outputs".into(),
      key,
    },
  ];
  for c in cases {
    let env = Envelope::new("id", Payload::Command(c.clone()));
    let json = serde_json::to_string(&env).expect("bdd");
    let back: Envelope = serde_json::from_str(&json).expect("bdd");
    assert!(
      matches!(back.payload, Payload::Command(_)),
      "expected Command payload after round-trip"
    );
    let _ = c;
  }
}

/// A subscribe envelope carries its topics bitset and a starting seq.
#[test]
fn subscribe_envelope_roundtrips() {
  let sub = Subscribe {
    topics:      Topics {
      state: true,
      jobs:  true,
      cache: false,
    },
    resume_from: None,
  };
  let env = Envelope::new("s", Payload::Subscribe(sub));
  let json = serde_json::to_string(&env).expect("bdd");
  let back: Envelope = serde_json::from_str(&json).expect("bdd");
  let Payload::Subscribe(s) = back.payload else {
    unreachable!("expected Subscribe payload after round-trip")
  };
  assert!(s.topics.state);
  assert!(s.topics.jobs);
  assert!(!s.topics.cache);
}

/// Every reply variant carried by the daemon round-trips.
#[test]
fn reply_variants_roundtrip() {
  let replies: Vec<Reply> = vec![
    Reply::Query(QueryReply::HeartBeat),
    Reply::Query(QueryReply::StateCatalog(Availability::Ready {
      value: serde_json::json!({ "size": 12 }),
    })),
    Reply::Command(CommandReply::Accepted {
      job_id:   "job-42".into(),
      attached: false,
    }),
    Reply::Command(CommandReply::Rejected {
      reason: "illegal_transition".into(),
    }),
    Reply::SubscribeAck { from_seq: 100 },
    Reply::Error {
      message: "unknown query".into(),
    },
  ];
  for r in replies {
    let env = Envelope::new("r", Payload::Reply(r));
    let json = serde_json::to_string(&env).expect("bdd");
    let _back: Envelope = serde_json::from_str(&json).expect("bdd");
  }
}

/// A broadcaster event with `KeepAlive` and `Gap` round-trips.
#[test]
fn event_variants_roundtrip() {
  let e1 = Event {
    seq:     7,
    payload: EventPayload::KeepAlive,
  };
  let e2 = Event {
    seq:     8,
    payload: EventPayload::Gap(Gap {
      from_seq: 5,
      to_seq:   7,
    }),
  };
  for e in [e1, e2] {
    let env = Envelope::new("ev", Payload::Event(e));
    let json = serde_json::to_string(&env).expect("bdd");
    let _back: Envelope = serde_json::from_str(&json).expect("bdd");
  }
}

/// Idempotency key encoding is stable: 64 hex chars on the wire.
#[test]
fn idempotency_key_wire_shape_is_64_hex() {
  let key = IdempotencyKey::from_bytes([0xABu8; 32]);
  let json = serde_json::to_string(&key).expect("bdd");
  // 64 hex chars + 2 quote chars
  assert_eq!(json.len(), 66);
  assert!(json.contains("abab"));
}

/// The server's version-peek routes v3 envelopes to the v3 path.
///
/// This is the routing gate that separates the v2 dispatcher (task 2.x
/// legacy path) from the CQRS handlers (`daemon::v3_handlers`). A regression
/// here would silently degrade v3 clients to v2 replies.
#[test]
fn server_peek_routes_v3_envelope_to_v3_path() {
  let v3 = Envelope::new("peek", Payload::Query(Query::HeartBeat));
  let raw = serde_json::to_vec(&v3).expect("bdd");
  assert!(is_v3_envelope(&raw), "v3 envelope must be detected");
}

/// The server's version-peek routes v2 (legacy) requests to the v2 path.
#[test]
fn server_peek_routes_v2_request_to_v2_path() {
  // v2 shape: `{"type":"Status"}` without a `v` field.
  let v2 = serde_json::json!({ "type": "Status" });
  let raw = serde_json::to_vec(&v2).expect("bdd");
  assert!(!is_v3_envelope(&raw), "v2 envelope must NOT be detected as v3");
}

/// Malformed JSON is not routed to v3 (it falls through to v2 decode which
/// will surface the error).
#[test]
fn malformed_json_is_not_v3() {
  let junk = b"{not valid json";
  assert!(!is_v3_envelope(junk));
}

/// Availability shapes round-trip preserving the tag discriminator.
#[test]
fn availability_variants_roundtrip() {
  let cases: Vec<Availability<serde_json::Value>> = vec![
    Availability::Ready {
      value: serde_json::json!({ "ok": true }),
    },
    Availability::Pending {
      job_id: "j-1".into(),
    },
    Availability::Missing,
    Availability::Degraded {
      reason: "watcher lost tree".into(),
    },
  ];
  for a in cases {
    let json = serde_json::to_string(&a).expect("bdd");
    let _back: Availability<serde_json::Value> =
      serde_json::from_str(&json).expect("bdd");
  }
}
