//! The transaction boundary every mutating sources actor runs inside.
//!
//! Part of the `xi-sources-crash-recovery` capability
//! (`openspec/specs/xi-sources-crash-recovery/spec.md`, requirement
//! "Transaction boundary per actor").
//!
//! Every actor in the `xi-sources` family  -  switch, the gitops daemon,
//! push, bootstrap, sync, cache-prestage  -  can be killed between the
//! first and the last line of its critical section. The failure that
//! matters is not the lost work; it is the *inconsistent* leftovers: a
//! half-populated clone, a `sources-head` naming a sha that was never
//! realised, a receipt claiming an action that got rolled back.
//!
//! A [`Transaction`] makes that recoverable with three pieces:
//!
//! - a **sentinel** at `<state_dir>/transaction.json` naming the actor, its
//!   intent, when it started and its pid, so a later process can tell "someone
//!   is working" from "someone died working";
//! - a **shadow list**: nothing the actor builds is visible at its final path
//!   until [`Transaction::commit`] renames it there, so an abandoned
//!   transaction leaves only shadow paths, which are safe to delete;
//! - a **`Drop` impl** that rolls back, so an early `?`, a panic, or an
//!   operator Ctrl+C all unwind to the pre-transaction state without every call
//!   site having to remember to clean up.
//!
//! What a `Drop` impl cannot cover is SIGKILL and power loss. That is
//! exactly what the sentinel is for, and what [`super::repair`] reads.
//!
//! The lock protocol from `xi-sources-race` sits *outside* this: lock
//! first, then open the transaction, then commit, then unlock. A
//! transaction says "this work is half-done"; a lock says "this work is
//! mine". Conflating them would make a crashed actor's lock
//! indistinguishable from a live one.

use std::{
  fs,
  io,
  path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{
  atomic::write_atomic,
  receipt::{Receipt, ReceiptEntry},
  shadow::promote_dir,
};

/// Filename of the transaction sentinel inside the state dir.
pub const SENTINEL_NAME: &str = "transaction.json";

/// The current instant as ISO 8601 / RFC 3339 in UTC, to the second.
///
/// Second resolution on purpose: the sentinel's timestamp is read by a
/// human deciding whether an actor is wedged, not by anything that
/// needs to order two events in the same second.
#[must_use]
pub fn now_iso8601() -> String {
  chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The on-disk sentinel describing an in-progress transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sentinel {
  /// Which actor opened it, e.g. `switch` or `gitops-daemon`.
  pub actor:      String,
  /// What it is doing, in one line, for the repair report.
  pub intent:     String,
  /// When it opened, ISO 8601 / RFC 3339 in UTC.
  pub started_at: String,
  /// The pid that opened it, so liveness is checkable.
  pub pid:        u32,
}

impl Sentinel {
  /// Read the sentinel from `state_dir`, if one is present.
  ///
  /// A missing sentinel means "no transaction in progress" and is not
  /// an error: that is the normal state, and it is also what a
  /// pre-upgrade install looks like.
  ///
  /// A *malformed* sentinel is reported as a corrupt-but-present
  /// transaction rather than an error, because refusing to start is the
  /// one outcome that would make the damage permanent: repair has to be
  /// able to clear it.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors other than `NotFound`.
  pub fn load(state_dir: &Path) -> io::Result<Option<Self>> {
    let path = state_dir.join(SENTINEL_NAME);
    let raw = match fs::read_to_string(&path) {
      Ok(raw) => raw,
      Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(err) => return Err(err),
    };
    Ok(Some(serde_json::from_str(&raw).unwrap_or_else(|err| {
      warn!("{}: unreadable transaction sentinel: {err}", path.display());
      Self {
        actor:      "unknown".to_owned(),
        intent:     format!("unparsable sentinel: {err}"),
        started_at: String::new(),
        // 0 is never a live pid, so an unreadable sentinel is always
        // treated as abandoned and therefore always clearable.
        pid:        0,
      }
    })))
  }

  /// Whether the process that opened this transaction is still running.
  #[must_use]
  pub fn owner_is_live(&self) -> bool {
    pid_is_live(self.pid)
  }
}

/// Whether `pid` names a live process on this machine.
///
/// The null-signal probe, which is the only check that works for a
/// process we do not own. `EPERM` counts as live: a pid we are not
/// allowed to signal is emphatically still there, and treating it as
/// dead would let repair delete a running actor's shadow paths.
///
/// A pid of 0 is never a live process for this purpose (it means "my
/// process group" to `kill(2)`), so it is rejected up front and used as
/// the "owner unknown" marker by [`Sentinel::load`].
///
/// **Seam.** `xi-sources-race` owns the liveness check for the lock
/// protocol and lands in parallel; when it does, both should call one
/// implementation rather than two.
#[must_use]
pub fn pid_is_live(pid: u32) -> bool {
  if pid == 0 {
    return false;
  }
  #[cfg(unix)]
  {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    // A pid too large for an i32 cannot name a process we could signal.
    i32::try_from(pid).is_ok_and(|raw| {
      !matches!(kill(Pid::from_raw(raw), None), Err(Errno::ESRCH))
    })
  }
  #[cfg(not(unix))]
  {
    // No portable probe; assume live so repair never deletes the
    // working set of a running actor on an unsupported platform.
    true
  }
}

/// One thing a transaction built off to the side.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shadow {
  /// Removed on rollback, dropped on commit. The actor is responsible
  /// for whatever it does with the path itself.
  Scratch(PathBuf),
  /// Renamed over `to` on commit, removed on rollback.
  Promote {
    /// The shadow path the actor populated.
    from: PathBuf,
    /// Where it belongs once the work is known-good.
    to:   PathBuf,
  },
}

impl Shadow {
  const fn path(&self) -> &PathBuf {
    match self {
      Self::Scratch(path) | Self::Promote { from: path, .. } => path,
    }
  }
}

/// An open transaction: work in progress, with a published sentinel.
///
/// Construct with [`Transaction::begin`], finish with
/// [`Transaction::commit`]. Dropping without committing rolls back.
#[derive(Debug)]
pub struct Transaction {
  state_dir: PathBuf,
  sentinel:  Sentinel,
  shadows:   Vec<Shadow>,
  /// Sanctioned state-dir writes held back until commit, in order.
  state:     Vec<(String, String)>,
  /// Receipt entries appended as the last step of commit.
  receipt:   Vec<ReceiptEntry>,
  settled:   bool,
}

impl Transaction {
  /// Open a transaction for `actor`, publishing the sentinel.
  ///
  /// An existing sentinel is *overwritten*, not refused. Mutual
  /// exclusion is the lock's job (`xi-sources-race`), which the caller
  /// has already taken; a sentinel left by a dead actor must not be
  /// able to wedge the machine forever, and one left by a live actor is
  /// a lock bug, not something this layer can fix by failing.
  ///
  /// # Errors
  ///
  /// Propagates failures to create the state dir or write the sentinel.
  pub fn begin(
    state_dir: &Path,
    actor: impl Into<String>,
    intent: impl Into<String>,
  ) -> io::Result<Self> {
    let sentinel = Sentinel {
      actor:      actor.into(),
      intent:     intent.into(),
      started_at: now_iso8601(),
      pid:        std::process::id(),
    };

    fs::create_dir_all(state_dir)?;
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))?;
    }
    write_atomic(
      &state_dir.join(SENTINEL_NAME),
      &serde_json::to_vec_pretty(&sentinel)?,
      Some(0o600),
    )?;
    debug!(
      actor = %sentinel.actor,
      intent = %sentinel.intent,
      "transaction opened"
    );

    Ok(Self {
      state_dir: state_dir.to_path_buf(),
      sentinel,
      shadows: Vec::new(),
      state: Vec::new(),
      receipt: Vec::new(),
      settled: false,
    })
  }

  /// The sentinel this transaction published.
  #[must_use]
  pub const fn sentinel(&self) -> &Sentinel {
    &self.sentinel
  }

  /// Register a path to delete if this transaction does not commit.
  pub fn scratch(&mut self, path: impl Into<PathBuf>) {
    self.shadows.push(Shadow::Scratch(path.into()));
  }

  /// Register a shadow path to rename over `to` at commit.
  ///
  /// The rename happens in [`Transaction::commit`] rather than when the
  /// actor finishes populating `from`, which is what makes the spec's
  /// "all shadow paths have been renamed into place" a property of
  /// committing rather than a thing each actor has to sequence right.
  pub fn promote(&mut self, from: impl Into<PathBuf>, to: impl Into<PathBuf>) {
    self.shadows.push(Shadow::Promote {
      from: from.into(),
      to:   to.into(),
    });
  }

  /// Hold back a sanctioned state-dir write until commit.
  ///
  /// Staging rather than writing is what closes the window where a
  /// crash leaves state naming work that was never promoted: until
  /// commit runs, the state dir still describes the previous, intact
  /// world.
  pub fn stage_state(&mut self, name: impl Into<String>, value: &str) {
    self.state.push((name.into(), value.to_owned()));
  }

  /// Hold back a receipt entry until commit.
  ///
  /// The receipt append is the last write commit performs before
  /// removing the sentinel, per the capability's receipt consistency
  /// rule. Staging is how that ordering is enforced structurally
  /// instead of by convention.
  pub fn stage_receipt(&mut self, entry: ReceiptEntry) {
    self.receipt.push(entry);
  }

  /// Promote every shadow, flush staged writes, drop the sentinel.
  ///
  /// Order is load-bearing and is the spec's: shadows first (so the
  /// world is correct before anything claims it is), then state (so
  /// nothing names a sha that is not on disk), then the receipt, then
  /// the sentinel. A crash at any step leaves a state repair can fix,
  /// and a crash after the last one leaves nothing to fix.
  ///
  /// # Errors
  ///
  /// Propagates the first failure. The transaction is *not* rolled back
  /// on a commit error: a half-promoted commit is exactly the case
  /// `xi sources repair` is built to finish, and unwinding it here
  /// would have to undo renames that other readers may already have
  /// observed.
  pub fn commit(mut self) -> io::Result<()> {
    self.settled = true;

    for shadow in &self.shadows {
      if let Shadow::Promote { from, to } = shadow {
        promote_dir(from, to)?;
      }
    }

    for (name, value) in &self.state {
      super::write_state_file(&self.state_dir, name, value)?;
    }

    if !self.receipt.is_empty() {
      Receipt::append(&self.state_dir, self.receipt.drain(..))?;
    }

    self.clear_sentinel()?;
    debug!(actor = %self.sentinel.actor, "transaction committed");
    Ok(())
  }

  /// Remove every shadow path and the sentinel, keeping staged writes
  /// unapplied.
  ///
  /// # Errors
  ///
  /// Propagates the failure to remove the sentinel. Shadow removal
  /// failures are logged rather than propagated: a shadow that cannot
  /// be deleted now is still inert, and [`super::repair`] will find it
  /// again, whereas failing here would hide the actor's original error
  /// behind a cleanup error.
  pub fn rollback(mut self) -> io::Result<()> {
    self.settled = true;
    self.undo_shadows();
    self.clear_sentinel()
  }

  fn undo_shadows(&mut self) {
    for shadow in self.shadows.drain(..) {
      let path = shadow.path();
      if let Err(err) = remove_path(path)
        && err.kind() != io::ErrorKind::NotFound
      {
        warn!("could not remove shadow {}: {err}", path.display());
      }
    }
  }

  fn clear_sentinel(&self) -> io::Result<()> {
    match fs::remove_file(self.state_dir.join(SENTINEL_NAME)) {
      Ok(()) => Ok(()),
      Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
      Err(err) => Err(err),
    }
  }
}

impl Drop for Transaction {
  /// Roll back a transaction that was never settled.
  ///
  /// This is the panic and early-`?` path. It is best-effort by
  /// necessity  -  a `Drop` impl has nowhere to return an error, and
  /// during an unwind it must not panic  -  so failures are logged and
  /// left for [`super::repair`], which is the backstop for everything
  /// `Drop` cannot reach anyway (SIGKILL, OOM, power loss).
  fn drop(&mut self) {
    if self.settled {
      return;
    }
    warn!(
      actor = %self.sentinel.actor,
      intent = %self.sentinel.intent,
      "transaction abandoned; rolling back"
    );
    self.undo_shadows();
    if let Err(err) = self.clear_sentinel() {
      warn!("could not remove transaction sentinel: {err}");
    }
  }
}

/// Delete a path whether it is a file, a symlink, or a directory tree.
fn remove_path(path: &Path) -> io::Result<()> {
  match fs::symlink_metadata(path) {
    Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
    Ok(_) => fs::remove_file(path),
    Err(err) => Err(err),
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn own_pid_is_live_and_pid_zero_is_not() {
    assert!(pid_is_live(std::process::id()));
    assert!(!pid_is_live(0));
  }

  #[test]
  fn begin_publishes_a_sentinel_naming_this_process() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let txn =
      Transaction::begin(tmp.path(), "switch", "adopt origin").expect("begin");

    let on_disk = Sentinel::load(tmp.path())
      .expect("load")
      .expect("sentinel present");
    assert_eq!(on_disk, *txn.sentinel());
    assert_eq!(on_disk.pid, std::process::id());
    assert!(on_disk.owner_is_live());

    txn.commit().expect("commit");
  }

  #[test]
  fn unparsable_sentinel_reads_as_abandoned() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    fs::write(tmp.path().join(SENTINEL_NAME), "not json").expect("write junk");

    let sentinel = Sentinel::load(tmp.path())
      .expect("load")
      .expect("present but corrupt");
    assert!(!sentinel.owner_is_live());
  }

  #[test]
  fn staged_state_lands_only_on_commit() {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    let mut txn = Transaction::begin(tmp.path(), "switch", "x").expect("begin");
    txn.stage_state("sources-head", "cafe");
    drop(txn);
    assert_eq!(
      super::super::read_state_file(tmp.path(), "sources-head").expect("read"),
      None,
      "a rolled-back transaction must not have written state"
    );

    let mut txn = Transaction::begin(tmp.path(), "switch", "x").expect("begin");
    txn.stage_state("sources-head", "cafe");
    txn.commit().expect("commit");
    assert_eq!(
      super::super::read_state_file(tmp.path(), "sources-head").expect("read"),
      Some("cafe".to_owned())
    );
  }
}
