//! The reversal receipt, and the rule that keeps it honest after a crash.
//!
//! Part of the `xi-sources-crash-recovery` capability
//! (`openspec/specs/xi-sources-crash-recovery/spec.md`, requirement
//! "Receipt consistency rule").
//!
//! A receipt is the list of physical changes an actor made to the
//! machine, in the order it made them, so `xi system uninstall` can
//! reverse exactly those and nothing else. Two capabilities write
//! one: `xi-sources-bootstrap` (installing Nix, enabling flakes) and
//! `xi-sources-cache-prestage` (writing `xi.conf`, amending
//! `nix.conf`).
//!
//! **Seam.** Those two capabilities land in parallel and own the
//! *vocabulary* of receipt actions. This module owns only the two
//! things crash-recovery is responsible for:
//!
//! 1. the file format and the "append is the last step before commit" rule,
//!    enforced structurally by [`super::transaction::Transaction`] rather than
//!    by each actor remembering to do it in the right order;
//! 2. [`Receipt::reconstruct`], which lets `xi sources repair` add back an
//!    entry whose physical effect is visible on disk but whose write never
//!    happened, flagged so an operator can tell it apart from one the actor
//!    wrote itself.
//!
//! Bootstrap and cache-prestage supply the action names and the
//! on-disk evidence that proves an action happened; see
//! [`super::repair::ReceiptEvidence`].

use std::{io, path::Path};

use serde::{Deserialize, Serialize};

use super::{atomic::write_atomic, transaction::now_iso8601};

/// Filename of the receipt inside the state dir.
pub const RECEIPT_NAME: &str = "receipt.json";

/// Schema version of the receipt document.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// One reversible thing an actor did to the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptEntry {
  /// What was done, in the owning capability's vocabulary, e.g.
  /// `install-nix` or `write-xi-conf`.
  pub action:           String,
  /// When the actor recorded it, ISO 8601 / RFC 3339 in UTC.
  pub at:               String,
  /// Capability-specific payload the reversal needs, e.g. the path
  /// that was written or the installer that was used.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub detail:           Option<serde_json::Value>,
  /// True when `xi sources repair` inferred this entry from on-disk
  /// evidence rather than the actor having written it.
  ///
  /// Reversal honours a reconstructed entry exactly like a real one;
  /// the flag exists so an operator auditing the receipt can see which
  /// entries are inferences.
  #[serde(default, skip_serializing_if = "is_false")]
  pub reconstructed:    bool,
  /// When repair reconstructed it. Absent on actor-written entries.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub reconstructed_at: Option<String>,
}

impl ReceiptEntry {
  /// A new entry, stamped now, for an action that just succeeded.
  #[must_use]
  pub fn new(action: impl Into<String>) -> Self {
    Self {
      action:           action.into(),
      at:               now_iso8601(),
      detail:           None,
      reconstructed:    false,
      reconstructed_at: None,
    }
  }

  /// Attach the capability-specific payload the reversal needs.
  #[must_use]
  pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
    self.detail = Some(detail);
    self
  }

  /// Mark this entry as inferred by `xi sources repair`.
  #[must_use]
  pub fn reconstructed(mut self) -> Self {
    let stamp = now_iso8601();
    self.reconstructed = true;
    self.reconstructed_at = Some(stamp);
    self
  }
}

#[expect(
  clippy::trivially_copy_pass_by_ref,
  reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_false(flag: &bool) -> bool {
  !*flag
}

/// The whole receipt document for one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
  /// Document schema version; see [`RECEIPT_SCHEMA_VERSION`].
  pub schema_version: u32,
  /// Completed actions, oldest first. Reversal walks this backwards.
  pub entries:        Vec<ReceiptEntry>,
}

impl Default for Receipt {
  fn default() -> Self {
    Self {
      schema_version: RECEIPT_SCHEMA_VERSION,
      entries:        Vec::new(),
    }
  }
}

impl Receipt {
  /// Load the receipt for `state_dir`, treating absent as empty.
  ///
  /// "No receipt" and "a receipt with no entries" mean the same thing
  /// (nothing has been done that needs reversing), so a missing file is
  /// not an error. An *unparsable* file is: silently starting over
  /// would drop the record of real changes to the machine, which is the
  /// one failure mode a receipt exists to prevent.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors other than `NotFound`, and reports a
  /// malformed document as `InvalidData`.
  pub fn load(state_dir: &Path) -> io::Result<Self> {
    let path = state_dir.join(RECEIPT_NAME);
    let raw = match std::fs::read_to_string(&path) {
      Ok(raw) => raw,
      Err(err) if err.kind() == io::ErrorKind::NotFound => {
        return Ok(Self::default());
      },
      Err(err) => return Err(err),
    };
    if raw.trim().is_empty() {
      return Ok(Self::default());
    }
    serde_json::from_str(&raw).map_err(|err| {
      io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {err}", path.display()),
      )
    })
  }

  /// True when `action` is already recorded, reconstructed or not.
  #[must_use]
  pub fn records(&self, action: &str) -> bool {
    self.entries.iter().any(|entry| entry.action == action)
  }

  /// Replace the receipt on disk with this document.
  ///
  /// Routed through [`write_atomic`] like every other state-dir file,
  /// so a reader never sees a truncated receipt and a crash mid-write
  /// leaves the previous one intact.
  ///
  /// # Errors
  ///
  /// Propagates serialisation and I/O errors.
  pub fn store(&self, state_dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let body = serde_json::to_vec_pretty(self)?;
    write_atomic(&state_dir.join(RECEIPT_NAME), &body, Some(0o600))
  }

  /// Append `entries` to the receipt on disk.
  ///
  /// Callers inside a transaction must not use this directly: stage the
  /// entries with [`super::transaction::Transaction::stage_receipt`] so
  /// the append is structurally the last step before the sentinel is
  /// removed.
  ///
  /// # Errors
  ///
  /// Propagates the load and store errors.
  pub fn append(
    state_dir: &Path,
    entries: impl IntoIterator<Item = ReceiptEntry>,
  ) -> io::Result<()> {
    let mut receipt = Self::load(state_dir)?;
    receipt.entries.extend(entries);
    receipt.store(state_dir)
  }

  /// Add a reconstructed entry for `action`, if it is not recorded yet.
  ///
  /// Returns the entry that was added, or `None` when the receipt
  /// already names the action  -  which is what makes repair idempotent
  /// over receipts.
  ///
  /// # Errors
  ///
  /// Propagates the load and store errors.
  pub fn reconstruct(
    state_dir: &Path,
    action: &str,
    detail: Option<serde_json::Value>,
  ) -> io::Result<Option<ReceiptEntry>> {
    let mut receipt = Self::load(state_dir)?;
    if receipt.records(action) {
      return Ok(None);
    }
    let mut entry = ReceiptEntry::new(action).reconstructed();
    entry.detail = detail;
    receipt.entries.push(entry.clone());
    receipt.store(state_dir)?;
    Ok(Some(entry))
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn absent_receipt_loads_empty() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let receipt = Receipt::load(tmp.path()).expect("load");
    assert!(receipt.entries.is_empty());
  }

  #[test]
  fn malformed_receipt_is_an_error_not_a_fresh_start() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join(RECEIPT_NAME), "{not json")
      .expect("write junk");
    let err = Receipt::load(tmp.path()).expect_err("must refuse");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  fn reconstruct_is_a_no_op_when_already_recorded() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    Receipt::append(tmp.path(), [ReceiptEntry::new("install-nix")])
      .expect("append");

    let added =
      Receipt::reconstruct(tmp.path(), "install-nix", None).expect("second");
    assert!(added.is_none());
    assert_eq!(Receipt::load(tmp.path()).expect("load").entries.len(), 1);
  }

  #[test]
  fn reconstructed_entry_is_flagged_and_timestamped() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let entry = Receipt::reconstruct(tmp.path(), "install-nix", None)
      .expect("reconstruct")
      .expect("entry added");
    assert!(entry.reconstructed);
    assert!(entry.reconstructed_at.is_some());

    let json = serde_json::to_value(&entry).expect("encode");
    assert_eq!(json["reconstructed"], true);
    assert!(json["reconstructed_at"].is_string());
  }

  #[test]
  fn actor_written_entry_omits_the_reconstruction_fields() {
    let json =
      serde_json::to_value(ReceiptEntry::new("write-xi-conf")).expect("encode");
    assert!(json.get("reconstructed").is_none());
    assert!(json.get("reconstructed_at").is_none());
  }
}
