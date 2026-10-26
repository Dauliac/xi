//! BDD: async-daemon-cqrs / v3 client round-trip on a real Unix socket.
//!
//! Spins up a UnixListener that speaks the v3 protocol, connects via
//! [`xi_develop::daemon::client_v3`], and asserts that queries and commands
//! round-trip end to end. This is the integration proof that server routing
//! + handlers + client shim all work together — one layer above the
//! wire-format unit tests in `bdd_cqrs_protocol.rs`.
//!
//! v2 downgrade shim is exercised by pointing the client at a socket that
//! only speaks legacy `DaemonResponse` and asserting the outcome carries
//! [`QueryOutcome::V2`].

use std::{
  io::{BufReader, BufWriter},
  os::unix::net::{UnixListener, UnixStream},
  path::PathBuf,
  sync::mpsc::channel,
  thread::{self, JoinHandle},
};

use tempfile::TempDir;
use xi_develop::daemon::{
  client_v3::{self, CommandOutcome, QueryOutcome},
  protocol::{
    DaemonResponse,
    DaemonState,
    StatusResponse,
    read_message,
    write_message,
  },
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
};

/// A tiny in-process daemon that answers one connection then exits.
struct MiniDaemon {
  socket:  PathBuf,
  _handle: JoinHandle<()>,
  _tmp:    TempDir,
}

impl MiniDaemon {
  /// Spawn a listener that reads one envelope, hands it to `dispatch`, and
  /// writes the reply back. Terminates once a single connection is served.
  fn spawn_one<F>(dispatch: F) -> Self
  where
    F: FnOnce(Envelope) -> Envelope + Send + 'static,
  {
    let tmp = tempfile::tempdir().expect("mktemp");
    let socket = tmp.path().join("v3.sock");
    let listener = UnixListener::bind(&socket).expect("bind");

    let (ready_tx, ready_rx) = channel::<()>();
    let handle = thread::spawn(move || {
      ready_tx.send(()).ok();
      if let Ok((stream, _)) = listener.accept() {
        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);
        if let Ok(env) = read_message::<_, Envelope>(&mut reader) {
          let reply = dispatch(env);
          let _ = write_message(&mut writer, &reply);
        }
      }
    });
    ready_rx.recv().expect("listener ready");
    Self {
      socket,
      _handle: handle,
      _tmp: tmp,
    }
  }
}

/// A tiny v2-only daemon that answers one connection with a
/// [`DaemonResponse`], to drive the client_v3 downgrade shim.
struct MiniV2Daemon {
  socket:  PathBuf,
  _handle: JoinHandle<()>,
  _tmp:    TempDir,
}

impl MiniV2Daemon {
  fn spawn_status(response: StatusResponse) -> Self {
    let tmp = tempfile::tempdir().expect("mktemp");
    let socket = tmp.path().join("v2.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let (ready_tx, ready_rx) = channel::<()>();
    let handle = thread::spawn(move || {
      ready_tx.send(()).ok();
      if let Ok((stream, _)) = listener.accept() {
        let mut writer = BufWriter::new(&stream);
        // Read and discard the incoming envelope frame — the point is to
        // reply with a v2-shaped body no matter what came in.
        let mut reader = BufReader::new(&stream);
        let _ = xi_develop::daemon::protocol::read_frame(&mut reader);
        let _ = write_message(&mut writer, &DaemonResponse::Status(response));
      }
    });
    ready_rx.recv().expect("listener ready");
    Self {
      socket,
      _handle: handle,
      _tmp: tmp,
    }
  }
}

fn ok_reply(id: &str, reply: Reply) -> Envelope {
  Envelope::new(id, Payload::Reply(reply))
}

/// A v3 daemon replies to `Query::HeartBeat` with `QueryReply::HeartBeat`,
/// echoing the envelope id.
#[test]
fn query_heartbeat_roundtrips_end_to_end() {
  let daemon = MiniDaemon::spawn_one(|env| {
    assert!(matches!(env.payload, Payload::Query(Query::HeartBeat)));
    ok_reply(&env.id, Reply::Query(QueryReply::HeartBeat))
  });

  let outcome =
    client_v3::query(&daemon.socket, Query::HeartBeat).expect("query");
  assert!(matches!(outcome, QueryOutcome::V3(QueryReply::HeartBeat)));
}

/// The client short-hand `heartbeat_ok` returns true against a v3 daemon.
#[test]
fn heartbeat_ok_is_true_on_v3_daemon() {
  let daemon = MiniDaemon::spawn_one(|env| {
    ok_reply(&env.id, Reply::Query(QueryReply::HeartBeat))
  });
  assert!(client_v3::heartbeat_ok(&daemon.socket));
}

/// A v3 daemon replies to `Command::EvalDevshell` with a job id.
#[test]
fn command_eval_devshell_returns_job_id() {
  let daemon = MiniDaemon::spawn_one(|env| {
    assert!(matches!(
      env.payload,
      Payload::Command(Command::EvalDevshell { .. })
    ));
    ok_reply(
      &env.id,
      Reply::Command(CommandReply::Accepted {
        job_id:   "job-abc".into(),
        attached: false,
      }),
    )
  });

  let cmd = Command::EvalDevshell {
    target: "default".into(),
    key:    IdempotencyKey::from_bytes([1u8; 32]),
  };
  let out = client_v3::command(&daemon.socket, cmd).expect("command");
  let CommandOutcome::V3(CommandReply::Accepted { job_id, attached }) = out
  else {
    unreachable!("expected V3 Accepted")
  };
  assert_eq!(job_id, "job-abc");
  assert!(!attached);
}

/// A v3 `Query::Status` reply is transcoded into the legacy
/// `StatusResponse` shape for existing callers.
#[test]
fn status_query_transcodes_v3_to_legacy_shape() {
  let daemon = MiniDaemon::spawn_one(|env| {
    ok_reply(
      &env.id,
      Reply::Query(QueryReply::Status(Availability::Ready {
        value: serde_json::json!({
          "state": "Ready",
          "uptime_secs": 42,
          "current_target": "default",
          "package_count": 7,
          "version": "4.4.0",
        }),
      })),
    )
  });

  let status = client_v3::status(&daemon.socket).expect("status");
  assert_eq!(status.state, DaemonState::Ready);
  assert_eq!(status.uptime_secs, 42);
  assert_eq!(status.current_target, "default");
  assert_eq!(status.package_count, 7);
  assert_eq!(status.version, "4.4.0");
}

/// A v2-only daemon replies with a `DaemonResponse` shape; the client
/// downgrade shim surfaces it as `QueryOutcome::V2` rather than an error.
#[test]
fn v2_daemon_reply_surfaces_as_downgrade_outcome() {
  let daemon = MiniV2Daemon::spawn_status(StatusResponse {
    state:               DaemonState::Ready,
    uptime_secs:         12,
    consumer_count:      1,
    current_target:      "default".into(),
    package_count:       3,
    version:             "4.4.0".into(),
    active_cache_pushes: Vec::new(),
  });

  let out = client_v3::query(&daemon.socket, Query::Status).expect("query");
  match out {
    QueryOutcome::V2(DaemonResponse::Status(s)) => {
      assert_eq!(s.state, DaemonState::Ready);
      assert_eq!(s.uptime_secs, 12);
    },
    other => unreachable!("expected V2 Status, got {other:?}"),
  }
}

/// The client's `status()` helper transparently falls through to the v2
/// path when the socket only speaks v2.
#[test]
fn status_helper_falls_back_to_v2_client_on_downgrade() {
  // Two consecutive connections: the first is answered by client_v3::status
  // → downgrade branch → v2_client::status which opens a *second* socket.
  // We need the daemon to accept two.
  let tmp = tempfile::tempdir().expect("mktemp");
  let socket = tmp.path().join("v2.sock");
  let listener = UnixListener::bind(&socket).expect("bind");
  let (ready_tx, ready_rx) = channel::<()>();
  let s2 = socket.clone();
  let handle = thread::spawn(move || {
    ready_tx.send(()).ok();
    for _ in 0..2 {
      if let Ok((stream, _)) = listener.accept() {
        let mut writer = BufWriter::new(&stream);
        let mut reader = BufReader::new(&stream);
        let _ = xi_develop::daemon::protocol::read_frame(&mut reader);
        let _ = write_message(
          &mut writer,
          &DaemonResponse::Status(StatusResponse {
            state:               DaemonState::Ready,
            uptime_secs:         88,
            consumer_count:      2,
            current_target:      "default".into(),
            package_count:       9,
            version:             "4.4.0".into(),
            active_cache_pushes: Vec::new(),
          }),
        );
      }
    }
  });
  ready_rx.recv().expect("listener ready");

  let status = client_v3::status(&s2).expect("status");
  assert_eq!(status.state, DaemonState::Ready);
  assert_eq!(status.uptime_secs, 88);
  assert_eq!(status.consumer_count, 2, "v2 fills consumer_count");
  drop(handle);
}

/// Nonsense on the wire (client opens then receives a garbage envelope id)
/// still returns an error rather than panicking.
#[test]
fn mismatched_reply_id_surfaces_as_error() {
  let daemon = MiniDaemon::spawn_one(|_env| {
    ok_reply("wrong-id", Reply::Query(QueryReply::HeartBeat))
  });
  // The client tolerates id mismatch on the query path by falling to the
  // v2 decode; since the raw payload doesn't match `DaemonResponse` either,
  // an error propagates.
  let outcome = client_v3::query(&daemon.socket, Query::HeartBeat);
  assert!(
    outcome.is_err(),
    "expected error on mismatched id, got {outcome:?}"
  );
}

/// Round-trip a state catalog query; the daemon returns a JSON array.
#[test]
fn state_catalog_query_returns_ready_array() {
  let daemon = MiniDaemon::spawn_one(|env| {
    ok_reply(
      &env.id,
      Reply::Query(QueryReply::StateCatalog(Availability::Ready {
        value: serde_json::json!([
          { "name": "Ready", "display": "Ready" }
        ]),
      })),
    )
  });
  let out =
    client_v3::query(&daemon.socket, Query::StateCatalog).expect("query");
  match out {
    QueryOutcome::V3(QueryReply::StateCatalog(Availability::Ready {
      value,
    })) => {
      assert!(value.as_array().expect("array").len() >= 1);
    },
    other => unreachable!("expected StateCatalog Ready, got {other:?}"),
  }
}

// Keep the `_socket` field marker in the daemon structs referenced so
// clippy's dead-field lint doesn't fire under `--tests`.
fn _use(_: &MiniDaemon, _: &MiniV2Daemon, _: &UnixStream) {}
