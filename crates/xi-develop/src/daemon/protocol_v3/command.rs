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

use serde::{Deserialize, Serialize};

use super::query::JobId;

/// A blake3-derived idempotency key (32 bytes).
///
/// **This is a stub.** Task xi-dyu.1.3 will fill in the derivation:
/// `blake3(kind || 0x1F || canonical_json(params) || 0x1F || flake_lock_hash)`,
/// truncated to 16 bytes for on-wire compactness. This module keeps the full
/// 32-byte digest surface so 1.3 can decide the truncation strategy without
/// re-shaping this type. The `blake3` crate dependency also belongs to 1.3.
///
/// Serialised as a lowercase hex string on the wire so it stays human-readable
/// under `socat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdempotencyKey(#[serde(with = "hex_bytes")] pub [u8; 32]);

impl IdempotencyKey {
  /// Placeholder constructor. **Task 1.3 will replace this with the real
  /// blake3 derivation** — do not depend on the current byte pattern.
  #[must_use]
  pub const fn placeholder() -> Self {
    Self([0u8; 32])
  }

  /// Construct from an already-derived 32-byte digest.
  ///
  /// Intended for callers in task 1.3 that compute blake3 externally. Do not
  /// hand-craft keys — the daemon's dedup relies on the derivation being the
  /// same at every call site.
  #[must_use]
  pub const fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
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
    ///
    /// Task 1.3 will derive this from `blake3(kind || params || flake.lock)`;
    /// until then the client passes [`IdempotencyKey::placeholder`].
    key: IdempotencyKey,
  },
  /// Restart the daemon (used to recover from `Stuck`).
  ///
  /// Aborts non-idempotent in-flight jobs, transitions through `SelfHealing`,
  /// and returns to `Ready`.
  Restart {
    /// The idempotency key. Lock-independent — task 1.3 must reflect that in
    /// the derivation for `Restart` and `AbortJob`.
    key: IdempotencyKey,
  },
  /// Abort a specific job.
  ///
  /// Used by clients when a query returned `Availability::Pending(job_id)` and
  /// the caller no longer wants that work (e.g. shell exited mid-eval).
  AbortJob {
    /// The job id to abort.
    job_id: JobId,
    /// The idempotency key. Lock-independent.
    key: IdempotencyKey,
  },
  /// Invalidate one or more daemon caches.
  ///
  /// Scope of invalidation is documented in later tasks; this variant is the
  /// wire hook.
  InvalidateCache {
    /// A free-form cache scope selector (task 2.x refines this).
    scope: String,
    /// The idempotency key.
    key: IdempotencyKey,
  },
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
    job_id: JobId,
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
}
