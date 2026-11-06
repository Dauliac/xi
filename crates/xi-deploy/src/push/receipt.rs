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
use tracing::warn;
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
  /// `--notify <command>`, if the push that wrote this receipt was given
  /// one: run exactly once, by whichever process (this one, or a later
  /// `--reattach`) actually removes this receipt after observing a
  /// terminal outcome for `unit_name`.
  ///
  /// `#[serde(default)]` so a receipt written before this field existed
  /// still deserializes, as `None`.
  #[serde(default)]
  pub notify:     Option<String>,
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
      notify: None,
    }
  }

  /// Attach a `--notify` command to this receipt before it is written.
  #[must_use]
  pub fn with_notify(mut self, notify: Option<String>) -> Self {
    self.notify = notify;
    self
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

/// Remove the receipt for `push_id`, returning whether this call is the
/// one that actually deleted it.
///
/// An already-absent receipt is `Ok(false)`, not an error: the original
/// push process and a later `--reattach` can both observe the same
/// terminal outcome for a unit at nearly the same moment and race to
/// remove the same receipt, and `std::fs::remove_file` is the atomic
/// point that decides between them - exactly one caller gets `Ok(())`
/// (reported here as `true`), the other gets `NotFound` (`false`).
/// [`remove_and_notify`] is what turns that distinction into "only the
/// winner fires `--notify`".
///
/// # Errors
///
/// Propagates any I/O error other than `NotFound`.
pub fn remove(state_dir: &Path, push_id: &str) -> Result<bool> {
  let path = receipt_path(state_dir, push_id);
  match std::fs::remove_file(&path) {
    Ok(()) => Ok(true),
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
    Err(err) => Err(err.into()),
  }
}

/// Remove the receipt for `push_id` and, only if this call is the one
/// that actually deleted it, run `receipt.notify` (when set).
///
/// This is the single gate `--notify` fires through. A receipt is only
/// ever removed once this process (or a `--reattach`) is certain it has
/// observed a terminal outcome for the unit - never on an ambiguous
/// error (see `orchestrate::switch_with_receipt`) or an intermediate
/// still-running poll (see `reattach::run`) - so gating on [`remove`]'s
/// own atomicity, rather than re-deriving "did I just observe a terminal
/// state" at each call site, is what keeps two racing observers (the
/// original push reconnecting, and a concurrent `--reattach`) from both
/// running the command.
///
/// A notify command that fails to spawn or exits non-zero is logged, not
/// propagated: by the time this runs, the push or reattach it is
/// attached to has already succeeded or failed on its own terms, and a
/// broken `--notify` command should not turn that into an error.
///
/// # Errors
///
/// Propagates [`remove`]'s own I/O error.
pub fn remove_and_notify(
  state_dir: &Path,
  push_id: &str,
  receipt: &PushReceipt,
) -> Result<()> {
  if remove(state_dir, push_id)?
    && let Some(command) = &receipt.notify
  {
    run_notify(command);
  }
  Ok(())
}

/// Path of the "already notified" marker for `push_id` under `state_dir`.
///
/// Only ever created by [`mark_notified_and_notify`]: the confirmed-failure
/// path leaves the receipt itself in place for operator acknowledgment (see
/// that function's docs), so there is no `remove_file` call for `--notify`
/// to gate on the way [`remove_and_notify`] gates on the receipt's own
/// removal. This sibling file is the stand-in - its *creation* is the
/// atomic event instead.
#[must_use]
fn notified_marker_path(state_dir: &Path, push_id: &str) -> PathBuf {
  state_dir
    .join(RECEIPTS_DIR)
    .join(format!("{push_id}.notified"))
}

/// Run `receipt.notify` (when set) exactly once for a terminal outcome
/// that does *not* remove the receipt - a confirmed failure, where the
/// receipt stays on disk for the operator to acknowledge.
///
/// [`remove_and_notify`] gates a single fire on `fs::remove_file`'s own
/// atomicity: exactly one racing caller gets `Ok(())`. A failure never
/// removes the receipt, so there is nothing there to race on - this
/// function creates a sibling `<push_id>.notified` marker file with
/// `OpenOptions::create_new`, which carries the same `O_EXCL` guarantee:
/// exactly one of two racing callers (the original push reconnecting, and
/// a concurrent `--reattach`, or two concurrent `--reattach`s) wins the
/// create and fires `receipt.notify`; the other finds `AlreadyExists` and
/// does nothing.
///
/// # Errors
///
/// Propagates an I/O error other than `AlreadyExists` creating the
/// receipts directory or the marker file.
pub fn mark_notified_and_notify(
  state_dir: &Path,
  push_id: &str,
  receipt: &PushReceipt,
) -> Result<()> {
  std::fs::create_dir_all(state_dir.join(RECEIPTS_DIR))?;
  let path = notified_marker_path(state_dir, push_id);
  match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
    Ok(_) => {
      if let Some(command) = &receipt.notify {
        run_notify(command);
      }
      Ok(())
    },
    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
    Err(err) => Err(err.into()),
  }
}

/// Run `command` through the shell, logging rather than propagating a
/// spawn failure or a non-zero exit.
fn run_notify(command: &str) {
  match std::process::Command::new("sh").arg("-c").arg(command).status() {
    Ok(status) if status.success() => {},
    Ok(status) => {
      warn!(command, %status, "--notify command exited non-zero");
    },
    Err(error) => {
      warn!(command, %error, "failed to spawn --notify command");
    },
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
  use crate::push::engine::shell_quote;

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
  fn remove_deletes_an_existing_receipt_and_reports_it_removed() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "web1-1700000000-abc123", &receipt()).expect("writes");
    assert!(
      remove(tmp.path(), "web1-1700000000-abc123").expect("removes"),
      "the call that deletes the receipt must report true"
    );
    assert!(!receipt_path(tmp.path(), "web1-1700000000-abc123").exists());
  }

  #[test]
  fn removing_an_absent_receipt_is_not_an_error_and_reports_false() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(
      !remove(tmp.path(), "never-written").expect("absent receipt is fine"),
      "nothing was there to delete"
    );
  }

  #[test]
  fn removing_an_already_removed_receipt_reports_false() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "web1-1700000000-abc123", &receipt()).expect("writes");
    assert!(remove(tmp.path(), "web1-1700000000-abc123").expect("first remove"));
    assert!(
      !remove(tmp.path(), "web1-1700000000-abc123").expect("second remove"),
      "a second remove of the same push_id must not report true again - this \
       is the exact distinction remove_and_notify gates on"
    );
  }

  #[test]
  fn with_notify_attaches_a_command_to_the_receipt() {
    let receipt = receipt().with_notify(Some("echo done".to_owned()));
    assert_eq!(receipt.notify.as_deref(), Some("echo done"));
  }

  #[test]
  fn a_receipt_with_no_notify_round_trips_as_none() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "web1-1700000000-abc123", &receipt()).expect("writes");
    assert_eq!(
      read(tmp.path(), "web1-1700000000-abc123")
        .expect("read")
        .expect("some")
        .notify,
      None
    );
  }

  #[test]
  fn a_receipt_written_before_notify_existed_still_deserializes() {
    // A receipt on disk from before this field existed has no `notify`
    // key at all; `--reattach` against one of those (or this process
    // reading its own receipt back) must not fail to parse it.
    let tmp = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join(RECEIPTS_DIR)).expect("mkdir");
    std::fs::write(
      tmp.path().join(RECEIPTS_DIR).join("legacy.json"),
      r#"{"target":"web1","unit_name":"xi-push-web1-1",
          "started_at":"2024-01-01T00:00:00Z","scope":"system"}"#,
    )
    .expect("write legacy receipt");
    assert_eq!(
      read(tmp.path(), "legacy").expect("read").expect("some").notify,
      None
    );
  }

  #[test]
  fn remove_and_notify_only_runs_the_command_for_the_call_that_removes() {
    let tmp = TempDir::new().expect("tempdir");
    let marker = tmp.path().join("fired");
    let receipt = receipt()
      .with_notify(Some(format!("touch {}", shell_quote(&marker.display().to_string()))));
    write(tmp.path(), "web1-1700000000-abc123", &receipt).expect("writes");

    remove_and_notify(tmp.path(), "web1-1700000000-abc123", &receipt)
      .expect("first call removes and notifies");
    assert!(
      marker.exists(),
      "the call that actually removed the receipt must have run --notify"
    );

    std::fs::remove_file(&marker).expect("clear marker to detect a rerun");
    remove_and_notify(tmp.path(), "web1-1700000000-abc123", &receipt)
      .expect("second call finds the receipt already gone, which is not an error");
    assert!(
      !marker.exists(),
      "a call that finds the receipt already gone must not rerun --notify"
    );
  }

  #[test]
  fn remove_and_notify_does_nothing_when_no_notify_command_is_set() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "web1-1700000000-abc123", &receipt()).expect("writes");
    remove_and_notify(tmp.path(), "web1-1700000000-abc123", &receipt())
      .expect("removes cleanly with no --notify to run");
    assert!(!receipt_path(tmp.path(), "web1-1700000000-abc123").exists());
  }

  #[test]
  fn mark_notified_and_notify_fires_once_and_leaves_the_receipt_in_place() {
    let tmp = TempDir::new().expect("tempdir");
    let marker = tmp.path().join("fired");
    let receipt = receipt()
      .with_notify(Some(format!("touch {}", shell_quote(&marker.display().to_string()))));
    write(tmp.path(), "web1-1700000000-abc123", &receipt).expect("writes");

    mark_notified_and_notify(tmp.path(), "web1-1700000000-abc123", &receipt)
      .expect("first call fires notify");
    assert!(marker.exists(), "the winning call must have run --notify");
    assert!(
      receipt_path(tmp.path(), "web1-1700000000-abc123").exists(),
      "a confirmed-failure notify must not remove the receipt"
    );

    std::fs::remove_file(&marker).expect("clear marker to detect a rerun");
    mark_notified_and_notify(tmp.path(), "web1-1700000000-abc123", &receipt)
      .expect("second call finds the marker already there, which is not an error");
    assert!(
      !marker.exists(),
      "a second call must not rerun --notify for the same push_id"
    );
  }

  #[test]
  fn mark_notified_and_notify_does_nothing_when_no_notify_command_is_set() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "web1-1700000000-abc123", &receipt()).expect("writes");
    mark_notified_and_notify(tmp.path(), "web1-1700000000-abc123", &receipt())
      .expect("marks cleanly with no --notify to run");
  }

  #[test]
  fn mark_notified_and_notify_fires_exactly_once_under_a_concurrent_race() {
    let tmp = TempDir::new().expect("tempdir");
    let marker = tmp.path().join("fired");
    let fire_count = tmp.path().join("fire-count");
    std::fs::write(&fire_count, "").expect("seed counter file");
    // Each racing caller appends one byte on every invocation of the
    // notify command, so a double-fire is visible as a longer file even
    // though both racers target the same marker - this is the same
    // distinction `remove_and_notify`'s own race test draws for the
    // receipt-removal path, applied to the no-removal marker-file path.
    let receipt = receipt().with_notify(Some(format!(
      "printf x >> {} && touch {}",
      shell_quote(&fire_count.display().to_string()),
      shell_quote(&marker.display().to_string())
    )));
    write(tmp.path(), "web1-1700000000-abc123", &receipt).expect("writes");

    let state_dir = tmp.path().to_owned();
    let push_id = "web1-1700000000-abc123".to_owned();
    let receipt_a = receipt.clone();
    let receipt_b = receipt;
    let state_dir_a = state_dir.clone();
    let state_dir_b = state_dir;
    let push_id_a = push_id.clone();
    let push_id_b = push_id;

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let barrier_a = std::sync::Arc::clone(&barrier);
    let barrier_b = std::sync::Arc::clone(&barrier);

    let handle_a = std::thread::spawn(move || {
      barrier_a.wait();
      mark_notified_and_notify(&state_dir_a, &push_id_a, &receipt_a)
    });
    let handle_b = std::thread::spawn(move || {
      barrier_b.wait();
      mark_notified_and_notify(&state_dir_b, &push_id_b, &receipt_b)
    });

    handle_a.join().expect("thread a").expect("thread a notify");
    handle_b.join().expect("thread b").expect("thread b notify");

    assert_eq!(
      std::fs::read_to_string(&fire_count).expect("read counter"),
      "x",
      "--notify must have run exactly once across both racing callers"
    );
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
