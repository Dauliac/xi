//! Local push receipts: evidence that this process asked a target to
//! start a transient `systemd-run` unit, so a disconnect mid-push still
//! leaves a record of what is in flight.
//!
//! Part of the `xi-push-resilient-remote-build` proposal's "Named,
//! receipted unit" requirement
//! (`openspec/changes/xi-push-resilient-remote-build/proposal.md`):
//! before the SSH call runs, the operator side writes
//! `<state dir>/push-receipts/<push-id>.json`, and removes it only once
//! this process observes a terminal outcome (success or failure) for
//! that unit. A receipt left behind by a process that never got that
//! far is exactly the signal a later `--reattach` verb (a sibling task,
//! not implemented here) needs.

use std::path::{Path, PathBuf};

use color_eyre::Result;
use serde::{Deserialize, Serialize};
use xi_core::sources::{Scope, atomic::write_atomic, transaction::now_iso8601};

/// Directory under a scope's state dir where push receipts live.
pub const RECEIPTS_DIR: &str = "push-receipts";

/// What this process recorded about one in-flight remote unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushReceipt {
  /// The target's flake-attribute / inventory name.
  pub target:     String,
  /// The `systemd-run --unit` name started on `target`.
  pub unit_name:  String,
  /// When this process started the unit, ISO 8601 / RFC 3339 in UTC.
  pub started_at: String,
  /// The sources scope the push ran under.
  pub scope:      Scope,
}

impl PushReceipt {
  /// A new receipt, stamped now, for a unit this process is about to
  /// start.
  #[must_use]
  pub fn new(
    target: impl Into<String>,
    unit_name: impl Into<String>,
    scope: Scope,
  ) -> Self {
    Self {
      target: target.into(),
      unit_name: unit_name.into(),
      started_at: now_iso8601(),
      scope,
    }
  }
}

/// Path of the receipt file for `push_id` under `state_dir`.
#[must_use]
pub fn receipt_path(state_dir: &Path, push_id: &str) -> PathBuf {
  state_dir.join(RECEIPTS_DIR).join(format!("{push_id}.json"))
}

/// Write `receipt` to `<state_dir>/push-receipts/<push_id>.json`,
/// creating the receipts directory first if it does not exist yet.
///
/// # Errors
///
/// Propagates an I/O failure creating the receipts directory,
/// serializing `receipt`, or writing the file.
pub fn write(
  state_dir: &Path,
  push_id: &str,
  receipt: &PushReceipt,
) -> Result<()> {
  std::fs::create_dir_all(state_dir.join(RECEIPTS_DIR))?;
  let json = serde_json::to_string_pretty(receipt)?;
  write_atomic(
    &receipt_path(state_dir, push_id),
    json.as_bytes(),
    Some(0o600),
  )?;
  Ok(())
}

/// Read the receipt for `push_id`, or `None` if it has none.
///
/// # Errors
///
/// Propagates an I/O error other than `NotFound`, or a parse error for a
/// receipt file that exists but is not valid JSON.
pub fn read(state_dir: &Path, push_id: &str) -> Result<Option<PushReceipt>> {
  let path = receipt_path(state_dir, push_id);
  let raw = match std::fs::read_to_string(&path) {
    Ok(raw) => raw,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(err) => return Err(err.into()),
  };
  let receipt: PushReceipt = serde_json::from_str(&raw).map_err(|e| {
    color_eyre::eyre::eyre!("push receipt {} is not valid JSON: {e}", path.display())
  })?;
  Ok(Some(receipt))
}

/// Remove the receipt for `push_id`, treating an already-absent receipt
/// as success rather than an error.
///
/// # Errors
///
/// Propagates any I/O error other than `NotFound`.
pub fn remove(state_dir: &Path, push_id: &str) -> Result<()> {
  let path = receipt_path(state_dir, push_id);
  match std::fs::remove_file(&path) {
    Ok(()) => Ok(()),
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(err) => Err(err.into()),
  }
}

/// A receipt discovered on disk by [`list`], paired with the push id its
/// file name carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedReceipt {
  /// The push id the receipt's file name encodes
  /// (`<push_id>.json`).
  pub push_id: String,
  /// The receipt itself.
  pub receipt: PushReceipt,
}

/// List every receipt under `<state_dir>/push-receipts`, newest first.
///
/// An absent receipts directory is an empty list, not an error: no push
/// has ever written a receipt under `state_dir` yet. `started_at` sorts
/// correctly as a plain string because [`now_iso8601`] always produces a
/// fixed-width, zero-padded UTC timestamp.
///
/// # Errors
///
/// Propagates an I/O error reading the receipts directory or a receipt
/// file, or a parse error for a receipt file that exists but is not
/// valid JSON.
pub fn list(state_dir: &Path) -> Result<Vec<NamedReceipt>> {
  let dir = state_dir.join(RECEIPTS_DIR);
  let entries = match std::fs::read_dir(&dir) {
    Ok(entries) => entries,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
      return Ok(Vec::new());
    },
    Err(err) => return Err(err.into()),
  };

  let mut receipts = Vec::new();
  for entry in entries {
    let path = entry?.path();
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
      continue;
    }
    let Some(push_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
      continue;
    };
    let raw = std::fs::read_to_string(&path)?;
    let receipt: PushReceipt = serde_json::from_str(&raw).map_err(|e| {
      color_eyre::eyre::eyre!(
        "push receipt {} is not valid JSON: {e}",
        path.display()
      )
    })?;
    receipts.push(NamedReceipt {
      push_id: push_id.to_owned(),
      receipt,
    });
  }

  receipts.sort_by(|a, b| b.receipt.started_at.cmp(&a.receipt.started_at));
  Ok(receipts)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use tempfile::TempDir;

  use super::*;

  fn receipt() -> PushReceipt {
    PushReceipt::new("web1", "xi-push-web1-1700000000-abc123", Scope::System)
  }

  #[test]
  fn write_creates_the_receipts_dir_and_names_the_file_after_the_push_id() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "web1-1700000000-abc123", &receipt()).expect("writes");
    let path = tmp
      .path()
      .join("push-receipts")
      .join("web1-1700000000-abc123.json");
    assert!(path.exists(), "expected a receipt at {}", path.display());
  }

  #[test]
  fn a_written_receipt_round_trips_its_fields() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = receipt();
    write(tmp.path(), "web1-1700000000-abc123", &receipt).expect("writes");
    let raw = std::fs::read_to_string(receipt_path(
      tmp.path(),
      "web1-1700000000-abc123",
    ))
    .expect("read back");
    let read: PushReceipt = serde_json::from_str(&raw).expect("parses");
    assert_eq!(read, receipt);
  }

  #[test]
  fn read_returns_a_written_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = receipt();
    write(tmp.path(), "web1-1700000000-abc123", &receipt).expect("writes");
    assert_eq!(
      read(tmp.path(), "web1-1700000000-abc123").expect("read"),
      Some(receipt)
    );
  }

  #[test]
  fn read_returns_none_for_an_absent_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    assert_eq!(read(tmp.path(), "never-written").expect("read"), None);
  }

  #[test]
  fn remove_deletes_an_existing_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "web1-1700000000-abc123", &receipt()).expect("writes");
    remove(tmp.path(), "web1-1700000000-abc123").expect("removes");
    assert!(!receipt_path(tmp.path(), "web1-1700000000-abc123").exists());
  }

  #[test]
  fn removing_an_absent_receipt_is_not_an_error() {
    let tmp = TempDir::new().expect("tempdir");
    remove(tmp.path(), "never-written").expect("absent receipt is fine");
  }

  #[test]
  fn listing_with_no_receipts_dir_is_an_empty_list_not_an_error() {
    let tmp = TempDir::new().expect("tempdir");
    assert_eq!(list(tmp.path()).expect("list"), Vec::new());
  }

  #[test]
  fn listing_sorts_newest_first() {
    let tmp = TempDir::new().expect("tempdir");
    let mut older = receipt();
    older.started_at = "2024-01-01T00:00:00Z".to_owned();
    let mut newer = receipt();
    newer.started_at = "2024-06-01T00:00:00Z".to_owned();
    write(tmp.path(), "older", &older).expect("writes");
    write(tmp.path(), "newer", &newer).expect("writes");

    let listed = list(tmp.path()).expect("list");
    let ids: Vec<&str> =
      listed.iter().map(|named| named.push_id.as_str()).collect();
    assert_eq!(ids, vec!["newer", "older"]);
  }

  #[test]
  fn a_malformed_receipt_file_is_an_error() {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join(RECEIPTS_DIR)).expect("mkdir");
    std::fs::write(
      tmp.path().join(RECEIPTS_DIR).join("broken.json"),
      "{not json",
    )
    .expect("write junk");
    let err = list(tmp.path()).expect_err("must refuse");
    assert!(format!("{err}").contains("broken.json"), "got: {err}");
  }
}
