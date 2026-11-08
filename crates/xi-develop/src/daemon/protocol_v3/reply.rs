//! v3 replies — top-level reply sum type.
//!
//! The daemon returns [`Reply`] variants on the request/response path (queries,
//! commands, subscribe acks). Broadcaster events use
//! [`super::event::Event`] and are not wrapped in [`Reply`].
//!
//! The `id` correlation field on the outer [`super::envelope::Envelope`] lets
//! the client match a reply to the request it sent.

use serde::{Deserialize, Serialize};

use super::{command::CommandReply, query::QueryReply};

/// Classification of a [`super::query::Query::ExprEval`] result.
///
/// The handler never hands back raw `nix eval --json` output - it always
/// classifies first, so callers (notably `xi-repl`'s `:raw` escape) get an
/// explicit, named shape instead of having to sniff JSON or risk a silent
/// hang on something unrepresentable (a lambda, an unforced thunk, a
/// genuine eval failure).
///
/// - `Attrs` only appears for a *shallow* query: attribute names obtained via
///   `builtins.attrNames`, which forces the attrset's own WHNF but never forces
///   (or serializes) any of its values.
/// - `Derivation` appears for either query shape: `name`/`version`/
///   `description` are plain attrs on the `mkDerivation` convention, cheap to
///   read without realizing any build output.
/// - `Scalar` only appears for a *non-shallow* query: the full `nix eval
///   --json` value (string, number, bool, list, or a plain attrset with no
///   `type = "derivation"` marker).
/// - `Lambda` appears for either query shape: `builtins.isFunction` only needs
///   WHNF, so it's cheap even when shallow.
/// - `Thunk` only appears for a *shallow* query, as the fallback for every
///   value that is neither a function nor a (non-derivation) attrset - scalars,
///   lists, paths, etc. A shallow query deliberately declines to serialize
///   those (serialization would force them, deeply, same as a full query) and
///   reports back "this exists but wasn't materialized" instead.
/// - `Error` covers genuine failures: parse errors, undefined names, missing
///   attrs, or a `nix eval` that exited non-zero for any other reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "expr_eval_result", content = "value", rename_all = "snake_case")]
pub enum ExprEvalResult {
  /// Shallow result: attribute names only. Never forces any attribute's
  /// value.
  Attrs(Vec<String>),
  /// The expression evaluated to a derivation.
  Derivation {
    /// `v.name`, if present.
    name:        Option<String>,
    /// `v.version`, if present.
    version:     Option<String>,
    /// `v.meta.description`, if present.
    description: Option<String>,
  },
  /// The expression evaluated to a JSON-representable scalar or structure
  /// (non-shallow queries only).
  Scalar(serde_json::Value),
  /// The expression evaluated to a function.
  Lambda,
  /// Shallow query: the value exists and was forced to WHNF but is not a
  /// function, derivation, or attrset - its contents were deliberately not
  /// serialized.
  Thunk,
  /// Non-shallow query only: the value forced fine but contains a function
  /// somewhere inside it (a list or attrset element, not the top-level
  /// value itself - that case is [`ExprEvalResult::Lambda`]) and so cannot
  /// be JSON-serialized - e.g. a flake-parts/NixOS module's
  /// `imports = [ { ... }: {...} ]`. `nix`'s own `--json` output flag
  /// crashes the whole `nix eval` process on this rather than raising a
  /// catchable evaluation error, so `v3_handlers::classify_wrapper` checks
  /// for it ahead of time and reports this distinct, nameable shape
  /// instead of a generic [`ExprEvalResult::Error`].
  Unrepresentable,
  /// The daemon could not evaluate the expression.
  Error {
    /// Human-readable message (a trimmed `nix eval` stderr summary).
    message: String,
  },
}

/// The result of a [`super::query::Query::ExprDoc`] lookup.
///
/// Mirrors real `nix repl`'s `:doc <expr>` three outcomes: documentation
/// text, the explicit "this value has none" case (kept distinct from
/// `Error` so callers can render it as an unremarkable "no docs" state
/// rather than a failure), and a genuine evaluation error (undefined name,
/// syntax error, or the `nix repl` subprocess itself failing).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocResult {
  /// Documentation text was found - either a primop's synopsis+description,
  /// or a non-primop's "defined at `FILE:LINE`" plus any preceding doc
  /// comment, verbatim as `nix repl`'s `:doc` printed it.
  Found {
    /// The doc text, trimmed of leading/trailing blank lines.
    doc: String,
  },
  /// The expression evaluated fine but has no documentation attached -
  /// `nix repl`'s own "value does not have documentation" case.
  NotFound,
  /// The expression itself failed to evaluate, or the `nix repl`
  /// subprocess could not be run at all.
  Error {
    /// Human-readable message.
    message: String,
  },
}

/// A top-level reply carried inside an [`super::envelope::Envelope`] on the
/// request/response path.
///
/// Variants:
/// - `Query(QueryReply)` — reply to a preceding [`super::query::Query`];
///   value-bearing replies wrap the value in `Availability`.
/// - `Command(CommandReply)` — reply to a preceding
///   [`super::command::Command`]; always carries a job id on `Accepted`.
/// - `SubscribeAck` — acknowledgement that a subscription has been established.
///   Subsequent messages on the same envelope stream are
///   [`super::event::Event`]s, not more replies.
/// - `Error { message }` — a generic transport-level error (malformed envelope,
///   unknown query, etc). Not used for domain rejections — commands surface
///   those via [`CommandReply::Rejected`].
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
