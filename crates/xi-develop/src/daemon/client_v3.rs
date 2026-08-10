//! v3 CQRS daemon client with v2 downgrade shim.
//!
//! Talks the same length-prefixed JSON framing as v2 (see
//! [`super::protocol::write_message`] / [`super::protocol::read_message`])
//! but exchanges [`Envelope`]s carrying a `"v":3` discriminator. On
//! version-negotiation failure — a daemon that only speaks v2 replies with
//! an untagged `DaemonResponse` or an I/O error — the shim falls back to
//! the legacy [`super::client`] path for that session.
//!
//! The client is stateless per call: every [`query`] / [`command`] opens
//! its own socket. Long-lived consumers (status bars, `xi develop events`)
//! use [`subscribe`] which returns the open stream for streaming reads.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use color_eyre::Result;
use color_eyre::eyre::{bail, eyre};
use ulid::Ulid;

use super::client as v2_client;
use super::protocol::{
  DaemonResponse, StatusResponse, read_frame, write_message,
};
use super::protocol_v3::{
  Command, CommandReply, Envelope, Payload, Query, QueryReply, Reply,
  Subscribe, Topics,
};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of a client call on the shared v2/v3 socket.
///
/// The legacy variant lets the caller know a v2 daemon answered, so it can
/// route to the v2 renderer instead of failing.
#[derive(Debug)]
pub enum QueryOutcome {
  /// The daemon speaks v3 and returned a typed [`QueryReply`].
  V3(QueryReply),
  /// The daemon only speaks v2 — the v2 response is included for the caller
  /// to map onto its own render path.
  V2(DaemonResponse),
}

/// Outcome of a v3 [`Command`] on the shared v2/v3 socket. Mirrors
/// [`QueryOutcome`] so callers can branch on version negotiation once.
#[derive(Debug)]
pub enum CommandOutcome {
  /// v3 daemon answered.
  V3(CommandReply),
  /// v2 daemon answered — commands aren't v2-mapped by the shim (there is no
  /// v2 equivalent for `EvalDevshell` etc.); callers get the raw response.
  V2(DaemonResponse),
}

/// Send a v3 [`Query`] on `socket_path`.
///
/// A v2 daemon that cannot parse the envelope falls through to the v2
/// codepath: the caller receives [`QueryOutcome::V2`] wrapping whatever the
/// v2 daemon produced.
///
/// # Errors
///
/// Returns an error only on connect / I/O failure; wire-level version
/// negotiation is expressed via [`QueryOutcome`], not an error.
pub fn query(socket_path: &Path, q: Query) -> Result<QueryOutcome> {
  let stream = connect(socket_path)?;
  let request = envelope(Payload::Query(q));
  write_envelope(&stream, &request)?;
  read_outcome(&stream, request.id.as_str(), OutcomeKind::Query)
}

/// Send a v3 [`Command`] on `socket_path`.
///
/// # Errors
///
/// Returns an error on connect / I/O failure, or when a v3 daemon replies
/// with a non-`Command` payload.
pub fn command(socket_path: &Path, c: Command) -> Result<CommandOutcome> {
  let stream = connect(socket_path)?;
  let request = envelope(Payload::Command(c));
  write_envelope(&stream, &request)?;

  let mut reader = BufReader::new(&stream);
  let raw = read_frame(&mut reader)?;

  if let Ok(env) = serde_json::from_slice::<Envelope>(&raw)
    && env.v == super::protocol_v3::V
    && env.id == request.id
  {
    return match env.payload {
      Payload::Reply(Reply::Command(cr)) => Ok(CommandOutcome::V3(cr)),
      Payload::Reply(Reply::Error { message }) => {
        bail!("daemon reply error: {message}")
      },
      other => bail!("unexpected v3 payload on command reply: {other:?}"),
    };
  }

  let response: DaemonResponse = serde_json::from_slice(&raw)
    .map_err(|e| eyre!("could not decode command reply as v3 or v2: {e}"))?;
  Ok(CommandOutcome::V2(response))
}

/// Short-hand: send `Query::HeartBeat` and report whether the daemon
/// answered with a v3 heartbeat.
///
/// Used as the version-probe: a `true` return proves v3 is speakable on
/// this socket. Anything else (v2 fallback, transport error) is `false`.
#[must_use]
pub fn heartbeat_ok(socket_path: &Path) -> bool {
  matches!(
    query(socket_path, Query::HeartBeat),
    Ok(QueryOutcome::V3(QueryReply::HeartBeat))
  )
}

/// Open a subscription to the given [`Topics`] and return the raw stream
/// plus the daemon's `SubscribeAck` metadata.
///
/// The caller drives event reads via [`read_event`] on the returned stream.
///
/// # Errors
///
/// Returns an error on connect / I/O failure or if the daemon replied with
/// a non-`SubscribeAck` envelope.
pub fn subscribe(
  socket_path: &Path,
  topics: Topics,
) -> Result<(UnixStream, u64)> {
  let stream = connect(socket_path)?;
  let request = envelope(Payload::Subscribe(Subscribe {
    topics,
    resume_from: None,
  }));
  write_envelope(&stream, &request)?;

  let mut reader = BufReader::new(&stream);
  let raw = read_frame(&mut reader)?;
  let reply: Envelope = serde_json::from_slice(&raw).map_err(|e| eyre!(e))?;
  if reply.id != request.id {
    bail!(
      "subscribe reply id mismatch: expected {}, got {}",
      request.id,
      reply.id
    );
  }
  match reply.payload {
    Payload::Reply(Reply::SubscribeAck { from_seq }) => Ok((stream, from_seq)),
    other => bail!("expected SubscribeAck, got {other:?}"),
  }
}

/// Read the next [`Envelope`] carrying an [`super::protocol_v3::Event`]
/// from an active subscription stream.
///
/// # Errors
///
/// Returns an error on I/O or if the envelope shape is not an `Event`.
pub fn read_event(stream: &UnixStream) -> Result<super::protocol_v3::Event> {
  let mut reader = BufReader::new(stream);
  let raw = read_frame(&mut reader)?;
  let env: Envelope = serde_json::from_slice(&raw).map_err(|e| eyre!(e))?;
  match env.payload {
    Payload::Event(e) => Ok(e),
    other => bail!("expected Event payload, got {other:?}"),
  }
}

/// Query the daemon status, mapping either v3 or v2 replies into the same
/// legacy [`StatusResponse`] shape so existing callers keep working.
///
/// v3 replies are transcoded from the [`QueryReply::Status`] JSON value.
///
/// # Errors
///
/// Returns an error on connect / I/O failure or when neither version
/// produced a parseable status.
pub fn status(socket_path: &Path) -> Result<StatusResponse> {
  match query(socket_path, Query::Status)? {
    QueryOutcome::V3(QueryReply::Status(availability)) => {
      status_from_availability(availability)
    },
    QueryOutcome::V3(other) => {
      bail!("daemon returned {other:?} for Query::Status")
    },
    QueryOutcome::V2(_) => v2_client::status(socket_path),
  }
}

// -----------------------------------------------------------------------
// internals
// -----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutcomeKind {
  Query,
  Command,
}

fn connect(socket_path: &Path) -> Result<UnixStream> {
  let stream = UnixStream::connect(socket_path)
    .map_err(|e| eyre!("Cannot connect to daemon: {e}"))?;
  stream.set_read_timeout(Some(IO_TIMEOUT))?;
  stream.set_write_timeout(Some(IO_TIMEOUT))?;
  Ok(stream)
}

fn envelope(payload: Payload) -> Envelope {
  Envelope::new(Ulid::new().to_string(), payload)
}

fn write_envelope(stream: &UnixStream, env: &Envelope) -> Result<()> {
  let mut writer = BufWriter::new(stream);
  write_message(&mut writer, env)?;
  Ok(())
}

/// Read one reply frame and interpret it under the shared v2/v3 shim.
///
/// The daemon frames both versions identically (length-prefixed JSON), so
/// we read the raw frame once and try v3 first (matching the expected `id`
/// on the envelope); if that fails, fall back to a v2 `DaemonResponse`
/// decode. Any leftover parse error is surfaced.
fn read_outcome(
  stream: &UnixStream,
  expected_id: &str,
  _kind: OutcomeKind,
) -> Result<QueryOutcome> {
  let mut reader = BufReader::new(stream);
  let raw = read_frame(&mut reader)?;

  // v3 first: envelope carries `"v":3` and echoes our request id.
  if let Ok(env) = serde_json::from_slice::<Envelope>(&raw)
    && env.v == super::protocol_v3::V
    && env.id == expected_id
  {
    return match env.payload {
      Payload::Reply(Reply::Query(qr)) => Ok(QueryOutcome::V3(qr)),
      Payload::Reply(Reply::Error { message }) => {
        bail!("daemon reply error: {message}")
      },
      Payload::Reply(Reply::Command(_)) => {
        bail!("daemon returned CommandReply on a query path")
      },
      Payload::Reply(Reply::SubscribeAck { .. }) => {
        bail!("daemon returned SubscribeAck on a query path")
      },
      other => bail!("unexpected v3 payload on reply: {other:?}"),
    };
  }

  // v2 fallback: raw is the legacy `DaemonResponse`.
  let response: DaemonResponse = serde_json::from_slice(&raw)
    .map_err(|e| eyre!("could not decode reply as v3 or v2: {e}"))?;
  Ok(QueryOutcome::V2(response))
}

fn status_from_availability(
  avail: super::protocol_v3::Availability<serde_json::Value>,
) -> Result<StatusResponse> {
  use super::protocol_v3::Availability;
  match avail {
    Availability::Ready { value } => serde_json::from_value(value.clone())
      .or_else(|_| synth_status_from_v3(&value))
      .map_err(|e| eyre!("could not decode v3 status: {e}")),
    Availability::Pending { job_id } => {
      bail!("daemon status pending (job {job_id})")
    },
    Availability::Missing => bail!("daemon status missing"),
    Availability::Degraded { reason } => {
      bail!("daemon status degraded: {reason}")
    },
  }
}

/// Best-effort synthesis of a legacy [`StatusResponse`] from the v3 status
/// JSON. v3 emits a flatter shape (`state`, `uptime_secs`, `current_target`,
/// `package_count`, `version`) which the older `StatusResponse` almost
/// mirrors; unfilled fields default to empty.
fn synth_status_from_v3(
  value: &serde_json::Value,
) -> std::result::Result<StatusResponse, serde_json::Error> {
  use super::protocol::DaemonState;
  let state: DaemonState = serde_json::from_value(
    value
      .get("state")
      .cloned()
      .unwrap_or_else(|| serde_json::Value::String("Ready".into())),
  )?;
  let uptime_secs = value
    .get("uptime_secs")
    .and_then(serde_json::Value::as_u64)
    .unwrap_or(0);
  let current_target = value
    .get("current_target")
    .and_then(|v| v.as_str())
    .unwrap_or("default")
    .to_owned();
  let package_count = value
    .get("package_count")
    .and_then(serde_json::Value::as_u64)
    .and_then(|n| u32::try_from(n).ok())
    .unwrap_or(0);
  let version = value
    .get("version")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_owned();
  Ok(StatusResponse {
    state,
    uptime_secs,
    consumer_count: 0,
    current_target,
    package_count,
    version,
    active_cache_pushes: Vec::new(),
  })
}

// Keep `CONNECT_TIMEOUT` alive for a future `connect_with_timeout` path
// (Rust's stdlib `UnixStream` has no direct connect-timeout knob and needs
// a socket-options dance; the daemon reconnect loop lives with the hook).
const _: Duration = CONNECT_TIMEOUT;
