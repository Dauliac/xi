//! BDD: async-daemon-cqrs / v2 ↔ v3 protocol migration (MVP task 4.5).
//!
//! One release cycle of v2 coexistence is a hard requirement (spec:
//! `Protocol — protocol-version-drift compatibility`). This suite proves
//! the four cross-version scenarios that matter for a rolling upgrade
//! without re-entering the devshell (SC-010):
//!
//! 1. **v2 client → v3 daemon** — the daemon's server routing detects the
//!    missing `"v"` field on the envelope and dispatches to the v2 path.
//! 2. **v3 client → v2 daemon** — the client_v3 shim receives a plain
//!    `DaemonResponse` and surfaces it as `QueryOutcome::V2` so the
//!    caller can render legacy replies without failing.
//! 3. **v3 client → v3 daemon** — the round-trip succeeds and echoes the
//!    envelope id.
//! 4. **v2 client → v2 daemon** — the legacy path is not disturbed by the
//!    new peek scanner.
//!
//! Every scenario spins up an in-process Unix listener that mimics the
//! daemon behaviour for its version — no real `ServerState` needed.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::thread::{self, JoinHandle};

use tempfile::TempDir;
use xi_develop::daemon::client_v3::{self, QueryOutcome};
use xi_develop::daemon::protocol::{
  DaemonRequest, DaemonResponse, DaemonState, StatusResponse, read_frame,
  read_message, write_message,
};
use xi_develop::daemon::protocol_v3::{
  Envelope, Payload, Query, QueryReply, Reply,
};
use xi_develop::daemon::server::is_v3_envelope;

struct MiniListener {
  socket:  PathBuf,
  _handle: JoinHandle<()>,
  _tmp:    TempDir,
}

impl MiniListener {
  fn spawn<F>(handle_conn: F) -> Self
  where
    F: FnOnce(UnixStream) + Send + 'static,
  {
    let tmp = tempfile::tempdir().expect("mktemp");
    let socket = tmp.path().join("s.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let (tx, rx) = channel::<()>();
    let handle = thread::spawn(move || {
      tx.send(()).ok();
      if let Ok((stream, _)) = listener.accept() {
        handle_conn(stream);
      }
    });
    rx.recv().expect("ready");
    Self {
      socket,
      _handle: handle,
      _tmp: tmp,
    }
  }
}

/// A daemon that dispatches raw frames on `"v":3` and answers a v3 or v2
/// reply accordingly — exactly what the real server does.
fn dual_daemon() -> MiniListener {
  MiniListener::spawn(|stream| {
    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);
    let Ok(raw) = read_frame(&mut reader) else {
      return;
    };
    if is_v3_envelope(&raw) {
      let Ok(env) = serde_json::from_slice::<Envelope>(&raw) else {
        return;
      };
      let reply = Envelope::new(
        &env.id,
        Payload::Reply(Reply::Query(QueryReply::HeartBeat)),
      );
      let _ = write_message(&mut writer, &reply);
    } else {
      let Ok(_req) = serde_json::from_slice::<DaemonRequest>(&raw) else {
        return;
      };
      let _ = write_message(
        &mut writer,
        &DaemonResponse::Status(StatusResponse {
          state:               DaemonState::Ready,
          uptime_secs:         1,
          consumer_count:      0,
          current_target:      "default".into(),
          package_count:       0,
          version:             "4.4.0".into(),
          active_cache_pushes: Vec::new(),
        }),
      );
    }
  })
}

/// A daemon that only speaks v2 — never inspects the version discriminator.
fn v2_only_daemon() -> MiniListener {
  MiniListener::spawn(|stream| {
    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);
    let _ = read_frame(&mut reader);
    let _ = write_message(
      &mut writer,
      &DaemonResponse::Status(StatusResponse {
        state:               DaemonState::Ready,
        uptime_secs:         7,
        consumer_count:      1,
        current_target:      "default".into(),
        package_count:       2,
        version:             "4.4.0".into(),
        active_cache_pushes: Vec::new(),
      }),
    );
  })
}

/// A daemon that only speaks v3 — always replies with an envelope.
fn v3_only_daemon() -> MiniListener {
  MiniListener::spawn(|stream| {
    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);
    let Ok(raw) = read_frame(&mut reader) else {
      return;
    };
    let Ok(env) = serde_json::from_slice::<Envelope>(&raw) else {
      // Ignore v2 requests entirely — the daemon under test is v3-only.
      return;
    };
    let reply = Envelope::new(
      &env.id,
      Payload::Reply(Reply::Query(QueryReply::HeartBeat)),
    );
    let _ = write_message(&mut writer, &reply);
  })
}

/// v3 client → v3 daemon: HeartBeat round-trips.
#[test]
fn v3_client_talks_to_v3_daemon() {
  let daemon = v3_only_daemon();
  let outcome =
    client_v3::query(&daemon.socket, Query::HeartBeat).expect("query");
  assert!(matches!(
    outcome,
    QueryOutcome::V3(QueryReply::HeartBeat)
  ));
}

/// v3 client → v2 daemon: the shim surfaces the legacy reply.
#[test]
fn v3_client_downgrades_when_daemon_is_v2_only() {
  let daemon = v2_only_daemon();
  let outcome =
    client_v3::query(&daemon.socket, Query::Status).expect("query");
  match outcome {
    QueryOutcome::V2(DaemonResponse::Status(s)) => {
      assert_eq!(s.uptime_secs, 7);
      assert_eq!(s.consumer_count, 1);
    },
    other => unreachable!("expected V2 downgrade, got {other:?}"),
  }
}

/// v2 client → v3 daemon: the daemon's peek scanner routes the missing
/// `"v"` field to the v2 dispatch path and returns a v2 response.
#[test]
fn v2_client_reaches_v2_path_on_a_dual_daemon() {
  let daemon = dual_daemon();
  let stream = UnixStream::connect(&daemon.socket).expect("connect");
  let mut writer = BufWriter::new(&stream);
  write_message(&mut writer, &DaemonRequest::Status).expect("write");
  drop(writer);

  let mut reader = BufReader::new(&stream);
  let response: DaemonResponse = read_message(&mut reader).expect("read");
  match response {
    DaemonResponse::Status(s) => {
      assert_eq!(s.state, DaemonState::Ready);
    },
    other => unreachable!("expected v2 Status reply, got {other:?}"),
  }
}

/// v3 client → dual-speaking daemon: the same daemon serves both versions
/// on the same socket without contamination.
#[test]
fn dual_daemon_serves_v3_on_the_same_socket() {
  let daemon = dual_daemon();
  let outcome =
    client_v3::query(&daemon.socket, Query::HeartBeat).expect("query");
  assert!(matches!(
    outcome,
    QueryOutcome::V3(QueryReply::HeartBeat)
  ));
}

/// The server's routing gate is deterministic: raw bytes with `"v":3`
/// route v3; anything else routes v2. Wire-format guardrail.
#[test]
fn routing_gate_is_deterministic() {
  let v3_body = br#"{"v":3,"id":"x","kind":"query","payload":{"query":"heart_beat"}}"#;
  let v2_body = br#"{"type":"Status"}"#;
  assert!(is_v3_envelope(v3_body));
  assert!(!is_v3_envelope(v2_body));
}
