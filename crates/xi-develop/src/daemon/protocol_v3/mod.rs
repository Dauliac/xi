//! Daemon protocol v3 — CQRS-shaped wire types.
//!
//! Continues length-prefixed JSON (as v2). The envelope carries a `"v":3`
//! discriminator plus a payload tagged with `"kind"`
//! (`query` | `command` | `subscribe` | `reply` | `event`). Absence of `"v"`
//! signals v2. Server reads the first envelope and routes on `"v"`.
//!
//! This module ONLY defines wire types. Server dispatch for v3 lives in
//! `server.rs` (added by task 2.4). The v2 protocol at
//! `crate::daemon::protocol` remains untouched and coexists for one release
//! cycle (xi 4.5.x → xi 4.6.0).
//!
//! Text JSON keeps the wire debuggable via `socat`.
//!
//! Modules:
//! - [`envelope`] — top-level `Envelope { v, id, kind, payload }`.
//! - [`query`] — read-only requests + `QueryReply` / `Availability` shapes.
//! - [`command`] — mutating intents + `IdempotencyKey` stub + `CommandReply`.
//! - [`subscribe`] — subscription requests + `Topics` bitset.
//! - [`event`] — broadcaster events incl. `KeepAlive` and `Gap`.
//! - [`reply`] — top-level `Reply` sum returned on the request/response path.

pub mod command;
pub mod envelope;
pub mod event;
pub mod query;
pub mod reply;
pub mod subscribe;

pub use command::{Command, CommandReply, IdempotencyKey};
pub use envelope::{Envelope, Payload};
pub use event::{Event, EventPayload, Gap};
pub use query::{Availability, Query, QueryReply};
pub use reply::Reply;
pub use subscribe::{Subscribe, Topics};

/// The protocol version served by this module.
///
/// Every v3 envelope on the wire carries `"v": 3`. Absence of the field
/// signals a v2 client; the server routes accordingly.
pub const V: u8 = 3;
