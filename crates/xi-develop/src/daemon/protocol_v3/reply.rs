//! v3 replies — top-level reply sum type.
//!
//! The daemon returns [`Reply`] variants on the request/response path (queries,
//! commands, subscribe acks). Broadcaster events use
//! [`super::event::Event`] and are not wrapped in [`Reply`].
//!
//! The `id` correlation field on the outer [`super::envelope::Envelope`] lets
//! the client match a reply to the request it sent.

use serde::{Deserialize, Serialize};

use super::command::CommandReply;
use super::query::QueryReply;

/// A top-level reply carried inside an [`super::envelope::Envelope`] on the
/// request/response path.
///
/// Variants:
/// - `Query(QueryReply)` — reply to a preceding [`super::query::Query`];
///   value-bearing replies wrap the value in `Availability`.
/// - `Command(CommandReply)` — reply to a preceding [`super::command::Command`];
///   always carries a job id on `Accepted`.
/// - `SubscribeAck` — acknowledgement that a subscription has been established.
///   Subsequent messages on the same envelope stream are [`super::event::Event`]s,
///   not more replies.
/// - `Error { message }` — a generic transport-level error (malformed
///   envelope, unknown query, etc). Not used for domain rejections — commands
///   surface those via [`CommandReply::Rejected`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", content = "body", rename_all = "snake_case")]
pub enum Reply {
  /// Reply to a preceding query.
  Query(QueryReply),
  /// Reply to a preceding command.
  Command(CommandReply),
  /// Acknowledgement of a subscription request. The socket then streams events.
  SubscribeAck {
    /// The snapshot sequence number the subscription starts from.
    ///
    /// If the client requested a `resume_from` older than what the daemon can
    /// serve, the ack still returns the current sequence and the first event
    /// is a `Gap` marker.
    from_seq: super::query::SnapshotSeq,
  },
  /// A transport-level or unrecognised-request error.
  ///
  /// Domain-level command rejections use [`CommandReply::Rejected`] instead;
  /// this variant covers malformed envelopes, unknown query variants, and
  /// version-negotiation failures.
  Error {
    /// Human-readable error message. Structured taxonomy is task 2.x.
    message: String,
  },
}
