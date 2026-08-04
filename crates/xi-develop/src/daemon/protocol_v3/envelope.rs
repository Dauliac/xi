//! v3 envelope — versioned discriminator + typed payload.
//!
//! Every wire message carries `{ "v":3, "id": <ulid>, "kind": …, "payload": … }`.
//! Absence of `"v"` on ingress signals a v2 client (see `crate::daemon::protocol`).
//! The `kind` tag drives payload dispatch on the receive path.
//!
//! Length-prefixed JSON framing (4-byte LE prefix + JSON) is preserved from v2;
//! only the on-wire schema changes.

use serde::{Deserialize, Serialize};

use super::command::Command;
use super::event::Event;
use super::query::Query;
use super::reply::Reply;
use super::subscribe::Subscribe;

/// A v3 wire envelope.
///
/// The `v` field is the version discriminator; the server routes on it.
/// The `id` is a client-supplied correlation identifier (typically a ULID)
/// that echoes back on replies bound to this envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
  /// Protocol version. Always `3` for this module. Absence on ingress = v2.
  pub v: u8,
  /// Client-supplied correlation id (typically a ULID string).
  ///
  /// The daemon echoes this back on the reply so clients can multiplex
  /// several in-flight requests on one socket.
  pub id: String,
  /// The typed payload; the `kind` tag drives the receive-side switch.
  #[serde(flatten)]
  pub payload: Payload,
}

impl Envelope {
  /// Build a new envelope at the current protocol version.
  #[must_use]
  pub fn new(id: impl Into<String>, payload: Payload) -> Self {
    Self { v: super::V, id: id.into(), payload }
  }
}

/// The typed payload variants carried by an [`Envelope`].
///
/// Tagged externally on the wire via a `"kind"` string discriminator so a
/// human (or `jq`) can inspect the payload class without deserialising the
/// full body. Variant names mirror the CQRS surface described in the change
/// design: read-only [`Query`], mutating [`Command`], [`Subscribe`] to open a
/// stream, [`Reply`] on the request/response path, and broadcast [`Event`]s.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Payload {
  /// A read-only request. Returns fast without waiting for work; see
  /// [`Query`] and the `Ready|Pending|Missing|Degraded` shape in
  /// [`super::query::Availability`].
  Query(Query),
  /// A mutating intent. Returns a job id via [`super::command::CommandReply`];
  /// deduplicated by [`super::command::IdempotencyKey`].
  Command(Command),
  /// A request to open a subscription to one or more [`super::subscribe::Topics`].
  Subscribe(Subscribe),
  /// The daemon's reply to a preceding query, command, or subscribe.
  Reply(Reply),
  /// A broadcaster event delivered on a live subscription.
  Event(Event),
}
