//! `xi push --reattach`: recover a remote-build push's outcome after an
//! operator-side disconnect.
//!
//! The no-id listing path (`xi-enwe.2.1`) lists every local receipt
//! under `<state_dir>/push-receipts`, newest first, probing each
//! receipt's target reachability with a plain connectivity check.
//!
//! The with-id path (`xi-enwe.2.2`) `SSH`s to that one receipt's target
//! and asks `systemctl show` for the named unit's state. The
//! `journalctl` fallback for an already-`--collect`ed unit, the
//! live-streaming case for a still-running unit, and `--notify` are
//! `xi-enwe.2.3`, a sibling task not implemented here.

use std::{collections::HashMap, path::Path};

use color_eyre::{
  Result,
  eyre::{bail, eyre},
};

use super::{
  engine::{RemoteCommand, TargetAddr, Transport, shell_quote},
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

/// What a `systemctl show` query against a receipted unit found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachOutcome {
  /// The unit is still active (or on its way there, or on its way out);
  /// nothing terminal to report yet. Streaming its journal live is
  /// `xi-enwe.2.3`.
  Running,
  /// The unit ran to completion with a zero exit.
  Succeeded,
  /// The unit ran to completion with a non-zero exit, or was killed.
  Failed {
    /// The raw `ExecMainStatus` - an exit code, or (when `detail` says
    /// the unit was killed/dumped/trapped) a signal number.
    exec_main_status: i64,
    /// `ActiveState`/`SubState`/`ExecMainCode`, for an operator-readable
    /// failure message.
    detail: String,
  },
  /// `systemctl show` has nothing live to report on this unit.
  ///
  /// This is read off `ExecMainCode` being unset rather than off a
  /// missing/empty reply: `systemctl show` answers an unknown unit name
  /// with all-default properties instead of erroring, and a unit whose
  /// main process genuinely ran (to success or failure) always leaves
  /// `ExecMainCode` set - so "unset" here only ever means `--collect`
  /// already unloaded it. Falling back to `journalctl -u <unit_name>` is
  /// `xi-enwe.2.3`, not implemented here.
  Collected,
}

/// `ExecMainCode`'s `waitid(2)` `si_code`, as `systemctl show` reports
/// it: `0` when systemd has never recorded an exit for the unit's main
/// process, `1` (`CLD_EXITED`) when it has exited normally.
const EXEC_MAIN_CODE_UNSET: i64 = 0;
const EXEC_MAIN_CODE_EXITED: i64 = 1;

/// `ActiveState` values that mean "not done yet" - including
/// `deactivating`, since a unit mid-shutdown still has nothing terminal
/// to report.
fn is_unsettled(active_state: &str) -> bool {
  matches!(
    active_state,
    "activating" | "active" | "reloading" | "deactivating"
  )
}

/// Parse the `KEY=VALUE` lines `systemctl show --property=...` emits.
fn parse_show_output(stdout: &str) -> HashMap<&str, &str> {
  stdout.lines().filter_map(|line| line.split_once('=')).collect()
}

/// Look up `key` in `systemctl show`'s parsed output, or error naming
/// which property was missing.
fn field<'a>(
  fields: &HashMap<&'a str, &'a str>,
  unit_name: &str,
  key: &'static str,
) -> Result<&'a str> {
  fields
    .get(key)
    .copied()
    .ok_or_else(|| eyre!("systemctl show {unit_name} did not report {key}"))
}

/// Ask `target` for `unit_name`'s `systemctl show` properties and map
/// the answer to a [`ReattachOutcome`].
///
/// # Errors
///
/// Returns an error if the local `ssh` could not be spawned, if
/// `systemctl show` itself exited non-zero, or if its output is missing
/// one of the four requested properties or reports a non-numeric
/// `ExecMainStatus`/`ExecMainCode`.
fn query_unit(
  transport: &dyn Transport,
  target: &TargetAddr,
  unit_name: &str,
) -> Result<ReattachOutcome> {
  let outcome = transport.run(
    target,
    &RemoteCommand::probe(format!(
      "systemctl show {} \
       --property=ActiveState,SubState,ExecMainStatus,ExecMainCode",
      shell_quote(unit_name)
    )),
  )?;
  if !outcome.success {
    bail!(
      "systemctl show {unit_name} failed on {}: {}",
      target.name,
      outcome.message()
    );
  }

  let fields = parse_show_output(&outcome.stdout);
  let active_state = field(&fields, unit_name, "ActiveState")?;
  if is_unsettled(active_state) {
    return Ok(ReattachOutcome::Running);
  }

  let exec_main_code = field(&fields, unit_name, "ExecMainCode")?
    .parse::<i64>()
    .map_err(|e| eyre!("{unit_name}'s ExecMainCode is not a number: {e}"))?;
  if exec_main_code == EXEC_MAIN_CODE_UNSET {
    return Ok(ReattachOutcome::Collected);
  }

  let exec_main_status = field(&fields, unit_name, "ExecMainStatus")?
    .parse::<i64>()
    .map_err(|e| eyre!("{unit_name}'s ExecMainStatus is not a number: {e}"))?;
  if exec_main_code == EXEC_MAIN_CODE_EXITED && exec_main_status == 0 {
    return Ok(ReattachOutcome::Succeeded);
  }

  let sub_state = field(&fields, unit_name, "SubState")?;
  Ok(ReattachOutcome::Failed {
    exec_main_status,
    detail: format!(
      "ActiveState={active_state} SubState={sub_state} \
       ExecMainCode={exec_main_code}"
    ),
  })
}

/// Dispatch `xi push --reattach`.
///
/// With an empty `id` (`xi push --reattach` on its own), lists every
/// local receipt newest first. With an id, `SSH`s to that receipt's
/// target and asks `systemctl show` the named unit's state.
///
/// # Errors
///
/// Propagates [`list`]'s error for an empty `id`. For a non-empty `id`,
/// errors if no receipt exists for it, if the `systemctl show` query
/// itself fails (see [`query_unit`]), or if the unit is found to have
/// failed - a failed reattach deliberately exits non-zero so the
/// operator has to acknowledge it rather than it passing silently.
pub fn run(id: &str, state_dir: &Path, transport: &dyn Transport) -> Result<()> {
  if id.is_empty() {
    let rows = list(state_dir, transport)?;
    println!("{}", format_rows(&rows));
    return Ok(());
  }

  let Some(receipt) = receipt::read(state_dir, id)? else {
    bail!("no push receipt found for {id}");
  };

  let addr = TargetAddr {
    name:     receipt.target.clone(),
    ssh_host: receipt.target.clone(),
    ssh_opts: Vec::new(),
  };

  match query_unit(transport, &addr, &receipt.unit_name)? {
    ReattachOutcome::Running => {
      println!(
        "{} is still running on {}",
        receipt.unit_name, receipt.target
      );
      Ok(())
    },
    ReattachOutcome::Succeeded => {
      println!("{} succeeded on {}", receipt.unit_name, receipt.target);
      receipt::remove(state_dir, id)
    },
    ReattachOutcome::Failed {
      exec_main_status,
      detail,
    } => {
      bail!(
        "{} failed on {} (ExecMainStatus={exec_main_status}): {detail}",
        receipt.unit_name,
        receipt.target,
      );
    },
    ReattachOutcome::Collected => {
      println!(
        "{} is no longer tracked by systemd on {} - it was already \
         `--collect`ed; a journalctl fallback by unit name is \
         xi-enwe.2.3, not implemented yet",
        receipt.unit_name, receipt.target
      );
      Ok(())
    },
  }
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
  fn dispatching_with_an_unknown_id_errors() {
    let tmp = TempDir::new().expect("tempdir");
    let transport = FixtureTransport::default();
    let err = run("some-push-id", tmp.path(), &transport)
      .expect_err("no receipt for this id");
    assert!(format!("{err}").contains("some-push-id"), "got: {err}");
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

  /// A [`Transport`] that answers every `run` call with one scripted
  /// `systemctl show`-shaped [`Outcome`], for the single-call scenarios
  /// `query_unit`/`run`'s with-id path exercises.
  struct ScriptedTransport(Outcome);

  impl Transport for ScriptedTransport {
    fn run(&self, _target: &TargetAddr, _cmd: &RemoteCommand) -> Result<Outcome> {
      Ok(self.0.clone())
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

  fn addr() -> TargetAddr {
    TargetAddr {
      name:     "web1".to_owned(),
      ssh_host: "web1".to_owned(),
      ssh_opts: Vec::new(),
    }
  }

  #[test]
  fn a_still_active_unit_is_running() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=active\nSubState=running\nExecMainStatus=0\n\
       ExecMainCode=0\n",
    ));
    assert_eq!(
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query"),
      ReattachOutcome::Running
    );
  }

  #[test]
  fn a_deactivating_unit_is_running() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=deactivating\nSubState=stop-sigterm\n\
       ExecMainStatus=0\nExecMainCode=0\n",
    ));
    assert_eq!(
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query"),
      ReattachOutcome::Running
    );
  }

  #[test]
  fn a_zero_exit_unit_succeeded() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\n\
       ExecMainCode=1\n",
    ));
    assert_eq!(
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query"),
      ReattachOutcome::Succeeded
    );
  }

  #[test]
  fn a_non_zero_exit_unit_failed() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=1\n\
       ExecMainCode=1\n",
    ));
    let outcome =
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query");
    assert_eq!(
      outcome,
      ReattachOutcome::Failed {
        exec_main_status: 1,
        detail: "ActiveState=failed SubState=failed ExecMainCode=1"
          .to_owned(),
      }
    );
  }

  #[test]
  fn a_killed_unit_failed() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=9\n\
       ExecMainCode=2\n",
    ));
    let outcome =
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query");
    assert_eq!(
      outcome,
      ReattachOutcome::Failed {
        exec_main_status: 9,
        detail: "ActiveState=failed SubState=failed ExecMainCode=2"
          .to_owned(),
      }
    );
  }

  #[test]
  fn a_never_recorded_main_process_reads_as_collected() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\n\
       ExecMainCode=0\n",
    ));
    assert_eq!(
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query"),
      ReattachOutcome::Collected
    );
  }

  #[test]
  fn a_non_zero_exit_systemctl_show_errors() {
    let transport = ScriptedTransport(Outcome::failed("permission denied"));
    let err = query_unit(&transport, &addr(), "xi-push-web1-1")
      .expect_err("ssh/systemctl failure must surface");
    assert!(format!("{err}").contains("permission denied"), "got: {err}");
  }

  #[test]
  fn reattaching_to_a_running_unit_leaves_the_receipt_and_does_not_error() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=active\nSubState=running\nExecMainStatus=0\n\
       ExecMainCode=0\n",
    ));

    run("web1-push", tmp.path(), &transport).expect("still running is not an error");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "a still-running unit's receipt must survive"
    );
  }

  #[test]
  fn reattaching_to_a_succeeded_unit_removes_the_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\n\
       ExecMainCode=1\n",
    ));

    run("web1-push", tmp.path(), &transport).expect("success is not an error");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_none(),
      "a succeeded unit's receipt must be removed"
    );
  }

  #[test]
  fn reattaching_to_a_failed_unit_errors_and_keeps_the_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=1\n\
       ExecMainCode=1\n",
    ));

    let err = run("web1-push", tmp.path(), &transport)
      .expect_err("a failed unit must not pass silently");
    assert!(format!("{err}").contains("ExecMainStatus=1"), "got: {err}");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "an operator must still acknowledge a failure before its receipt goes away"
    );
  }

  #[test]
  fn reattaching_to_a_collected_unit_reports_it_without_erroring() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\n\
       ExecMainCode=0\n",
    ));

    run("web1-push", tmp.path(), &transport)
      .expect("a collected unit is deferred work, not an error");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "xi-enwe.2.3's journalctl fallback still needs this receipt"
    );
  }
}
