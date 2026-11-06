//! `xi push --reattach`: recover a remote-build push's outcome after an
//! operator-side disconnect.
//!
//! Only the no-id listing path lives here (`xi-enwe.2.1`): list every
//! local receipt under `<state_dir>/push-receipts`, newest first,
//! probing each receipt's target reachability with a plain connectivity
//! check. Reattaching to a specific unit - `systemctl show`, the
//! `journalctl` fallback for an already-`--collect`ed unit, and
//! `--notify` - is `xi-enwe.2.2`/`.2.3`, sibling tasks not implemented
//! here.

use std::path::Path;

use color_eyre::{Result, eyre::bail};

use super::{
  engine::{RemoteCommand, TargetAddr, Transport},
  receipt::{self, NamedReceipt, PushReceipt},
};

/// One row of the `--reattach` (no id) listing: a receipt plus whether
/// its target answered the reachability probe just now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptRow {
  /// The push id the receipt's file name encoded.
  pub push_id:   String,
  /// The receipt itself.
  pub receipt:   PushReceipt,
  /// Whether `receipt.target` answered a bare connectivity probe.
  pub reachable: bool,
}

/// Probe whether `target` is reachable at all, independent of the
/// `systemctl show`/`journalctl` query a specific reattach (`xi-enwe.2.2`)
/// will add.
///
/// Built straight off the receipt's stored target name rather than
/// through flake discovery: a receipt only ever recorded the bare
/// flake-attribute / inventory name
/// ([`PushReceipt::target`](super::receipt::PushReceipt::target)), and a
/// listing's connectivity check has no other discovery-derived detail
/// (`ssh_opts`, profile kind, ...) to need.
///
/// `Ok` from [`Transport::run`] with a failed [`Outcome`](super::engine::Outcome)
/// and an `Err` (the local `ssh` could not even be spawned) both read as
/// unreachable: the listing only needs "can I reach it right now", not
/// why not.
fn probe_reachable(transport: &dyn Transport, target: &str) -> bool {
  let addr = TargetAddr {
    name:     target.to_owned(),
    ssh_host: target.to_owned(),
    ssh_opts: Vec::new(),
  };
  transport
    .run(&addr, &RemoteCommand::probe("true"))
    .is_ok_and(|outcome| outcome.success)
}

/// List every local receipt under `state_dir`, newest first, probing
/// each one's target reachability.
///
/// # Errors
///
/// Propagates [`receipt::list`]'s error reading or parsing a receipt
/// file.
pub fn list(state_dir: &Path, transport: &dyn Transport) -> Result<Vec<ReceiptRow>> {
  Ok(
    receipt::list(state_dir)?
      .into_iter()
      .map(|NamedReceipt { push_id, receipt }| {
        let reachable = probe_reachable(transport, &receipt.target);
        ReceiptRow {
          push_id,
          receipt,
          reachable,
        }
      })
      .collect(),
  )
}

/// Render [`list`]'s rows the way an operator reads `xi push --reattach`
/// with no id.
#[must_use]
pub fn format_rows(rows: &[ReceiptRow]) -> String {
  if rows.is_empty() {
    return "no in-flight push receipts".to_owned();
  }
  rows
    .iter()
    .map(|row| {
      format!(
        "{}  target={}  unit={}  scope={}  started={}  {}",
        row.push_id,
        row.receipt.target,
        row.receipt.unit_name,
        row.receipt.scope,
        row.receipt.started_at,
        if row.reachable {
          "reachable"
        } else {
          "unreachable"
        },
      )
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Dispatch `xi push --reattach`.
///
/// With an empty `id` (`xi push --reattach` on its own), lists every
/// local receipt newest first. A non-empty `id` names a specific unit to
/// reattach to, which is `xi-enwe.2.2`'s `systemctl show` query, not
/// implemented yet.
///
/// # Errors
///
/// Propagates [`list`]'s error, or refuses outright for a non-empty
/// `id`.
pub fn run(id: &str, state_dir: &Path, transport: &dyn Transport) -> Result<()> {
  if !id.is_empty() {
    bail!(
      "xi push --reattach {id} is not implemented yet; only `xi push \
       --reattach` with no id (listing every local receipt) is available \
       today - reattaching to a specific push is xi-enwe.2.2"
    );
  }

  let rows = list(state_dir, transport)?;
  println!("{}", format_rows(&rows));
  Ok(())
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::sync::Mutex;

  use tempfile::TempDir;
  use xi_core::sources::Scope;

  use super::*;
  use crate::push::engine::Outcome;

  /// A [`Transport`] that answers every probe the same way, and records
  /// which targets it was asked about.
  #[derive(Default)]
  struct FixtureTransport {
    reachable: bool,
    asked:     Mutex<Vec<String>>,
  }

  impl Transport for FixtureTransport {
    fn run(&self, target: &TargetAddr, _cmd: &RemoteCommand) -> Result<Outcome> {
      self.asked.lock().expect("lock").push(target.name.clone());
      Ok(if self.reachable {
        Outcome::ok("")
      } else {
        Outcome::failed("unreachable")
      })
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: super::super::args::CopyVia,
    ) -> Result<()> {
      Ok(())
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
    ) -> Result<()> {
      Ok(())
    }

    fn sync_dir(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _remote: &str,
      _excludes: &[&str],
      _interactive_sudo: bool,
    ) -> Result<()> {
      Ok(())
    }
  }

  #[test]
  fn an_empty_receipts_dir_lists_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let transport = FixtureTransport {
      reachable: true,
      ..FixtureTransport::default()
    };
    let rows = list(tmp.path(), &transport).expect("list");
    assert!(rows.is_empty());
    assert_eq!(format_rows(&rows), "no in-flight push receipts");
  }

  #[test]
  fn listing_probes_every_receipts_target_and_reports_newest_first() {
    let tmp = TempDir::new().expect("tempdir");
    let mut older = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    older.started_at = "2024-01-01T00:00:00Z".to_owned();
    let mut newer = PushReceipt::new("web2", "xi-push-web2-1", Scope::System);
    newer.started_at = "2024-06-01T00:00:00Z".to_owned();
    receipt::write(tmp.path(), "web1-push", &older).expect("write");
    receipt::write(tmp.path(), "web2-push", &newer).expect("write");

    let transport = FixtureTransport {
      reachable: true,
      ..FixtureTransport::default()
    };
    let rows = list(tmp.path(), &transport).expect("list");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].push_id, "web2-push", "newest first");
    assert_eq!(rows[1].push_id, "web1-push");
    assert!(rows.iter().all(|row| row.reachable));
    assert_eq!(
      transport.asked.lock().expect("lock").clone(),
      vec!["web2".to_owned(), "web1".to_owned()],
      "every row's target must be probed"
    );
  }

  #[test]
  fn an_unreachable_target_is_reported_as_such() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");

    let transport = FixtureTransport {
      reachable: false,
      ..FixtureTransport::default()
    };
    let rows = list(tmp.path(), &transport).expect("list");

    assert_eq!(rows.len(), 1);
    assert!(!rows[0].reachable);
    assert!(format_rows(&rows).contains("unreachable"));
  }

  #[test]
  fn dispatching_with_a_non_empty_id_refuses() {
    let tmp = TempDir::new().expect("tempdir");
    let transport = FixtureTransport::default();
    let err = run("some-push-id", tmp.path(), &transport)
      .expect_err("not implemented yet");
    assert!(format!("{err}").contains("xi-enwe.2.2"), "got: {err}");
  }

  #[test]
  fn dispatching_with_an_empty_id_lists_without_erroring() {
    let tmp = TempDir::new().expect("tempdir");
    let transport = FixtureTransport {
      reachable: true,
      ..FixtureTransport::default()
    };
    run("", tmp.path(), &transport).expect("lists cleanly");
  }
}
