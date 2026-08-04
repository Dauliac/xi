//! v3 commands — mutating intents.
//!
//! Every command returns a job id via [`CommandReply`]. Concurrent identical
//! commands (same [`IdempotencyKey`]) attach to the same in-flight job rather
//! than starting a new one; N clients issuing the same command against the
//! same lock state observe exactly one job with N attached requesters.
//!
//! The idempotency key incorporates the current `flake.lock` hash so a
//! re-issued command after a lock bump correctly allocates a fresh job and
//! never attaches to the pre-change one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::query::JobId;

/// ASCII Unit Separator byte used to delimit the three fields fed to blake3
/// (`kind`, `canonical_json(params)`, `flake_lock_hash`). Using a byte that
/// cannot appear at the boundary between canonical-JSON output and the
/// lock-hash hex string prevents cross-field collisions.
const FIELD_SEP: u8 = 0x1F;

/// A blake3-derived idempotency key (32 bytes).
///
/// Derivation (see [`IdempotencyKey::derive_for`]):
/// `blake3(command_kind || 0x1F || canonical_json(params) || 0x1F || flake_lock_hash)`
///
/// * `command_kind` — the serde tag of the [`Command`] variant, e.g.
///   `"eval_devshell"`, `"restart"`, `"abort_job"`, `"invalidate_cache"`.
/// * `0x1F` — one literal ASCII Unit Separator byte between fields.
/// * `canonical_json(params)` — a canonical (sorted-keys, no insignificant
///   whitespace) JSON serialization of every field of the variant *except*
///   the [`IdempotencyKey`] itself.
/// * `flake_lock_hash` — the lowercase hex string of `blake3(flake.lock
///   contents)`. Callers with no lock available pass `""` (the empty
///   string); the resulting key is stable across such lock-less callers.
///
/// Serialised as a lowercase hex string on the wire so it stays human-readable
/// under `socat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdempotencyKey(#[serde(with = "hex_bytes")] pub [u8; 32]);

impl IdempotencyKey {
  /// Placeholder constructor. Retained for callers still on the task-1.2
  /// wire shape while task-1.3 rolls out. New code should call
  /// [`IdempotencyKey::derive_for`] instead.
  #[must_use]
  pub const fn placeholder() -> Self {
    Self([0u8; 32])
  }

  /// Construct from an already-derived 32-byte digest.
  ///
  /// Intended for callers that compute blake3 externally (e.g. tests). Do not
  /// hand-craft keys in production — the daemon's dedup relies on the
  /// derivation being identical at every call site.
  #[must_use]
  pub const fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  /// Derive an [`IdempotencyKey`] for `cmd` at the given `flake_lock_hash`.
  ///
  /// See the type-level documentation for the full formula. The
  /// `flake_lock_hash` should be the lowercase hex of `blake3(flake.lock
  /// contents)`; callers with no lock available pass `""` (the empty string).
  ///
  /// Lock-independent commands (`Restart`, `AbortJob`) still incorporate the
  /// hash — daemon-side dedup treats them as lock-scoped as a conservative
  /// default; callers wanting cross-lock dedup can pass `""` explicitly.
  #[must_use]
  pub fn derive_for(cmd: &Command, flake_lock_hash: &str) -> Self {
    let kind = cmd.kind();
    let params_json = canonical_json_of_params(cmd);

    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_bytes());
    hasher.update(&[FIELD_SEP]);
    hasher.update(params_json.as_bytes());
    hasher.update(&[FIELD_SEP]);
    hasher.update(flake_lock_hash.as_bytes());

    Self(*hasher.finalize().as_bytes())
  }
}

/// A mutating command targeted at the daemon.
///
/// Every command returns an existing or newly-allocated [`JobId`] so the
/// client can attach via `Query::GetJob(id)` or subscribe against it. Design
/// § "authoritative job registry" and § "command deduplication and attach"
/// govern the daemon-side handling of these variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", content = "args", rename_all = "snake_case")]
pub enum Command {
  /// Evaluate the devshell for `target`.
  ///
  /// Dedup key covers `{target, flake_lock_hash}`; identical concurrent
  /// requests attach to the running eval job.
  EvalDevshell {
    /// The devshell attribute path (typically `"default"`).
    target: String,
    /// The client-derived idempotency key (see [`IdempotencyKey`]).
    key:    IdempotencyKey,
  },
  /// Restart the daemon (used to recover from `Stuck`).
  ///
  /// Aborts non-idempotent in-flight jobs, transitions through `SelfHealing`,
  /// and returns to `Ready`.
  Restart {
    /// The idempotency key.
    key: IdempotencyKey,
  },
  /// Abort a specific job.
  ///
  /// Used by clients when a query returned `Availability::Pending(job_id)` and
  /// the caller no longer wants that work (e.g. shell exited mid-eval).
  AbortJob {
    /// The job id to abort.
    job_id: JobId,
    /// The idempotency key.
    key:    IdempotencyKey,
  },
  /// Invalidate one or more daemon caches.
  ///
  /// Scope of invalidation is documented in later tasks; this variant is the
  /// wire hook.
  InvalidateCache {
    /// A free-form cache scope selector (task 2.x refines this).
    scope: String,
    /// The idempotency key.
    key:   IdempotencyKey,
  },
}

impl Command {
  /// The serde tag of this variant — the ASCII string mixed into the
  /// [`IdempotencyKey`] derivation. Kept in lockstep with the
  /// `#[serde(rename_all = "snake_case")]` on [`Command`].
  #[must_use]
  pub const fn kind(&self) -> &'static str {
    match *self {
      Self::EvalDevshell { .. } => "eval_devshell",
      Self::Restart { .. } => "restart",
      Self::AbortJob { .. } => "abort_job",
      Self::InvalidateCache { .. } => "invalidate_cache",
    }
  }
}

/// Serialise a command's parameter payload (everything except `key`) into
/// canonical JSON.
///
/// We deliberately do not `serde_json::to_string(cmd)` here — that would
/// include the `key` field, which is exactly what we're trying to derive, and
/// would also emit the tag/content wrapper. Instead we build a `Value` tree
/// per-variant and hand it to [`canonicalize`].
fn canonical_json_of_params(cmd: &Command) -> String {
  let mut params = Map::new();
  match cmd {
    Command::EvalDevshell { target, .. } => {
      params.insert("target".to_owned(), Value::String(target.clone()));
    }
    Command::Restart { .. } => {}
    Command::AbortJob { job_id, .. } => {
      // JobId serialises via its own serde impl; roundtrip through Value so
      // we depend on the same representation as the wire.
      let v = serde_json::to_value(job_id).unwrap_or(Value::Null);
      params.insert("job_id".to_owned(), v);
    }
    Command::InvalidateCache { scope, .. } => {
      params.insert("scope".to_owned(), Value::String(scope.clone()));
    }
  }
  canonicalize(&Value::Object(params))
}

/// Emit a canonical JSON string: object keys sorted lexicographically, no
/// insignificant whitespace.
///
/// `serde_json::Map` preserves insertion order by default, so this rewrite is
/// the only way to guarantee stable object-key ordering across callers.
/// Numbers are re-emitted through `serde_json::Number`'s `Display`, which does
/// not canonicalise floating-point representation — all values fed here
/// originate from wire-parseable command params (strings and integer job ids)
/// so float ambiguity does not currently apply. Revisit if a variant later
/// grows a `f64` param.
fn canonicalize(value: &Value) -> String {
  let mut out = String::new();
  write_canonical(value, &mut out);
  out
}

fn write_canonical(value: &Value, out: &mut String) {
  match value {
    Value::Null => out.push_str("null"),
    Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
    Value::Number(n) => out.push_str(&n.to_string()),
    Value::String(s) => {
      // Delegate string escaping to serde_json to stay compatible with the
      // wire representation.
      let escaped =
        serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\""));
      out.push_str(&escaped);
    }
    Value::Array(items) => {
      out.push('[');
      for (i, item) in items.iter().enumerate() {
        if i > 0 {
          out.push(',');
        }
        write_canonical(item, out);
      }
      out.push(']');
    }
    Value::Object(map) => {
      // Sort keys lexicographically via BTreeMap.
      let sorted: BTreeMap<&String, &Value> = map.iter().collect();
      out.push('{');
      for (i, (k, v)) in sorted.iter().enumerate() {
        if i > 0 {
          out.push(',');
        }
        let escaped = serde_json::to_string(k.as_str())
          .unwrap_or_else(|_| String::from("\"\""));
        out.push_str(&escaped);
        out.push(':');
        write_canonical(v, out);
      }
      out.push('}');
    }
  }
}

/// The reply body returned by the daemon for a preceding [`Command`].
///
/// Always carries a [`JobId`] — either an existing (attach) or newly-allocated
/// (fresh) job — so the client can observe the outcome without polling.
///
/// `attached = true` signals dedup: the requested command matched an
/// in-flight job by [`IdempotencyKey`] and the caller attached rather than
/// starting a new job. This is observable so multi-pane demos can be tested
/// (SC-004).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command_reply", rename_all = "snake_case")]
pub enum CommandReply {
  /// The command was accepted and a job id is available.
  Accepted {
    /// The existing or newly-allocated job id.
    job_id:   JobId,
    /// True when this request attached to an already-running job by
    /// matching [`IdempotencyKey`]; false when a fresh job was allocated.
    attached: bool,
  },
  /// The command was rejected before job allocation.
  ///
  /// Used for illegal state-transition proposals or malformed inputs. The
  /// state machine's rejection path is documented in the design § "Single-
  /// writer state machine".
  Rejected {
    /// A machine-parseable reason (task 2.4 defines the taxonomy).
    reason: String,
  },
}

/// Serde helper: serialise a `[u8; 32]` as a lowercase hex string.
mod hex_bytes {
  use serde::{Deserialize, Deserializer, Serializer};

  pub fn serialize<S>(bytes: &[u8; 32], ser: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut out = [0u8; 64];
    for (i, b) in bytes.iter().enumerate() {
      let hi = b >> 4;
      let lo = b & 0x0f;
      out[i * 2] = hex_nibble(hi);
      out[i * 2 + 1] = hex_nibble(lo);
    }
    // SAFETY: `hex_nibble` only emits ASCII bytes 0-9 and a-f.
    let s = std::str::from_utf8(&out).expect("hex is ASCII");
    ser.serialize_str(s)
  }

  pub fn deserialize<'de, D>(de: D) -> Result<[u8; 32], D::Error>
  where
    D: Deserializer<'de>,
  {
    let s = String::deserialize(de)?;
    if s.len() != 64 {
      return Err(serde::de::Error::custom(format!(
        "expected 64-hex-char idempotency key, got {} chars",
        s.len()
      )));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
      let hi = decode_nibble(chunk[0]).map_err(serde::de::Error::custom)?;
      let lo = decode_nibble(chunk[1]).map_err(serde::de::Error::custom)?;
      out[i] = (hi << 4) | lo;
    }
    Ok(out)
  }

  const fn hex_nibble(n: u8) -> u8 {
    match n {
      0..=9 => b'0' + n,
      10..=15 => b'a' + (n - 10),
      _ => b'0',
    }
  }

  fn decode_nibble(b: u8) -> Result<u8, &'static str> {
    match b {
      b'0'..=b'9' => Ok(b - b'0'),
      b'a'..=b'f' => Ok(b - b'a' + 10),
      b'A'..=b'F' => Ok(b - b'A' + 10),
      _ => Err("non-hex character in idempotency key"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const PLACEHOLDER: IdempotencyKey = IdempotencyKey::placeholder();

  fn eval(target: &str) -> Command {
    Command::EvalDevshell {
      target: target.to_owned(),
      key:    PLACEHOLDER,
    }
  }

  fn invalidate(scope: &str) -> Command {
    Command::InvalidateCache {
      scope: scope.to_owned(),
      key:   PLACEHOLDER,
    }
  }

  #[test]
  fn idempotency_key_hex_roundtrip() {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
      *b = i as u8;
    }
    let key = IdempotencyKey::from_bytes(bytes);
    let json = serde_json::to_string(&key).expect("serialize");
    // 64 hex chars + 2 quote chars.
    assert_eq!(json.len(), 66);
    let decoded: IdempotencyKey =
      serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded.0, bytes);
  }

  #[test]
  fn placeholder_is_zeroed() {
    assert_eq!(IdempotencyKey::placeholder().0, [0u8; 32]);
  }

  #[test]
  fn derive_is_deterministic_for_same_inputs() {
    let a = IdempotencyKey::derive_for(&eval("default"), "lockhash");
    let b = IdempotencyKey::derive_for(&eval("default"), "lockhash");
    assert_eq!(a, b);
  }

  #[test]
  fn derive_differs_for_different_params() {
    let a = IdempotencyKey::derive_for(&eval("default"), "lockhash");
    let b = IdempotencyKey::derive_for(&eval("dev"), "lockhash");
    assert_ne!(a, b);
  }

  #[test]
  fn derive_differs_for_different_kinds() {
    // Different kinds against the same lock hash and (deliberately) empty
    // scope must diverge purely on the kind tag mixed into blake3.
    let a = IdempotencyKey::derive_for(
      &Command::Restart { key: PLACEHOLDER },
      "lockhash",
    );
    let b = IdempotencyKey::derive_for(&invalidate(""), "lockhash");
    assert_ne!(a, b);
  }

  #[test]
  fn derive_differs_for_different_lock_hashes() {
    let a = IdempotencyKey::derive_for(&eval("default"), "lockhash-a");
    let b = IdempotencyKey::derive_for(&eval("default"), "lockhash-b");
    assert_ne!(a, b);
  }

  #[test]
  fn derive_key_field_is_ignored() {
    // Two commands with different `key` fields but otherwise identical
    // must produce the same derived key — the derivation ignores `key`.
    let a = Command::EvalDevshell {
      target: "default".to_owned(),
      key:    IdempotencyKey::placeholder(),
    };
    let b = Command::EvalDevshell {
      target: "default".to_owned(),
      key:    IdempotencyKey::from_bytes([0xAA; 32]),
    };
    assert_eq!(
      IdempotencyKey::derive_for(&a, "lockhash"),
      IdempotencyKey::derive_for(&b, "lockhash"),
    );
  }

  #[test]
  fn canonical_json_sorts_object_keys() {
    // Direct canonicalize check: keys inserted in reverse order still emit
    // in lexicographic order.
    let mut m = Map::new();
    m.insert("z".to_owned(), Value::String("last".to_owned()));
    m.insert("a".to_owned(), Value::String("first".to_owned()));
    let canonical = canonicalize(&Value::Object(m));
    assert_eq!(canonical, r#"{"a":"first","z":"last"}"#);
  }

  #[test]
  fn canonical_json_is_stable_under_key_reordering() {
    // End-to-end: two logically-identical objects with different key
    // insertion orders canonicalise to the same string.
    let mut a = Map::new();
    a.insert("scope".to_owned(), Value::String("evals".to_owned()));
    a.insert("extra".to_owned(), Value::String("x".to_owned()));

    let mut b = Map::new();
    b.insert("extra".to_owned(), Value::String("x".to_owned()));
    b.insert("scope".to_owned(), Value::String("evals".to_owned()));

    assert_eq!(
      canonicalize(&Value::Object(a)),
      canonicalize(&Value::Object(b))
    );
  }

  #[test]
  fn derive_flake_lock_empty_string_is_valid() {
    // Callers with no lock available pass `""`. This must produce a
    // deterministic key and must differ from the same command against a
    // non-empty hash.
    let no_lock = IdempotencyKey::derive_for(&eval("default"), "");
    let with_lock =
      IdempotencyKey::derive_for(&eval("default"), "somehash");
    assert_ne!(no_lock, with_lock);
    // Deterministic:
    assert_eq!(no_lock, IdempotencyKey::derive_for(&eval("default"), ""));
  }
}
