//! `xi push --reattach`: recover a remote-build push's outcome after an
//! operator-side disconnect.
//!
//! The no-id listing path (`xi-enwe.2.1`) lists every local receipt
//! under `<state_dir>/push-receipts`, newest first, probing each
//! receipt's target reachability with a plain connectivity check.
//!
//! The with-id path (`xi-enwe.2.2`) `SSH`s to that one receipt's target
//! and asks `systemctl show` for the named unit's state.
//!
//! `xi-enwe.2.3` fills in what happens next: a [`ReattachOutcome::Collected`]
//! (`--collect` already unloaded the unit) falls back to parsing
//! `journalctl`'s terminal status line for the unit, and a
//! [`ReattachOutcome::Running`] unit is followed by streaming its journal
//! live until that same terminal line goes by or the operator interrupts.
//!
//! `xi-enwe.3.1` adds `--notify`: every path below that resolves to a
//! confirmed success removes the receipt through
//! [`receipt::remove_and_notify`] rather than [`receipt::remove`], so a
//! reattach that races the original push process to the same terminal
//! observation only fires the notify command once, from whichever side
//! actually wins the receipt's removal.
//!
//! `xi-enwe.3.2` closes the gap that left: a confirmed *failure* is a
//! terminal observation too (the spec's `--notify` requirement says
//! "exactly once per terminal observation", not "per success"), but the
//! failure path deliberately keeps the receipt in place for operator
//! acknowledgment, so there is no `remove_file` race to gate a single
//! fire on. [`notify_failure`] gates on [`receipt::mark_notified_and_notify`]
//! instead, which creates a sibling `<push_id>.notified` marker file with
//! the same `O_EXCL` atomicity `remove_file` gave the success path.

use std::{collections::HashMap, path::Path};

use color_eyre::{
  Result,
  eyre::{bail, eyre},
};
use tracing::warn;

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
/// `Ok` from [`Transport::run`] with a failed
/// [`Outcome`](super::engine::Outcome) and an `Err` (the local `ssh` could not
/// even be spawned) both read as unreachable: the listing only needs "can I
/// reach it right now", not why not.
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
pub fn list(
  state_dir: &Path,
  transport: &dyn Transport,
) -> Result<Vec<ReceiptRow>> {
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
  /// nothing terminal to report yet. `run` follows this with a live
  /// `journalctl -f` stream.
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
    detail:           String,
  },
  /// `systemctl show` has nothing live to report on this unit.
  ///
  /// This is read off `ExecMainCode` being unset rather than off a
  /// missing/empty reply: `systemctl show` answers an unknown unit name
  /// with all-default properties instead of erroring, and a unit whose
  /// main process genuinely ran (to success or failure) always leaves
  /// `ExecMainCode` set - so "unset" here only ever means `--collect`
  /// already unloaded it. `run` falls back to `journalctl -u <unit_name>`
  /// for this case.
  Collected,
}

/// What `journalctl -u <unit_name>`'s own `systemd[1]:` status lines say
/// about how the unit ended, recovered once `--collect` has already
/// unloaded it from `systemctl show`'s live state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JournalOutcome {
  /// systemd logged `<unit>: Succeeded.`
  Succeeded,
  /// systemd logged `<unit>: Failed with result '<result>'.`, carrying
  /// that `<result>` (e.g. `exit-code`, `signal`, `timeout`).
  Failed {
    /// The quoted result systemd gave for the failure.
    result: String,
  },
}

/// The literal marker systemd logs on a unit's successful exit.
const JOURNAL_SUCCEEDED_MARKER: &str = ": Succeeded.";
/// The literal marker prefix systemd logs on a unit's failed exit, just
/// before the single-quoted result.
const JOURNAL_FAILED_MARKER: &str = ": Failed with result '";

/// Scan `stdout` (a `journalctl -u <unit_name>` capture) backwards for
/// the unit's final status line.
///
/// Backwards because the log is chronological and only the *last*
/// `Succeeded.`/`Failed with result` line is the terminal one - an
/// in-progress unit's log never has one at all, which is how `--since`
/// window bugs (an empty or truncated capture) are told apart from a
/// genuinely-not-terminal-yet unit by returning `None` either way.
///
/// `journalctl -u <unit_name>` already filters to this unit's own
/// entries, so a bare marker match cannot belong to another unit.
fn parse_journal_status(stdout: &str) -> Option<JournalOutcome> {
  stdout.lines().rev().find_map(|line| {
    if line.contains(JOURNAL_SUCCEEDED_MARKER) {
      return Some(JournalOutcome::Succeeded);
    }
    let idx = line.find(JOURNAL_FAILED_MARKER)?;
    let rest = &line[idx + JOURNAL_FAILED_MARKER.len()..];
    let result = rest.split('\'').next().unwrap_or("unknown").to_owned();
    Some(JournalOutcome::Failed { result })
  })
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
  stdout
    .lines()
    .filter_map(|line| line.split_once('='))
    .collect()
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

/// Ask `target` for `unit_name`'s journal since `started_at` and parse
/// its terminal status line.
///
/// The only way to learn a `--collect`ed unit's outcome: `systemctl show`
/// has nothing live left for it, but journald retains unit-indexed
/// history independent of whether the unit is still loaded.
///
/// # Errors
///
/// Returns an error if the local `ssh` could not be spawned, if
/// `journalctl` itself exited non-zero, or if its output has no
/// `Succeeded.`/`Failed with result` line for `unit_name` since
/// `started_at`.
fn query_journal(
  transport: &dyn Transport,
  target: &TargetAddr,
  unit_name: &str,
  started_at: &str,
) -> Result<JournalOutcome> {
  let outcome = transport.run(
    target,
    &RemoteCommand::probe(format!(
      "journalctl -u {} --since {} --no-pager",
      shell_quote(unit_name),
      shell_quote(started_at)
    )),
  )?;
  if !outcome.success {
    bail!(
      "journalctl -u {unit_name} failed on {}: {}",
      target.name,
      outcome.message()
    );
  }

  parse_journal_status(&outcome.stdout).ok_or_else(|| {
    eyre!(
      "journalctl -u {unit_name} since {started_at} on {} has no terminal \
       status line yet",
      target.name
    )
  })
}

/// Stream `unit_name`'s journal live on `target`, echoing every line to
/// this process's own stdout, until a terminal status line goes by or
/// the operator interrupts (killing the local `ssh`, which ends this
/// call without returning).
///
/// Plain `journalctl -f` never exits on its own, so the remote script
/// itself watches each line for the same terminal markers
/// [`parse_journal_status`] looks for and exits `0`/`1` the moment one
/// goes by - that exit status is this function's return value, since a
/// streaming [`Outcome`](super::engine::Outcome) carries no captured
/// stdout to parse after the fact.
///
/// Piping `journalctl -f` straight into the reading `while` loop (an
/// earlier version of this script did exactly that) does not work:
/// exiting the loop only closes its end of the pipe, and since the unit
/// is already `--collect`-ed, no further journal entries ever arrive
/// for `journalctl -f` to notice the closed pipe on - it blocks in
/// `poll()` forever and never receives `SIGPIPE`. Instead, `journalctl
/// -f` is started in the background writing into a named pipe (a plain
/// anonymous pipe would not let this script recover its PID after the
/// fact the way `$!` does right after backgrounding it), and the
/// moment the loop sees a terminal line it `kill`s that PID explicitly
/// before exiting, rather than relying on the loop's own exit to stop
/// it.
///
/// The failure marker is matched without its trailing single quote
/// (unlike [`JOURNAL_FAILED_MARKER`]), since that quote cannot itself
/// appear inside the single-quoted glob a `case` pattern needs here.
///
/// # Errors
///
/// Returns an error if the local `ssh` could not be spawned.
fn stream_unit(
  transport: &dyn Transport,
  target: &TargetAddr,
  unit_name: &str,
  started_at: &str,
) -> Result<bool> {
  let script = format!(
    "tmp_dir=$(mktemp -d); fifo=\"$tmp_dir/journal.fifo\"; mkfifo \"$fifo\"; \
     journalctl -u {} -f --since {} --no-pager > \"$fifo\" & pid=$!; while \
     IFS= read -r line; do printf '%s\\n' \"$line\"; case \"$line\" in *': \
     Succeeded.'*) kill \"$pid\" 2>/dev/null; rm -rf \"$tmp_dir\"; exit 0 ;; \
     *': Failed with result'*) kill \"$pid\" 2>/dev/null; rm -rf \
     \"$tmp_dir\"; exit 1 ;; esac; done < \"$fifo\"; rm -rf \"$tmp_dir\"; \
     exit 1",
    shell_quote(unit_name),
    shell_quote(started_at),
  );
  let outcome = transport.run(target, &RemoteCommand::streaming(script))?;
  Ok(outcome.success)
}

/// Fire `receipt.notify` for a confirmed failure without disturbing the
/// receipt itself, logging rather than propagating a marker-file I/O
/// error - the caller is already about to report the failure to the
/// operator via its own `bail!`, and a broken notify marker must not
/// swallow that.
fn notify_failure(state_dir: &Path, push_id: &str, receipt: &PushReceipt) {
  if let Err(err) =
    receipt::mark_notified_and_notify(state_dir, push_id, receipt)
  {
    warn!(
      push_id,
      error = %err,
      "failed to record --notify as fired for a failed unit"
    );
  }
}

/// Dispatch `xi push --reattach`.
///
/// With an empty `id` (`xi push --reattach` on its own), lists every
/// local receipt newest first. With an id, `SSH`s to that receipt's
/// target and asks `systemctl show` the named unit's state, falling back
/// to `journalctl` if `--collect` already unloaded it, or following a
/// still-running unit's journal live until it reaches one of those same
/// terminal states.
///
/// # Errors
///
/// Propagates [`list`]'s error for an empty `id`. For a non-empty `id`,
/// errors if no receipt exists for it, if the `systemctl show` query
/// itself fails (see [`query_unit`]), if a `journalctl` fallback query
/// fails or finds no terminal status line (see [`query_journal`]), if the
/// local `ssh` for a live stream could not be spawned (see
/// [`stream_unit`]), or if the unit is found (by any of those three
/// paths) to have failed - a failed reattach deliberately exits non-zero
/// so the operator has to acknowledge it rather than it passing silently.
pub fn run(
  id: &str,
  state_dir: &Path,
  transport: &dyn Transport,
) -> Result<()> {
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
        "{} is still running on {}; streaming its journal until it finishes \
         (Ctrl-C to stop watching without affecting the unit)",
        receipt.unit_name, receipt.target
      );
      if stream_unit(transport, &addr, &receipt.unit_name, &receipt.started_at)?
      {
        println!("{} succeeded on {}", receipt.unit_name, receipt.target);
        receipt::remove_and_notify(state_dir, id, &receipt)
      } else {
        notify_failure(state_dir, id, &receipt);
        bail!(
          "{} failed on {} (see the streamed journal above)",
          receipt.unit_name,
          receipt.target,
        );
      }
    },
    ReattachOutcome::Succeeded => {
      println!("{} succeeded on {}", receipt.unit_name, receipt.target);
      receipt::remove_and_notify(state_dir, id, &receipt)
    },
    ReattachOutcome::Failed {
      exec_main_status,
      detail,
    } => {
      notify_failure(state_dir, id, &receipt);
      bail!(
        "{} failed on {} (ExecMainStatus={exec_main_status}): {detail}",
        receipt.unit_name,
        receipt.target,
      );
    },
    ReattachOutcome::Collected => {
      match query_journal(
        transport,
        &addr,
        &receipt.unit_name,
        &receipt.started_at,
      )? {
        JournalOutcome::Succeeded => {
          println!(
            "{} succeeded on {} (recovered from journalctl - the unit was \
             already `--collect`ed)",
            receipt.unit_name, receipt.target
          );
          receipt::remove_and_notify(state_dir, id, &receipt)
        },
        JournalOutcome::Failed { result } => {
          notify_failure(state_dir, id, &receipt);
          bail!(
            "{} failed on {} (result={result}, recovered from journalctl - \
             the unit was already `--collect`ed)",
            receipt.unit_name,
            receipt.target,
          );
        },
      }
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
    fn run(
      &self,
      target: &TargetAddr,
      _cmd: &RemoteCommand,
    ) -> Result<Outcome> {
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
      _retry_attempts: u32,
    ) -> Result<()> {
      Ok(())
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
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
      _retry_attempts: u32,
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
    fn run(
      &self,
      _target: &TargetAddr,
      _cmd: &RemoteCommand,
    ) -> Result<Outcome> {
      Ok(self.0.clone())
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: super::super::args::CopyVia,
      _retry_attempts: u32,
    ) -> Result<()> {
      Ok(())
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
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
      _retry_attempts: u32,
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
      "ActiveState=active\nSubState=running\nExecMainStatus=0\nExecMainCode=0\\
       n",
    ));
    assert_eq!(
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query"),
      ReattachOutcome::Running
    );
  }

  #[test]
  fn a_deactivating_unit_is_running() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=deactivating\nSubState=stop-sigterm\nExecMainStatus=0\\
       nExecMainCode=0\n",
    ));
    assert_eq!(
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query"),
      ReattachOutcome::Running
    );
  }

  #[test]
  fn a_zero_exit_unit_succeeded() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nExecMainCode=1\n",
    ));
    assert_eq!(
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query"),
      ReattachOutcome::Succeeded
    );
  }

  #[test]
  fn a_non_zero_exit_unit_failed() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=1\nExecMainCode=1\n",
    ));
    let outcome =
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query");
    assert_eq!(outcome, ReattachOutcome::Failed {
      exec_main_status: 1,
      detail:           "ActiveState=failed SubState=failed ExecMainCode=1"
        .to_owned(),
    });
  }

  #[test]
  fn a_killed_unit_failed() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=9\nExecMainCode=2\n",
    ));
    let outcome =
      query_unit(&transport, &addr(), "xi-push-web1-1").expect("query");
    assert_eq!(outcome, ReattachOutcome::Failed {
      exec_main_status: 9,
      detail:           "ActiveState=failed SubState=failed ExecMainCode=2"
        .to_owned(),
    });
  }

  #[test]
  fn a_never_recorded_main_process_reads_as_collected() {
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nExecMainCode=0\n",
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
  fn reattaching_to_a_succeeded_unit_removes_the_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nExecMainCode=1\n",
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
  fn reattaching_to_a_succeeded_unit_runs_notify_exactly_once() {
    let tmp = TempDir::new().expect("tempdir");
    let marker = tmp.path().join("notified");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System)
      .with_notify(Some(format!(
        "touch {}",
        shell_quote(&marker.display().to_string())
      )));
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nExecMainCode=1\n",
    ));

    run("web1-push", tmp.path(), &transport).expect("success is not an error");
    assert!(
      marker.exists(),
      "expected --notify to run once the unit's success was confirmed"
    );
  }

  #[test]
  fn reattaching_to_a_failed_unit_errors_and_keeps_the_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=1\nExecMainCode=1\n",
    ));

    let err = run("web1-push", tmp.path(), &transport)
      .expect_err("a failed unit must not pass silently");
    assert!(format!("{err}").contains("ExecMainStatus=1"), "got: {err}");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "an operator must still acknowledge a failure before its receipt goes \
       away"
    );
  }

  #[test]
  fn reattaching_to_a_failed_unit_runs_notify_exactly_once() {
    // xi-enwe.3.2: a confirmed failure is a terminal observation too (the
    // spec says "exactly once per terminal observation", not "per
    // success") - --notify must fire here even though the receipt itself
    // stays in place.
    let tmp = TempDir::new().expect("tempdir");
    let marker = tmp.path().join("notified");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System)
      .with_notify(Some(format!(
        "touch {}",
        shell_quote(&marker.display().to_string())
      )));
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=1\nExecMainCode=1\n",
    ));

    let _ = run("web1-push", tmp.path(), &transport)
      .expect_err("a failed unit must still error");
    assert!(
      marker.exists(),
      "expected --notify to run once the unit's failure was confirmed"
    );
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "notifying on failure must not remove the receipt"
    );

    std::fs::remove_file(&marker).expect("clear marker to detect a rerun");
    let _ = run("web1-push", tmp.path(), &transport).expect_err(
      "a second reattach against the same still-failed unit still errors",
    );
    assert!(
      !marker.exists(),
      "a second observation of the same confirmed failure must not refire \
       --notify"
    );
  }

  #[test]
  fn notify_fires_exactly_once_under_a_race_between_two_concurrent_reattaches_on_failure()
   {
    // xi-enwe.3.2's own acceptance criteria: notify must not double-fire
    // when two processes race to observe the same terminal state at
    // nearly the same moment. The success side of this race is already
    // covered by `remove_file`'s atomicity (xi-enwe.3.1); this exercises
    // the failure side's marker-file gate under genuine thread
    // concurrency rather than just sequential re-invocation.
    let tmp = TempDir::new().expect("tempdir");
    let fire_count = tmp.path().join("fire-count");
    std::fs::write(&fire_count, "").expect("seed counter file");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System)
      .with_notify(Some(format!(
        "printf x >> {}",
        shell_quote(&fire_count.display().to_string())
      )));
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");

    let state_dir = std::sync::Arc::new(tmp.path().to_owned());
    let transport = std::sync::Arc::new(ScriptedTransport(Outcome::ok(
      "ActiveState=failed\nSubState=failed\nExecMainStatus=1\nExecMainCode=1\n",
    )));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

    let runners: Vec<_> = (0..2)
      .map(|_| {
        let state_dir = std::sync::Arc::clone(&state_dir);
        let transport = std::sync::Arc::clone(&transport);
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
          barrier.wait();
          run("web1-push", &state_dir, transport.as_ref())
        })
      })
      .collect();

    for handle in runners {
      let _ = handle
        .join()
        .expect("thread")
        .expect_err("both racing reattaches must still report the failure");
    }

    assert_eq!(
      std::fs::read_to_string(&fire_count).expect("read counter"),
      "x",
      "--notify must have run exactly once across both racing reattaches"
    );
  }

  /// A [`Transport`] that answers a `systemctl show`, a captured
  /// `journalctl` query, or a streaming `journalctl -f` differently -
  /// for `run`'s with-id path, which chains more than one remote call
  /// once the first answer is `Collected` or `Running`.
  struct RoutedTransport {
    systemctl: Outcome,
    journal:   Outcome,
    stream:    Outcome,
  }

  impl Transport for RoutedTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      cmd: &RemoteCommand,
    ) -> Result<Outcome> {
      Ok(if cmd.script.contains("systemctl show") {
        self.systemctl.clone()
      } else if cmd.streaming {
        self.stream.clone()
      } else {
        self.journal.clone()
      })
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: super::super::args::CopyVia,
      _retry_attempts: u32,
    ) -> Result<()> {
      Ok(())
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
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
      _retry_attempts: u32,
    ) -> Result<()> {
      Ok(())
    }
  }

  const COLLECTED_SYSTEMCTL: &str =
    "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nExecMainCode=0\n";
  const RUNNING_SYSTEMCTL: &str =
    "ActiveState=active\nSubState=running\nExecMainStatus=0\nExecMainCode=0\n";

  #[test]
  fn parse_journal_status_finds_a_trailing_succeeded_line() {
    assert_eq!(
      parse_journal_status(
        "Nov 06 09:59:00 host systemd[1]: Started xi-push-web1-1.\nNov 06 \
         10:00:00 host systemd[1]: xi-push-web1-1.service: Succeeded.\n"
      ),
      Some(JournalOutcome::Succeeded)
    );
  }

  #[test]
  fn parse_journal_status_extracts_the_failed_result() {
    assert_eq!(
      parse_journal_status(
        "Nov 06 10:00:05 host systemd[1]: xi-push-web1-1.service: Main \
         process exited, code=exited, status=1/FAILURE\nNov 06 10:00:05 host \
         systemd[1]: xi-push-web1-1.service: Failed with result 'exit-code'.\n"
      ),
      Some(JournalOutcome::Failed {
        result: "exit-code".to_owned(),
      })
    );
  }

  #[test]
  fn parse_journal_status_is_none_without_a_terminal_line() {
    assert_eq!(
      parse_journal_status(
        "Nov 06 09:59:00 host systemd[1]: Started xi-push-web1-1.\n"
      ),
      None
    );
  }

  #[test]
  fn reattaching_to_a_collected_unit_falls_back_to_journalctl_success() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = RoutedTransport {
      systemctl: Outcome::ok(COLLECTED_SYSTEMCTL),
      journal:   Outcome::ok(
        "Nov 06 10:00:00 host systemd[1]: xi-push-web1-1.service: Succeeded.\n",
      ),
      stream:    Outcome::ok(""),
    };

    run("web1-push", tmp.path(), &transport)
      .expect("journalctl fallback resolves success");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_none(),
      "a collected-but-succeeded unit's receipt must be removed"
    );
  }

  #[test]
  fn reattaching_to_a_collected_unit_falls_back_to_journalctl_failure() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = RoutedTransport {
      systemctl: Outcome::ok(COLLECTED_SYSTEMCTL),
      journal:   Outcome::ok(
        "Nov 06 10:00:05 host systemd[1]: xi-push-web1-1.service: Failed with \
         result 'exit-code'.\n",
      ),
      stream:    Outcome::ok(""),
    };

    let err = run("web1-push", tmp.path(), &transport)
      .expect_err("a failure recovered via journalctl must not pass silently");
    assert!(format!("{err}").contains("exit-code"), "got: {err}");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "an operator must still acknowledge a failure before its receipt goes \
       away"
    );
  }

  #[test]
  fn reattaching_to_a_collected_and_journal_failed_unit_runs_notify_exactly_once()
   {
    let tmp = TempDir::new().expect("tempdir");
    let marker = tmp.path().join("notified");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System)
      .with_notify(Some(format!(
        "touch {}",
        shell_quote(&marker.display().to_string())
      )));
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = RoutedTransport {
      systemctl: Outcome::ok(COLLECTED_SYSTEMCTL),
      journal:   Outcome::ok(
        "Nov 06 10:00:05 host systemd[1]: xi-push-web1-1.service: Failed with \
         result 'exit-code'.\n",
      ),
      stream:    Outcome::ok(""),
    };

    let _ = run("web1-push", tmp.path(), &transport)
      .expect_err("a failure recovered via journalctl must still error");
    assert!(
      marker.exists(),
      "expected --notify to run once the collected unit's failure was \
       recovered"
    );
  }

  #[test]
  fn reattaching_to_a_collected_unit_with_no_terminal_journal_line_errors() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = RoutedTransport {
      systemctl: Outcome::ok(COLLECTED_SYSTEMCTL),
      journal:   Outcome::ok(
        "Nov 06 09:59:00 host systemd[1]: Started xi-push-web1-1.\n",
      ),
      stream:    Outcome::ok(""),
    };

    let err = run("web1-push", tmp.path(), &transport)
      .expect_err("no terminal status line must error, not silently succeed");
    assert!(
      format!("{err}").contains("no terminal status line"),
      "got: {err}"
    );
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "an unresolved reattach must not discard the receipt"
    );
  }

  #[test]
  fn reattaching_to_a_running_unit_streams_to_success_and_removes_the_receipt()
  {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = RoutedTransport {
      systemctl: Outcome::ok(RUNNING_SYSTEMCTL),
      journal:   Outcome::ok(""),
      stream:    Outcome::ok(""),
    };

    run("web1-push", tmp.path(), &transport)
      .expect("a stream that ends on the success marker is not an error");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_none(),
      "a unit that streamed to success must have its receipt removed"
    );
  }

  #[test]
  fn reattaching_to_a_running_unit_streams_to_failure_and_keeps_the_receipt() {
    let tmp = TempDir::new().expect("tempdir");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System);
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = RoutedTransport {
      systemctl: Outcome::ok(RUNNING_SYSTEMCTL),
      journal:   Outcome::ok(""),
      stream:    Outcome::failed(""),
    };

    let err = run("web1-push", tmp.path(), &transport).expect_err(
      "a stream that ends on the failure marker must not pass silently",
    );
    assert!(format!("{err}").contains("failed on web1"), "got: {err}");
    assert!(
      receipt::read(tmp.path(), "web1-push")
        .expect("read")
        .is_some(),
      "an operator must still acknowledge a failure before its receipt goes \
       away"
    );
  }

  #[test]
  fn reattaching_to_a_running_unit_that_streams_to_failure_runs_notify_exactly_once()
   {
    let tmp = TempDir::new().expect("tempdir");
    let marker = tmp.path().join("notified");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System)
      .with_notify(Some(format!(
        "touch {}",
        shell_quote(&marker.display().to_string())
      )));
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");
    let transport = RoutedTransport {
      systemctl: Outcome::ok(RUNNING_SYSTEMCTL),
      journal:   Outcome::ok(""),
      stream:    Outcome::failed(""),
    };

    let _ = run("web1-push", tmp.path(), &transport)
      .expect_err("a stream that ends on the failure marker must still error");
    assert!(
      marker.exists(),
      "expected --notify to run once the streamed unit's failure was confirmed"
    );
  }

  #[test]
  fn notify_fires_exactly_once_under_a_race_between_the_original_process_and_a_reattach_on_success()
   {
    // xi-enwe.3.2's named scenario: "the original push process reconnects
    // and observes a terminal state at the same moment a concurrent
    // --reattach also observes it." Simulate the original process's side
    // with `receipt::remove_and_notify` directly (what
    // `orchestrate::switch_with_receipt` calls on its own success path)
    // racing a concurrent `--reattach` against the same receipt.
    let tmp = TempDir::new().expect("tempdir");
    let fire_count = tmp.path().join("fire-count");
    std::fs::write(&fire_count, "").expect("seed counter file");
    let receipt = PushReceipt::new("web1", "xi-push-web1-1", Scope::System)
      .with_notify(Some(format!(
        "printf x >> {}",
        shell_quote(&fire_count.display().to_string())
      )));
    receipt::write(tmp.path(), "web1-push", &receipt).expect("write");

    let state_dir = std::sync::Arc::new(tmp.path().to_owned());
    let transport = std::sync::Arc::new(ScriptedTransport(Outcome::ok(
      "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nExecMainCode=1\n",
    )));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

    let original_process = {
      let state_dir = std::sync::Arc::clone(&state_dir);
      let barrier = std::sync::Arc::clone(&barrier);
      std::thread::spawn(move || {
        barrier.wait();
        receipt::remove_and_notify(&state_dir, "web1-push", &receipt)
      })
    };
    let reattach = {
      let state_dir = std::sync::Arc::clone(&state_dir);
      let transport = std::sync::Arc::clone(&transport);
      let barrier = std::sync::Arc::clone(&barrier);
      std::thread::spawn(move || {
        barrier.wait();
        run("web1-push", &state_dir, transport.as_ref())
      })
    };

    original_process
      .join()
      .expect("original process thread")
      .expect("remove_and_notify");
    // The loser of the race finds the receipt already gone, which
    // `receipt::read` inside `run` reports as "no receipt found" - not a
    // notify bug, just the expected shape of losing the removal race.
    let _ = reattach.join().expect("reattach thread");

    assert_eq!(
      std::fs::read_to_string(&fire_count).expect("read counter"),
      "x",
      "--notify must have run exactly once across the original process and \
       the reattach"
    );
  }

  /// A [`Transport`] that actually executes `cmd.script` locally through
  /// `sh -c`, with `PATH` pointing at a fixture `journalctl` binary
  /// instead of hitting a network target - the only way to prove the
  /// remote script [`stream_unit`] builds actually terminates, rather
  /// than merely asserting on its text.
  ///
  /// Kills the child and panics if it outlives `TIMEOUT`: a regression
  /// of the journalctl-kill fix would otherwise hang this test exactly
  /// as it hangs a real `xi push --reattach`.
  struct LocalShellTransport {
    path_env: String,
  }

  impl LocalShellTransport {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    fn with_fixture_bin(bin_dir: &std::path::Path) -> Self {
      let existing = std::env::var("PATH").unwrap_or_default();
      Self {
        path_env: format!("{}:{existing}", bin_dir.display()),
      }
    }
  }

  impl Transport for LocalShellTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      cmd: &RemoteCommand,
    ) -> Result<Outcome> {
      let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd.script)
        .env("PATH", &self.path_env)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sh -c <script>");

      let start = std::time::Instant::now();
      // Killed and reported as a failure rather than let to run forever: a
      // regression of the journalctl-kill fix would otherwise hang this
      // test exactly as it hangs a real `xi push --reattach`. The calling
      // test's own `elapsed < 5s` assertion is what actually catches the
      // regression - this bound just stops the test process from hanging
      // on one.
      let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
          break Some(status);
        }
        if start.elapsed() > Self::TIMEOUT {
          let _ = child.kill();
          let _ = child.wait();
          break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
      };
      Ok(status.map_or_else(
        || {
          Outcome::failed(format!(
            "script did not terminate within {:?}",
            Self::TIMEOUT
          ))
        },
        |status| {
          Outcome {
            success: status.success(),
            stdout:  String::new(),
            stderr:  String::new(),
          }
        },
      ))
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: super::super::args::CopyVia,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("stream_unit never copies a closure")
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("stream_unit never uploads a secret")
    }

    fn sync_dir(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _remote: &str,
      _excludes: &[&str],
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("stream_unit never syncs a directory")
    }
  }

  /// Write a fake `journalctl` under `dir/journalctl` that behaves the
  /// way the real one does `-f`-ing an already-`--collect`-ed unit: it
  /// prints a couple of lines including `terminal_line`, then blocks
  /// (simulating `poll()`-ing for journal entries that will never
  /// arrive) instead of ever exiting on its own.
  fn write_fake_journalctl(dir: &std::path::Path, terminal_line: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("journalctl");
    std::fs::write(
      &path,
      format!(
        "#!/bin/sh\necho 'Nov 06 10:00:00 host systemd[1]: Started \
         xi-push-web1-1.'\nsleep 0.2\necho {}\nsleep 20\n",
        shell_quote(terminal_line),
      ),
    )
    .expect("write fake journalctl");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
      .expect("chmod +x fake journalctl");
  }

  #[test]
  fn stream_unit_kills_a_hanging_journalctl_f_and_returns_true_on_success() {
    // Bug fix (xi-bnow finding 1): a plain `journalctl -f | while ...`
    // pipe never terminates once the loop exits, because `--collect`
    // means no further journal entries will ever arrive to make
    // `journalctl -f` notice the closed pipe. This drives the actual
    // generated script through a real shell against a fixture
    // `journalctl` that hangs exactly that way, and the surrounding
    // `LocalShellTransport` will panic the test if it does not return
    // well before the fixture's 20s sleep would otherwise force it to.
    let bin_dir = TempDir::new().expect("tempdir");
    write_fake_journalctl(
      bin_dir.path(),
      "Nov 06 10:00:05 host systemd[1]: xi-push-web1-1.service: Succeeded.",
    );
    let transport = LocalShellTransport::with_fixture_bin(bin_dir.path());

    let start = std::time::Instant::now();
    let result = stream_unit(
      &transport,
      &addr(),
      "xi-push-web1-1",
      "2024-01-01T00:00:00Z",
    );
    let elapsed = start.elapsed();

    assert!(
      result.expect("stream_unit must not error"),
      "a terminal Succeeded. line must report success"
    );
    assert!(
      elapsed < std::time::Duration::from_secs(5),
      "stream_unit must kill the backgrounded journalctl and return promptly \
       once it sees the terminal line, not hang for the fake journalctl's \
       full 20s sleep - took {elapsed:?}"
    );
  }

  #[test]
  fn stream_unit_kills_a_hanging_journalctl_f_and_returns_false_on_failure() {
    let bin_dir = TempDir::new().expect("tempdir");
    write_fake_journalctl(
      bin_dir.path(),
      "Nov 06 10:00:05 host systemd[1]: xi-push-web1-1.service: Failed with \
       result 'exit-code'.",
    );
    let transport = LocalShellTransport::with_fixture_bin(bin_dir.path());

    let start = std::time::Instant::now();
    let result = stream_unit(
      &transport,
      &addr(),
      "xi-push-web1-1",
      "2024-01-01T00:00:00Z",
    );
    let elapsed = start.elapsed();

    assert!(
      !result.expect("stream_unit must not error"),
      "a terminal Failed with result line must report failure"
    );
    assert!(
      elapsed < std::time::Duration::from_secs(5),
      "stream_unit must kill the backgrounded journalctl and return promptly \
       once it sees the terminal line - took {elapsed:?}"
    );
  }
}
