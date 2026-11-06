//! BDD: contract for the `xi-sources-crash-recovery` capability
//! (openspec/changes/xi-sources-crash-recovery/specs/…/spec.md).
//!
//! One test per SC-Y-* criterion in
//! `openspec/changes/xi-sources-crash-recovery/proposal.md`, named
//! after it, so a failure here names exactly one broken requirement.
//!
//! # Why these do not actually SIGKILL anything
//!
//! Each criterion has two halves: "a kill at point P leaves state S"
//! and "repair turns S into a correct machine". Only the second half is
//! this capability's logic; the first is a property of the orderings in
//! `transaction::Transaction::commit`, and a test that really forked
//! and killed would be asserting the kernel's timing, not xi's code -
//! the definition of a flaky test.
//!
//! So the halves are tested separately and both are tested. State S is
//! constructed on disk exactly as a kill at P would leave it, and
//! repair is driven over it. The orderings that *produce* S are pinned
//! directly: `staged_state_lands_only_on_commit` and the two commit
//! scenarios here assert that nothing names a sha before the sha is on
//! disk, which is what makes the constructed states the real ones.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{
  fs,
  path::{Path, PathBuf},
  process::Command,
};

use tempfile::TempDir;
use xi_core::sources::{
  Scope,
  atomic::temp_path_for,
  receipt::{Receipt, ReceiptEntry},
  repair::{
    Action,
    Finding,
    Outcome,
    REPAIR_SCHEMA_VERSION,
    RepairContext,
    SYNC_SENTINEL_NAME,
    SyncResolution,
    WRITE_XI_CONF_ACTION,
    discover,
    execute,
  },
  shadow::{incoming_path, previous_path},
  transaction::{SENTINEL_NAME, Sentinel, Transaction},
  write_state_file,
};

/// A pid no process can hold: above every `pid_max` Linux accepts.
const DEAD_PID: u32 = 0x7FFF_FFFF;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// One machine, with every path repair looks at injected.
struct Machine {
  _tmp:    TempDir,
  root:    PathBuf,
  ctx:     RepairContext,
  nix_dir: PathBuf,
}

impl Machine {
  fn new() -> Self {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let nix_dir = root.join("nix");
    for dir in [&root.join("state"), &nix_dir] {
      fs::create_dir_all(dir).expect("mkdir");
    }
    let ctx = RepairContext {
      scope:            Scope::User,
      sources_dir:      root.join("sources"),
      state_dir:        root.join("state"),
      nix_conf:         Some(nix_dir.join("nix.conf")),
      xi_conf:          Some(nix_dir.join("xi.conf")),
      receipt_evidence: Vec::new(),
      resume_sync:      false,
    };
    Self {
      _tmp: tmp,
      root,
      ctx,
      nix_dir,
    }
  }

  fn sources_dir(&self) -> &Path {
    &self.ctx.sources_dir
  }

  fn state_dir(&self) -> &Path {
    &self.ctx.state_dir
  }

  /// A git repo at `path` with one commit, as a clone would look.
  fn repo_at(&self, path: &Path, label: &str) -> String {
    fs::create_dir_all(path).expect("mkdir repo");
    fs::write(path.join("flake.nix"), format!("# {label}\n"))
      .expect("write flake");
    for args in [vec!["init", "-q", "-b", "main"], vec!["add", "-A"], vec![
      "commit", "-q", "-m", label,
    ]] {
      let ok = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["-c", "user.email=t@e", "-c", "user.name=t"])
        .args(&args)
        .status()
        .expect("run git")
        .success();
      assert!(ok, "git {args:?} failed in {}", path.display());
    }
    head_of(path)
  }

  fn record(&self, name: &str, value: &str) {
    write_state_file(self.state_dir(), name, value).expect("write state");
  }

  /// A sentinel left behind by a process that is gone.
  fn plant_abandoned_sentinel(&self, actor: &str, intent: &str) {
    let sentinel = Sentinel {
      actor:      actor.to_owned(),
      intent:     intent.to_owned(),
      started_at: "2020-01-01T00:00:00Z".to_owned(),
      pid:        DEAD_PID,
    };
    fs::write(
      self.state_dir().join(SENTINEL_NAME),
      serde_json::to_vec_pretty(&sentinel).expect("serialise"),
    )
    .expect("write sentinel");
  }

  fn plan(&self) -> Vec<Finding> {
    discover(&self.ctx).expect("discover").findings
  }

  fn repair(&self) -> xi_core::sources::repair::RepairReport {
    execute(&self.ctx, false).expect("repair")
  }

  fn dry_run(&self) -> xi_core::sources::repair::RepairReport {
    execute(&self.ctx, true).expect("dry run")
  }
}

fn head_of(repo: &Path) -> String {
  let out = Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["rev-parse", "HEAD"])
    .output()
    .expect("git rev-parse");
  String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A clone of `from`, so the two share object history the way a real
/// sources dir and working dir do - `conflict::sync`'s `reset --soft`
/// needs the sha it is resetting to be an object the destination repo
/// actually has.
fn clone_repo(from: &Path, to: &Path) {
  let ok = Command::new("git")
    .args(["clone", "-q"])
    .arg(from)
    .arg(to)
    .status()
    .expect("run git clone")
    .success();
  assert!(ok, "clone {} into {} failed", from.display(), to.display());
}

/// Every file under `root`, as path to bytes, `.git` included.
fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
  fn walk(
    dir: &Path,
    base: &Path,
    into: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
  ) {
    for entry in fs::read_dir(dir).expect("read dir") {
      let path = entry.expect("entry").path();
      if path.is_dir() {
        walk(&path, base, into);
      } else {
        let rel = path.strip_prefix(base).expect("under base").to_path_buf();
        into.insert(rel, fs::read(&path).unwrap_or_default());
      }
    }
  }
  let mut out = std::collections::BTreeMap::new();
  if root.is_dir() {
    walk(root, root, &mut out);
  }
  out
}

// ---------------------------------------------------------------------------
// SC-Y-001: killed mid-fetch
// ---------------------------------------------------------------------------

/// SC-Y-001: a fetch killed before it was promoted leaves the shadow on
/// disk and the sources dir untouched; repair deletes the shadow and
/// says so.
#[test]
fn sc_y_001_a_killed_fetch_leaves_the_sources_dir_intact_and_is_swept() {
  let m = Machine::new();
  let head = m.repo_at(m.sources_dir(), "live");
  let before = snapshot(m.sources_dir());

  // What a kill mid-fetch leaves: a populated shadow, no promote.
  let incoming = incoming_path(m.sources_dir());
  m.repo_at(&incoming, "half-fetched");

  // The sources dir is untouched by the interrupted fetch, which is the
  // whole point of fetching into a sibling.
  assert_eq!(
    snapshot(m.sources_dir()),
    before,
    "an interrupted fetch must not have touched the live clone"
  );
  assert_eq!(head_of(m.sources_dir()), head);

  let findings = m.plan();
  assert!(
    findings
      .iter()
      .any(|f| matches!(f, Finding::DanglingIncoming { .. })),
    "repair must see the shadow: {findings:?}"
  );

  let report = m.repair();
  assert_eq!(report.outcome, Outcome::Repaired);
  assert!(report.is_clean(), "failures: {:?}", report.failures);
  assert!(!incoming.exists(), "the shadow must be deleted");
  assert_eq!(
    snapshot(m.sources_dir()),
    before,
    "repair must not have touched the live clone either"
  );

  // The deletion is destructive, which is what the daemon logs loudly.
  assert!(
    report
      .destructive()
      .any(|a| matches!(a, Action::RemoveIncoming { .. })),
    "deleting a fetch must be reported as destructive"
  );
}

/// SC-Y-001, mid-promote half: a crash between the two renames leaves
/// the only copy parked aside, and repair puts it back rather than
/// deleting it.
#[test]
fn sc_y_001_a_crash_between_the_promote_renames_restores_the_parked_clone() {
  let m = Machine::new();
  let previous = previous_path(m.sources_dir());
  let head = m.repo_at(&previous, "parked");
  assert!(
    !m.sources_dir().exists(),
    "this is the one window where the sources dir is absent"
  );

  let findings = m.plan();
  assert!(
    findings
      .iter()
      .any(|f| matches!(f, Finding::SourcesDirParked { .. })),
    "a parked clone with no sources dir must be recognised: {findings:?}"
  );

  let report = m.repair();
  assert!(report.is_clean(), "failures: {:?}", report.failures);
  assert!(m.sources_dir().is_dir(), "the clone must be restored");
  assert_eq!(
    head_of(m.sources_dir()),
    head,
    "the restored clone must be the one that was parked"
  );
  assert!(!previous.exists(), "nothing is left parked");

  // Restoring is not destructive: it is the only copy of the operator's
  // config repo, so repair must never report it as something destroyed.
  assert!(
    !report
      .destructive()
      .any(|a| matches!(a, Action::RestorePrevious { .. })),
    "restoring the only copy must not be classed destructive"
  );
}

/// A `.previous` left over *beside a live sources dir* is the opposite
/// case: the promote finished and only its cleanup did not, so the
/// leftover is stale and is removed rather than restored.
#[test]
fn an_orphaned_previous_beside_a_live_clone_is_removed_not_restored() {
  let m = Machine::new();
  let live = m.repo_at(m.sources_dir(), "live");
  let previous = previous_path(m.sources_dir());
  m.repo_at(&previous, "stale");

  let findings = m.plan();
  assert!(
    findings
      .iter()
      .any(|f| matches!(f, Finding::OrphanedPrevious { .. })),
    "got {findings:?}"
  );

  m.repair();
  assert!(!previous.exists(), "the stale copy must be swept");
  assert_eq!(
    head_of(m.sources_dir()),
    live,
    "the live clone must be the one that survives"
  );
}

// ---------------------------------------------------------------------------
// SC-Y-002: killed between fetch success and the state write
// ---------------------------------------------------------------------------

/// SC-Y-002: a crash after the promote but before the state write
/// leaves `sources-head` naming the old sha; repair corrects the state
/// to what is on disk rather than moving the clone to match the state.
#[test]
fn sc_y_002_a_state_write_killed_after_the_promote_is_corrected_from_disk() {
  let m = Machine::new();
  let on_disk = m.repo_at(m.sources_dir(), "advanced");
  let stale = "0000000000000000000000000000000000000000";
  m.record("sources-head", stale);

  let findings = m.plan();
  let mismatch = findings
    .iter()
    .find_map(|f| {
      match f {
        Finding::SourcesHeadMismatch { recorded, on_disk } => {
          Some((recorded.clone(), on_disk.clone()))
        },
        _ => None,
      }
    })
    .expect("the mismatch must be found");
  assert_eq!(mismatch.0.as_deref(), Some(stale));
  assert_eq!(mismatch.1, on_disk);

  let report = m.repair();
  assert!(report.is_clean(), "failures: {:?}", report.failures);
  assert_eq!(
    fs::read_to_string(m.state_dir().join("sources-head"))
      .expect("read state")
      .trim(),
    on_disk,
    "state must be corrected to the sha the clone actually has"
  );
  assert_eq!(
    head_of(m.sources_dir()),
    on_disk,
    "repair must never move the clone to match a stale record"
  );
  assert!(
    !report
      .destructive()
      .any(|a| matches!(a, Action::CorrectSourcesHead { .. })),
    "correcting a record destroys nothing"
  );
}

/// SC-Y-002, producer half: the ordering that makes the above the only
/// possible mismatch.
///
/// `commit` promotes shadows before it writes state, so there is no
/// window in which the state names a sha that is not on disk. The
/// reverse mismatch - state ahead of the clone - is therefore not a
/// state a crash can produce, and repair does not have to handle it.
#[test]
fn sc_y_002_commit_never_lets_state_name_a_sha_that_is_not_on_disk() {
  let m = Machine::new();
  let incoming = incoming_path(m.sources_dir());
  let new_head = m.repo_at(&incoming, "fetched");

  let mut txn =
    Transaction::begin(m.state_dir(), "daemon", "tick").expect("begin");
  txn.promote(&incoming, m.sources_dir());
  txn.stage_state("sources-head", &new_head);

  // Mid-transaction: the shadow is populated and the state still
  // describes the previous, intact world.
  assert!(
    !m.state_dir().join("sources-head").exists(),
    "a staged write must not be on disk before commit"
  );
  assert!(
    m.state_dir().join(SENTINEL_NAME).is_file(),
    "the sentinel must be published while the transaction is open"
  );

  txn.commit().expect("commit");

  assert_eq!(head_of(m.sources_dir()), new_head);
  assert_eq!(
    fs::read_to_string(m.state_dir().join("sources-head"))
      .expect("read state")
      .trim(),
    new_head,
    "after commit the state and the clone agree"
  );
  assert!(
    !m.state_dir().join(SENTINEL_NAME).exists(),
    "a committed transaction leaves no sentinel to repair"
  );
  assert!(
    discover(&m.ctx).expect("discover").is_empty(),
    "a clean commit leaves nothing for repair to find"
  );
}

/// An abandoned transaction sentinel is cleared, and a live one is left
/// alone: clearing a running actor's sentinel would make repair the
/// cause of the next crash.
#[test]
fn an_abandoned_sentinel_is_cleared_and_a_live_one_is_not() {
  let m = Machine::new();
  m.plant_abandoned_sentinel("gitops-daemon", "fetch and activate");

  let findings = m.plan();
  let abandoned = findings
    .iter()
    .find_map(|f| {
      match f {
        Finding::AbandonedTransaction {
          actor, intent, pid, ..
        } => Some((actor.clone(), intent.clone(), *pid)),
        _ => None,
      }
    })
    .expect("the sentinel must be found");
  assert_eq!(abandoned.0, "gitops-daemon");
  assert_eq!(abandoned.1, "fetch and activate", "the intent is reported");
  assert_eq!(abandoned.2, DEAD_PID);

  m.repair();
  assert!(
    !m.state_dir().join(SENTINEL_NAME).exists(),
    "the abandoned sentinel must be gone"
  );

  // A live sentinel: this process really is running.
  let txn = Transaction::begin(m.state_dir(), "switch", "xi home switch")
    .expect("begin");
  let findings = m.plan();
  assert!(
    !findings
      .iter()
      .any(|f| matches!(f, Finding::AbandonedTransaction { .. })),
    "a live actor's sentinel must not be reported abandoned: {findings:?}"
  );
  txn.rollback().expect("rollback");
}

/// A half-written state file from a killed atomic write is swept, and a
/// live writer's temp file is not.
#[test]
fn a_dead_writers_temp_file_is_swept_and_a_live_writers_is_not() {
  let m = Machine::new();

  let dead = m.state_dir().join(format!("sources-head.tmp.{DEAD_PID}"));
  fs::write(&dead, b"half").expect("write dead temp");
  let live = temp_path_for(&m.state_dir().join("sources-head"));
  fs::write(&live, b"in flight").expect("write live temp");

  let findings = m.plan();
  let swept: Vec<&PathBuf> = findings
    .iter()
    .filter_map(|f| {
      match f {
        Finding::StaleTempFile { path, .. } => Some(path),
        _ => None,
      }
    })
    .collect();
  assert_eq!(swept, [&dead], "only the dead writer's temp is stale");

  m.repair();
  assert!(!dead.exists(), "the dead writer's leftover must be removed");
  assert!(
    live.exists(),
    "a running writer's temp file must survive, or repair corrupts the write \
     it is in the middle of"
  );
}

// ---------------------------------------------------------------------------
// SC-Y-003: receipts
// ---------------------------------------------------------------------------

/// SC-Y-003: a receipt lists exactly the phases that completed, so a
/// reversal undoes those and nothing else.
#[test]
fn sc_y_003_a_receipt_lists_exactly_the_completed_phases() {
  let m = Machine::new();

  Receipt::append(m.state_dir(), [
    ReceiptEntry::new("install-nix"),
    ReceiptEntry::new("clone-sources"),
  ])
  .expect("append");

  let receipt = Receipt::load(m.state_dir()).expect("load");
  assert!(receipt.records("install-nix"));
  assert!(receipt.records("clone-sources"));
  assert!(
    !receipt.records("switch"),
    "a phase that never ran must not be recorded, or the reversal would undo \
     work that was never done"
  );
}

/// SC-Y-003, ordering half: the receipt append is the last write a
/// commit performs before the sentinel is dropped.
///
/// That ordering is what makes an entry's presence proof the work
/// finished: an entry can only exist if every promote and every state
/// write ahead of it already succeeded.
#[test]
fn sc_y_003_a_staged_receipt_entry_lands_only_on_commit() {
  let m = Machine::new();

  let mut txn =
    Transaction::begin(m.state_dir(), "bootstrap", "phase 2").expect("begin");
  txn.stage_receipt(ReceiptEntry::new("install-nix"));
  assert!(
    !Receipt::load(m.state_dir())
      .expect("load")
      .records("install-nix"),
    "a staged entry must not be readable before commit"
  );

  txn.commit().expect("commit");
  assert!(
    Receipt::load(m.state_dir())
      .expect("load")
      .records("install-nix"),
    "commit must flush the staged entry"
  );

  // And an abandoned transaction records nothing.
  let mut txn =
    Transaction::begin(m.state_dir(), "bootstrap", "phase 3").expect("begin");
  txn.stage_receipt(ReceiptEntry::new("switch"));
  drop(txn);
  assert!(
    !Receipt::load(m.state_dir())
      .expect("load")
      .records("switch"),
    "a transaction that never committed must leave no receipt entry"
  );
}

/// Task 7.2: when the write-and-commit sequence was interrupted, repair
/// reconstructs the entry from on-disk evidence and flags it as
/// reconstructed rather than passing it off as an actor's own record.
#[test]
fn an_interrupted_receipt_write_is_reconstructed_from_evidence() {
  let m = Machine::new();

  // The physical effect is on disk: xi.conf exists and nix.conf
  // references it, which only happens if the amend completed.
  fs::write(m.nix_dir.join("xi.conf"), "extra-substituters =\n")
    .expect("write fragment");
  fs::write(m.nix_dir.join("nix.conf"), "!include xi.conf\n")
    .expect("write nix.conf");

  // In production `RepairContext::for_scope` derives this evidence
  // itself from exactly the two facts written above. It is supplied
  // here because this fixture's paths are injected rather than resolved
  // from the environment, so the derivation has nothing to read.
  let ctx = RepairContext {
    receipt_evidence: vec![xi_core::sources::repair::ReceiptEvidence {
      action:   WRITE_XI_CONF_ACTION.to_owned(),
      evidence: format!(
        "{} exists and is referenced by {}",
        m.nix_dir.join("xi.conf").display(),
        m.nix_dir.join("nix.conf").display()
      ),
    }],
    ..m.ctx.clone()
  };

  let findings = discover(&ctx).expect("discover").findings;
  assert!(
    findings.iter().any(|f| matches!(
      f,
      Finding::MissingReceiptEntry { action, .. } if action == WRITE_XI_CONF_ACTION
    )),
    "evidence without a receipt entry must be a finding: {findings:?}"
  );

  execute(&ctx, false).expect("repair");
  let receipt = Receipt::load(m.state_dir()).expect("load");
  assert!(
    receipt.records(WRITE_XI_CONF_ACTION),
    "repair must record what the evidence proves happened"
  );

  // Running again finds nothing: the entry is now there.
  assert!(
    !discover(&ctx)
      .expect("discover")
      .findings
      .iter()
      .any(|f| matches!(f, Finding::MissingReceiptEntry { .. })),
    "a reconstructed entry must not be reconstructed twice"
  );
}

// ---------------------------------------------------------------------------
// SC-Y-004: the dangling cache fragment
// ---------------------------------------------------------------------------

/// SC-Y-004: a prestage killed between writing the fragment and
/// amending `nix.conf` leaves a fragment nothing references, and repair
/// deletes it.
#[test]
fn sc_y_004_an_unreferenced_xi_conf_is_deleted() {
  let m = Machine::new();
  let xi_conf = m.nix_dir.join("xi.conf");
  fs::write(&xi_conf, "extra-substituters = https://cache.example.org\n")
    .expect("write fragment");
  fs::write(m.nix_dir.join("nix.conf"), "max-jobs = 4\n")
    .expect("write nix.conf");

  let findings = m.plan();
  assert!(
    findings
      .iter()
      .any(|f| matches!(f, Finding::UnreferencedXiConf { .. })),
    "got {findings:?}"
  );

  let report = m.repair();
  assert!(report.is_clean(), "failures: {:?}", report.failures);
  assert!(!xi_conf.exists(), "the orphaned fragment must be deleted");
  assert_eq!(
    fs::read_to_string(m.nix_dir.join("nix.conf")).expect("read"),
    "max-jobs = 4\n",
    "the operator's own nix.conf must be untouched"
  );
  assert!(
    report
      .destructive()
      .any(|a| matches!(a, Action::RemoveXiConf { .. })),
    "deleting the fragment is destructive and must be reported so"
  );
}

/// SC-Y-004, mirror case: an `!include` pointing at a fragment that is
/// not there would make every later `nix` invocation fail, so the
/// dangling line is stripped and the rest of the file kept.
#[test]
fn sc_y_004_an_include_with_no_fragment_is_stripped() {
  let m = Machine::new();
  fs::write(
    m.nix_dir.join("nix.conf"),
    "max-jobs = 4\n!include xi.conf\ncores = 0\n",
  )
  .expect("write nix.conf");

  let findings = m.plan();
  assert!(
    findings
      .iter()
      .any(|f| matches!(f, Finding::DanglingInclude { .. })),
    "got {findings:?}"
  );

  m.repair();
  let restored =
    fs::read_to_string(m.nix_dir.join("nix.conf")).expect("read nix.conf");
  assert_eq!(
    restored, "max-jobs = 4\ncores = 0\n",
    "the include goes, every other line stays"
  );
}

// ---------------------------------------------------------------------------
// SC-Y-005: the abandoned sync
// ---------------------------------------------------------------------------

/// SC-Y-005: a sync killed mid-copy leaves its sentinel; repair rolls
/// the working dir back to HEAD by default.
#[test]
fn sc_y_005_an_abandoned_sync_is_rolled_back_to_head() {
  let m = Machine::new();
  let working = m.root.join("working");
  m.repo_at(&working, "operator clone");
  m.record("working-dir", &working.display().to_string());

  // Mid-copy: a tracked file half-overwritten, and the sentinel.
  fs::write(working.join("flake.nix"), "# half-copied\n").expect("clobber");
  fs::write(m.state_dir().join(SYNC_SENTINEL_NAME), b"").expect("sentinel");

  let findings = m.plan();
  assert!(
    findings
      .iter()
      .any(|f| matches!(f, Finding::AbandonedSync { .. })),
    "got {findings:?}"
  );

  let report = m.repair();
  assert!(report.is_clean(), "failures: {:?}", report.failures);
  assert!(
    report.actions.iter().any(|a| {
      matches!(a, Action::ResolveSync {
        resolution: SyncResolution::RolledBack,
        ..
      })
    }),
    "the default resolution is a rollback: {:?}",
    report.actions
  );
  assert_eq!(
    fs::read_to_string(working.join("flake.nix")).expect("read"),
    "# operator clone\n",
    "the working dir must be back at HEAD"
  );
  assert!(
    !m.state_dir().join(SYNC_SENTINEL_NAME).exists(),
    "a resolved sync leaves no sentinel"
  );
}

/// SC-Y-005, resume half: with `resume_sync` the sentinel is left for
/// the sync actor and the working dir is not touched, because rolling
/// back would discard the copy the operator asked for.
#[test]
fn sc_y_005_a_sync_can_be_left_to_resume_instead_of_rolled_back() {
  let m = Machine::new();
  let working = m.root.join("working");
  m.repo_at(&working, "operator clone");
  m.record("working-dir", &working.display().to_string());
  fs::write(working.join("flake.nix"), "# half-copied\n").expect("clobber");
  fs::write(m.state_dir().join(SYNC_SENTINEL_NAME), b"").expect("sentinel");

  let ctx = RepairContext {
    resume_sync: true,
    ..m.ctx.clone()
  };
  let report = execute(&ctx, false).expect("repair");

  assert!(
    report.actions.iter().any(|a| {
      matches!(a, Action::ResolveSync {
        resolution: SyncResolution::LeftToResume,
        ..
      })
    }),
    "got {:?}",
    report.actions
  );
  assert_eq!(
    fs::read_to_string(working.join("flake.nix")).expect("read"),
    "# half-copied\n",
    "leaving a sync to resume must not revert the partial copy"
  );
  assert!(
    m.state_dir().join(SYNC_SENTINEL_NAME).exists(),
    "the sentinel is what the sync actor picks up"
  );
  assert!(
    !report.destructive().any(|a| {
      matches!(a, Action::ResolveSync {
        resolution: SyncResolution::LeftToResume,
        ..
      })
    }),
    "leaving a sync alone destroys nothing"
  );
}

// ---------------------------------------------------------------------------
// xi-xhsn: `conflict::sync` publishes and clears SC-Y-005's sentinel
// itself
// ---------------------------------------------------------------------------
//
// The two tests above construct the SC-Y-005 state by hand, which proves
// repair's half of the contract but not the sync actor's: nothing in
// `sources::conflict` wrote `SYNC_SENTINEL_NAME` before xi-xhsn. These
// drive the real `conflict::sync` entrypoint instead, so the sentinel
// existing (or not) is `sync`'s own behaviour, not a test fixture.

/// A completed sync leaves no sentinel behind: the claim/clear pair that
/// makes `AbandonedSync` mean something disappears on the success path
/// precisely because the copy reached its last step.
#[test]
fn a_completed_sync_leaves_no_sentinel_behind() {
  let m = Machine::new();
  m.repo_at(m.sources_dir(), "v1");
  let working = m.root.join("working");
  clone_repo(m.sources_dir(), &working);

  xi_core::sources::conflict::sync(&xi_core::sources::conflict::SyncRequest {
    sources_dir: m.sources_dir(),
    working_dir: &working,
    state_dir:   m.state_dir(),
    force_dirty: false,
    what:        "test sync",
  })
  .expect("sync");

  assert!(
    !m.state_dir().join(SYNC_SENTINEL_NAME).exists(),
    "a completed sync must clear the sentinel it published"
  );
  assert!(
    !m.plan()
      .iter()
      .any(|f| matches!(f, Finding::AbandonedSync { .. })),
    "a machine right after a clean sync must have no abandoned sync to \
     repair: {:?}",
    m.plan()
  );
}

/// The acceptance criterion behind xi-xhsn for the `sync` actor: a crash
/// mid-copy (simulated here by a blocked write, for the same reason
/// `switch::tests::a_kill_before_commit_leaves_an_abandoned_transaction_
/// naming_switch` uses `mem::forget` rather than a real SIGKILL - see
/// that module's doc comment) leaves the sentinel `conflict::sync`
/// itself published, naming the real working dir, and `xi sources
/// repair` clears it exactly as SC-Y-005 already proves for a
/// hand-planted one.
#[test]
fn a_sync_that_fails_mid_copy_leaves_a_real_abandoned_sync_finding() {
  let m = Machine::new();
  m.repo_at(m.sources_dir(), "v1");
  let working = m.root.join("working");
  clone_repo(m.sources_dir(), &working);

  // Block the one tracked file's write: a directory sitting where
  // `write_pass` wants to write a plain file. `inspect_working_tree`
  // then sees `flake.nix` as deleted, so `force_dirty` is needed to
  // reach the copy at all - this is standing in for "the operator
  // already had local edits", not for the failure itself.
  fs::remove_file(working.join("flake.nix")).expect("remove flake.nix");
  fs::create_dir_all(working.join("flake.nix")).expect("block the write");

  let err = xi_core::sources::conflict::sync(
    &xi_core::sources::conflict::SyncRequest {
      sources_dir: m.sources_dir(),
      working_dir: &working,
      state_dir:   m.state_dir(),
      force_dirty: true,
      what:        "test sync",
    },
  )
  .expect_err("a blocked write must fail the sync");
  assert!(
    !err.to_string().is_empty(),
    "the failure should at least say something"
  );

  assert!(
    m.state_dir().join(SYNC_SENTINEL_NAME).exists(),
    "sync must have published its own sentinel before the blocked write, not \
     relied on a test to plant one"
  );

  let findings = m.plan();
  let recorded_working_dir = findings.iter().find_map(|f| {
    match f {
      Finding::AbandonedSync { working_dir, .. } => Some(working_dir.clone()),
      _ => None,
    }
  });
  assert_eq!(
    recorded_working_dir,
    Some(Some(working.clone())),
    "repair must be able to name the real working dir sync was mid-copy into, \
     not just that a sync was abandoned: {findings:?}"
  );

  let report = m.repair();
  assert!(report.is_clean(), "failures: {:?}", report.failures);
  assert!(
    !m.state_dir().join(SYNC_SENTINEL_NAME).exists(),
    "repair clears the sentinel the failed sync left behind"
  );
}

// ---------------------------------------------------------------------------
// SC-Y-006: the dry run
// ---------------------------------------------------------------------------

/// SC-Y-006: a dry run prints the plan and changes nothing.
#[test]
fn sc_y_006_a_dry_run_plans_every_action_and_executes_none() {
  let m = Machine::new();

  // One of each kind repair can find, so the dry run has a real plan.
  m.repo_at(m.sources_dir(), "live");
  m.repo_at(&incoming_path(m.sources_dir()), "half-fetched");
  m.record("sources-head", "0000000000000000000000000000000000000000");
  m.plant_abandoned_sentinel("daemon", "tick");
  fs::write(m.nix_dir.join("xi.conf"), "extra-substituters =\n")
    .expect("write fragment");
  fs::write(m.nix_dir.join("nix.conf"), "max-jobs = 4\n").expect("write conf");

  let before = snapshot(&m.root);
  let report = m.dry_run();

  assert_eq!(report.outcome, Outcome::Planned);
  assert!(report.dry_run);
  assert_eq!(report.schema_version, REPAIR_SCHEMA_VERSION);
  assert_eq!(
    report.actions.len(),
    report.findings.len(),
    "the plan names one action per finding"
  );
  assert!(
    report.actions.len() >= 4,
    "the fixture should produce several findings, got {:?}",
    report.findings
  );
  assert!(report.failures.is_empty(), "a plan cannot fail");

  assert_eq!(
    snapshot(&m.root),
    before,
    "a dry run must not have changed a single byte"
  );

  // The envelope serialises, since it is what the verb prints.
  let json = serde_json::to_string(&report).expect("serialise");
  assert!(json.contains("\"dry_run\":true"), "got {json}");
}

// ---------------------------------------------------------------------------
// SC-Y-007 / SC-Y-008: idempotence, and the daemon's first action
// ---------------------------------------------------------------------------

/// SC-Y-007: a second run with no new damage finds nothing.
#[test]
fn sc_y_007_repair_is_idempotent() {
  let m = Machine::new();
  m.repo_at(m.sources_dir(), "live");
  m.repo_at(&incoming_path(m.sources_dir()), "half-fetched");
  m.plant_abandoned_sentinel("daemon", "tick");
  fs::write(m.nix_dir.join("xi.conf"), "extra-substituters =\n")
    .expect("write fragment");
  fs::write(m.nix_dir.join("nix.conf"), "max-jobs = 4\n").expect("write conf");

  let first = m.repair();
  assert_eq!(first.outcome, Outcome::Repaired);
  assert!(!first.actions.is_empty());
  assert!(first.is_clean(), "failures: {:?}", first.failures);

  let after_first = snapshot(&m.root);

  let second = m.repair();
  assert_eq!(
    second.outcome,
    Outcome::NoOp,
    "the second run must find nothing: {:?}",
    second.findings
  );
  assert!(second.findings.is_empty());
  assert!(second.actions.is_empty());
  assert_eq!(
    snapshot(&m.root),
    after_first,
    "a no-op run must not change anything either"
  );

  // Third run, to catch a repair that oscillates rather than settles.
  assert_eq!(m.repair().outcome, Outcome::NoOp);
}

/// SC-Y-007, clean-machine half: an untouched machine is a no-op, so
/// the daemon's startup repair is silent on the normal path.
#[test]
fn sc_y_007_an_untouched_machine_needs_no_repair() {
  let m = Machine::new();
  m.repo_at(m.sources_dir(), "live");
  m.record("sources-head", &head_of(m.sources_dir()));

  let report = m.repair();
  assert_eq!(report.outcome, Outcome::NoOp);
  assert!(report.findings.is_empty(), "got {:?}", report.findings);

  // And a machine that was never adopted at all: no sources dir, no
  // state, no nix.conf. Every one of those is a legitimate state, not
  // damage to report.
  let fresh = Machine::new();
  assert_eq!(fresh.repair().outcome, Outcome::NoOp);
}

/// SC-Y-008: repair runs inside its own transaction, so a crash of
/// repair is itself recoverable, and it leaves no sentinel behind on
/// the way out.
///
/// The "first action on startup" half belongs to the daemon's main,
/// which does not exist yet (see xi-auj); `auto_repair` is the entry
/// point it will call, and its contract is asserted here.
#[test]
fn sc_y_008_repair_leaves_no_transaction_of_its_own_behind() {
  let m = Machine::new();
  m.repo_at(m.sources_dir(), "live");
  m.repo_at(&incoming_path(m.sources_dir()), "half-fetched");

  let report = m.repair();
  assert_eq!(report.outcome, Outcome::Repaired);
  assert!(
    !m.state_dir().join(SENTINEL_NAME).exists(),
    "repair's own transaction must be committed, not left open"
  );
  assert!(
    discover(&m.ctx).expect("discover").is_empty(),
    "repair must not leave anything for the next run, including its own \
     bookkeeping"
  );
}

/// An actor's abandoned sentinel is cleared *before* repair opens its
/// own, so the report names the dead actor rather than repair itself.
///
/// Getting this backwards would make every repair report claim it had
/// rolled back a transaction belonging to "repair", which is
/// indistinguishable from its own bookkeeping and tells the operator
/// nothing.
#[test]
fn the_report_names_the_dead_actor_not_repair_itself() {
  let m = Machine::new();
  m.plant_abandoned_sentinel("gitops-daemon", "fetch and activate");

  let report = m.repair();
  let rolled: Vec<&str> = report
    .actions
    .iter()
    .filter_map(|a| {
      match a {
        Action::RollbackTransaction { actor, .. } => Some(actor.as_str()),
        _ => None,
      }
    })
    .collect();
  assert_eq!(rolled, ["gitops-daemon"], "got {:?}", report.actions);
  assert!(
    !rolled.contains(&"repair"),
    "repair must not report its own sentinel as a finding"
  );
}

// ---------------------------------------------------------------------------
// Task 8.4: the atomic write, seen from a concurrent reader
// ---------------------------------------------------------------------------

/// Task 8.4: a reader racing `write_atomic` never observes a partial
/// file.
///
/// This is the property the whole state dir rests on: readers take no
/// lock (`xi-sources-race` is explicit that only mutators do), so the
/// only thing standing between a reader and a half-written sha is that
/// the write lands by `rename`. A writer that truncated in place would
/// pass every single-threaded test in this file and fail here.
#[test]
fn task_8_4_a_concurrent_reader_never_sees_a_partial_state_file() {
  use std::{
    sync::{
      Arc,
      atomic::{AtomicBool, Ordering},
    },
    thread,
  };

  use xi_core::sources::atomic::write_atomic;

  let tmp = TempDir::new().expect("tempdir");
  let path = tmp.path().join("sources-head");

  // Two shas of different lengths, so a torn read is not merely
  // possible but obvious: a truncate-then-write would be caught
  // mid-flight as a prefix of the longer one.
  let short = "a".repeat(40);
  let long = "b".repeat(400);
  write_atomic(&path, short.as_bytes(), Some(0o600)).expect("seed");

  let stop = Arc::new(AtomicBool::new(false));
  let reader_stop = Arc::clone(&stop);
  let reader_path = path.clone();
  let (short_seen, long_seen) = (short.clone(), long.clone());

  let reader = thread::spawn(move || {
    let mut reads = 0_u32;
    while !reader_stop.load(Ordering::Relaxed) {
      // A missing file would also be a torn observation: `rename` is
      // atomic, so the path is never absent once it exists.
      let seen = fs::read_to_string(&reader_path)
        .expect("the path must never be absent mid-write");
      assert!(
        seen == short_seen || seen == long_seen,
        "a reader observed a partial file of {} bytes",
        seen.len()
      );
      reads += 1;
    }
    reads
  });

  for i in 0..200 {
    let body = if i % 2 == 0 { &long } else { &short };
    write_atomic(&path, body.as_bytes(), Some(0o600)).expect("write");
  }

  stop.store(true, Ordering::Relaxed);
  let reads = reader.join().expect("reader thread");
  assert!(
    reads > 0,
    "the reader never ran, so this proves nothing about concurrency"
  );
}

// ---------------------------------------------------------------------------
// Task 4.2: every state-dir write goes through the atomic helper
// ---------------------------------------------------------------------------

/// Task 4.2: a killed state-dir write leaves a leftover repair can
/// recognise and sweep.
///
/// The naming convention is the whole mechanism: `atomic::write_atomic`
/// writes `<name>.tmp.<pid>`, and both repair's sweep
/// (`atomic::temp_owner_pid`) and the model's unsanctioned-file guard
/// (`atomic::is_temp_name`) key off that infix. A writer that rolls its
/// own temp name is invisible to both, so its leftover is unsweepable
/// *and* degrades the status envelope forever.
#[test]
fn task_4_2_every_state_dir_writer_uses_the_sweepable_temp_name() {
  use xi_core::sources::{atomic::is_temp_name, bootstrap};

  let m = Machine::new();

  // The bootstrap receipt is a state-dir file at user scope, so its
  // writer is bound by this rule like every other.
  let receipt = m.state_dir().join(bootstrap::RECEIPT_FILE);
  bootstrap::Receipt::new(Scope::User)
    .save(&receipt)
    .expect("save the bootstrap receipt");

  // Whatever temp name that writer used must be one repair can sweep.
  let leftovers: Vec<String> = fs::read_dir(m.state_dir())
    .expect("read state dir")
    .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
    .filter(|name| name.contains(".tmp"))
    .collect();
  for name in &leftovers {
    assert!(
      is_temp_name(name),
      "`{name}` is a temp file repair cannot recognise, so a crash mid-write \
       would leave it on disk forever"
    );
  }

  // And the receipt itself must be a sanctioned state-dir name, or a
  // machine that completed a bootstrap reports Degraded for having done
  // so.
  let unknown = xi_core::sources::unknown_state_files(m.state_dir())
    .expect("list unknown state files");
  assert!(
    unknown.is_empty(),
    "a completed bootstrap must not make the state dir look corrupt: \
     {unknown:?}"
  );
}

/// A malformed sentinel is cleared rather than refused: refusing to
/// start is the one outcome that would make the damage permanent.
#[test]
fn a_corrupt_sentinel_is_clearable_rather_than_fatal() {
  let m = Machine::new();
  fs::write(m.state_dir().join(SENTINEL_NAME), b"{not json")
    .expect("write corrupt sentinel");

  let loaded = Sentinel::load(m.state_dir()).expect("load must not fail");
  assert!(
    loaded.is_some(),
    "a corrupt sentinel is present-but-unreadable, not absent"
  );

  let report = m.repair();
  assert!(report.is_clean(), "failures: {:?}", report.failures);
  assert!(
    !m.state_dir().join(SENTINEL_NAME).exists(),
    "a corrupt sentinel must be clearable"
  );
}

// ---------------------------------------------------------------------------
// Task 8.5: idempotence as a property, not as one fixture
// ---------------------------------------------------------------------------

use proptest::prelude::*;

proptest! {
  // Each case builds up to three real git repos and runs repair three
  // times, so the count is a deliberate trade: enough samples of the
  // 128-combination space to catch an interaction, few enough that the
  // whole workspace suite stays in single-digit seconds.
  #![proptest_config(ProptestConfig::with_cases(24))]

  /// Task 8.5 / SC-Y-007: for *any* combination of the damage repair
  /// knows how to find, one run settles the machine and the next finds
  /// nothing.
  ///
  /// The scenarios above each pick one combination. This picks from all
  /// 128, which is what catches an interaction: a repair that is
  /// idempotent for each kind of damage alone but oscillates when two
  /// are present would pass every test above. The parked-clone case is
  /// the obvious candidate, because its action renames the very
  /// directory the other findings are predicted against.
  #[test]
  fn task_8_5_repair_is_idempotent_for_any_combination_of_damage(
    live_clone in proptest::bool::ANY,
    incoming in proptest::bool::ANY,
    previous in proptest::bool::ANY,
    sentinel in proptest::bool::ANY,
    stale_temp in proptest::bool::ANY,
    head_mismatch in proptest::bool::ANY,
    orphan_fragment in proptest::bool::ANY,
  ) {
    let m = Machine::new();

    if live_clone {
      m.repo_at(m.sources_dir(), "live");
    }
    if incoming {
      m.repo_at(&incoming_path(m.sources_dir()), "half-fetched");
    }
    if previous {
      m.repo_at(&previous_path(m.sources_dir()), "parked");
    }
    if sentinel {
      m.plant_abandoned_sentinel("daemon", "tick");
    }
    if stale_temp {
      fs::write(
        m.state_dir().join(format!("sources-head.tmp.{DEAD_PID}")),
        b"half",
      )
      .expect("write stale temp");
    }
    if head_mismatch {
      m.record("sources-head", "0000000000000000000000000000000000000000");
    }
    if orphan_fragment {
      fs::write(m.nix_dir.join("xi.conf"), "extra-substituters =\n")
        .expect("write fragment");
      fs::write(m.nix_dir.join("nix.conf"), "max-jobs = 4\n")
        .expect("write nix.conf");
    }

    let first = execute(&m.ctx, false).expect("first repair");
    prop_assert!(
      first.is_clean(),
      "repair reported failures: {:?}",
      first.failures
    );

    let second = execute(&m.ctx, false).expect("second repair");
    prop_assert_eq!(
      second.outcome,
      Outcome::NoOp,
      "a second run found {:?} after doing {:?}",
      second.findings,
      first.actions
    );

    // Stable, not merely quiet: a third run must agree, and nothing may
    // have changed on disk between the second and the third.
    let after_second = snapshot(&m.root);
    prop_assert_eq!(
      execute(&m.ctx, false).expect("third repair").outcome,
      Outcome::NoOp
    );
    prop_assert!(snapshot(&m.root) == after_second, "repair oscillates");
  }
}
