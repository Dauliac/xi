//! `xi <scope> sources repair`: fix what a crash left behind, offline.
//!
//! Part of the `xi-sources-crash-recovery` capability
//! (`openspec/specs/xi-sources-crash-recovery/spec.md`, requirements
//! "`xi sources repair` verb" and "Daemon auto-repair on startup").
//!
//! Repair is deliberately three separate passes:
//!
//! 1. [`discover`]  -  read the world, decide nothing, touch nothing.
//! 2. [`RepairPlan::actions`]  -  the repairs those findings imply, which is
//!    all `--dry-run` prints.
//! 3. [`execute`]  -  carry them out and report.
//!
//! Splitting discovery from execution is what makes `--dry-run` honest:
//! it is the same code path minus the last pass, not a second
//! implementation that can drift from the real one. It is also what
//! makes repair idempotent, which matters because the gitops daemon runs
//! it on every startup: a second run over an already-repaired machine
//! discovers nothing and reports [`Outcome::NoOp`].
//!
//! Repair never touches the network. Everything it fixes is a local
//! inconsistency, and the one place it could be tempted to re-fetch
//! (a sources dir whose HEAD disagrees with the recorded sha) is
//! resolved the other way round: the on-disk HEAD is the ground truth
//! and the state file is corrected to match it. State describes
//! reality; it does not define it.

use std::{
  fs,
  io,
  path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{
  Scope,
  Sha,
  atomic::{self, write_atomic},
  config::ScopePaths,
  read_state_file,
  receipt::{Receipt, ReceiptEntry},
  shadow::{head_sha, incoming_path, previous_path, remove_if_present},
  transaction::{SENTINEL_NAME, Sentinel, Transaction, pid_is_live},
};

/// Schema version of the repair report envelope.
pub const REPAIR_SCHEMA_VERSION: u32 = 1;

/// Sentinel a sync run leaves while the working dir is mid-copy.
///
/// **Seam.** The sync actor itself is owned by `xi-sources-conflict`,
/// which lands in parallel. This capability owns the name, the fact that
/// the file lives in the state dir, and what repair does when it finds
/// one left behind.
pub const SYNC_SENTINEL_NAME: &str = "sync.in-progress";

/// Receipt action recorded by `xi-sources-cache-prestage` when it writes
/// the `xi.conf` fragment and references it from `nix.conf`.
pub const WRITE_XI_CONF_ACTION: &str = "write-xi-conf";

// ---------------------------------------------------------------------------
// Findings  -  what discovery saw
// ---------------------------------------------------------------------------

/// One piece of recoverable damage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "finding", rename_all = "kebab-case")]
pub enum Finding {
  /// A fetch was killed before it could be promoted.
  DanglingIncoming {
    /// The `<sources_dir>.incoming` path.
    path: PathBuf,
  },
  /// The sources dir was parked aside and never replaced: the crash
  /// landed between the two renames of the promote.
  SourcesDirParked {
    /// The `<sources_dir>.previous` holding the only copy.
    previous:    PathBuf,
    /// Where it belongs.
    sources_dir: PathBuf,
  },
  /// A promote finished but its cleanup did not.
  OrphanedPrevious {
    /// The `<sources_dir>.previous` path left over.
    path: PathBuf,
  },
  /// A transaction sentinel whose owning process is gone.
  AbandonedTransaction {
    /// Which actor opened it.
    actor:      String,
    /// What it said it was doing.
    intent:     String,
    /// The pid that is no longer running.
    pid:        u32,
    /// When it opened.
    started_at: String,
  },
  /// A half-written state-dir file from a killed atomic write.
  StaleTempFile {
    /// The leftover `<name>.tmp.<pid>` path.
    path: PathBuf,
    /// The dead pid that was writing it.
    pid:  u32,
  },
  /// `sources-head` disagrees with the clone it claims to describe.
  SourcesHeadMismatch {
    /// What the state dir says, if anything.
    recorded: Option<Sha>,
    /// What the clone's `HEAD` actually resolves to.
    on_disk:  Sha,
  },
  /// `nix.conf` references an `xi.conf` that is not there.
  DanglingInclude {
    /// The `nix.conf` carrying the reference.
    nix_conf: PathBuf,
    /// The file it points at, which does not exist.
    missing:  PathBuf,
  },
  /// An `xi.conf` fragment nothing references.
  UnreferencedXiConf {
    /// The orphaned fragment.
    path:     PathBuf,
    /// The `nix.conf` that should have referenced it.
    nix_conf: PathBuf,
  },
  /// A sync run left its sentinel behind.
  AbandonedSync {
    /// The sentinel path.
    path:        PathBuf,
    /// The working dir it was reconciling, if one is recorded.
    working_dir: Option<PathBuf>,
  },
  /// An action whose physical effect is on disk but whose receipt entry
  /// was never written.
  MissingReceiptEntry {
    /// The action name, in the owning capability's vocabulary.
    action:   String,
    /// What on disk proves it happened.
    evidence: String,
  },
}

// ---------------------------------------------------------------------------
// Actions  -  what repair would do
// ---------------------------------------------------------------------------

/// How repair settled an abandoned sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncResolution {
  /// `git reset --hard HEAD` in the working dir, sentinel removed.
  RolledBack,
  /// Sentinel left in place for the sync actor to pick up.
  LeftToResume,
}

/// One repair, named so the dry-run output reads as a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
  /// Delete the dangling shadow fetch.
  RemoveIncoming {
    /// The path to delete.
    path: PathBuf,
  },
  /// Rename `<sources_dir>.previous` back to `<sources_dir>`.
  RestorePrevious {
    /// The parked directory.
    from: PathBuf,
    /// Where it is restored to.
    to:   PathBuf,
  },
  /// Delete a `.previous` left over from a finished promote.
  RemovePrevious {
    /// The path to delete.
    path: PathBuf,
  },
  /// Clear a sentinel whose owner is dead.
  RollbackTransaction {
    /// Which actor left it.
    actor: String,
    /// The dead pid.
    pid:   u32,
  },
  /// Delete a half-written atomic-write temp file.
  RemoveStaleTemp {
    /// The path to delete.
    path: PathBuf,
  },
  /// Rewrite `sources-head` to the clone's real `HEAD`.
  CorrectSourcesHead {
    /// What the state dir said.
    from: Option<Sha>,
    /// What it will say.
    to:   Sha,
  },
  /// Strip the `!include` line pointing at a missing file.
  RemoveInclude {
    /// The `nix.conf` to rewrite.
    nix_conf: PathBuf,
    /// The missing target whose lines are dropped.
    missing:  PathBuf,
  },
  /// Delete an `xi.conf` nothing references.
  RemoveXiConf {
    /// The fragment to delete.
    path: PathBuf,
  },
  /// Settle an abandoned sync.
  ResolveSync {
    /// The sentinel involved.
    path:       PathBuf,
    /// How it was settled.
    resolution: SyncResolution,
  },
  /// Add back a receipt entry inferred from on-disk evidence.
  ReconstructReceiptEntry {
    /// The receipt action name to record. Spelled `receipt_action`
    /// rather than `action` because `action` is this enum's own
    /// serde tag and serde refuses the collision.
    receipt_action: String,
  },
}

impl Action {
  /// Whether carrying this out destroys something that was on disk.
  ///
  /// Used by [`auto_repair`] to log a WARN rather than a DEBUG line:
  /// the daemon deleting a half-finished fetch on startup is correct
  /// but it is also the operator's only chance to notice that a fetch
  /// was killed, so it must not be silent.
  #[must_use]
  pub const fn is_destructive(&self) -> bool {
    matches!(
      self,
      Self::RemoveIncoming { .. }
        | Self::RemovePrevious { .. }
        | Self::RemoveStaleTemp { .. }
        | Self::RemoveInclude { .. }
        | Self::RemoveXiConf { .. }
        | Self::ResolveSync {
          resolution: SyncResolution::RolledBack,
          ..
        }
    )
  }
}

/// The repair that one finding implies.
#[must_use]
fn action_for(finding: &Finding, resume_sync: bool) -> Action {
  match finding {
    Finding::DanglingIncoming { path } => {
      Action::RemoveIncoming { path: path.clone() }
    },
    Finding::SourcesDirParked {
      previous,
      sources_dir,
    } => {
      Action::RestorePrevious {
        from: previous.clone(),
        to:   sources_dir.clone(),
      }
    },
    Finding::OrphanedPrevious { path } => {
      Action::RemovePrevious { path: path.clone() }
    },
    Finding::AbandonedTransaction { actor, pid, .. } => {
      Action::RollbackTransaction {
        actor: actor.clone(),
        pid:   *pid,
      }
    },
    Finding::StaleTempFile { path, .. } => {
      Action::RemoveStaleTemp { path: path.clone() }
    },
    Finding::SourcesHeadMismatch { recorded, on_disk } => {
      Action::CorrectSourcesHead {
        from: recorded.clone(),
        to:   on_disk.clone(),
      }
    },
    Finding::DanglingInclude { nix_conf, missing } => {
      Action::RemoveInclude {
        nix_conf: nix_conf.clone(),
        missing:  missing.clone(),
      }
    },
    Finding::UnreferencedXiConf { path, .. } => {
      Action::RemoveXiConf { path: path.clone() }
    },
    Finding::AbandonedSync { path, .. } => {
      Action::ResolveSync {
        path:       path.clone(),
        resolution: if resume_sync {
          SyncResolution::LeftToResume
        } else {
          SyncResolution::RolledBack
        },
      }
    },
    Finding::MissingReceiptEntry { action, .. } => {
      Action::ReconstructReceiptEntry {
        receipt_action: action.clone(),
      }
    },
  }
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// On-disk evidence that an action in a receipt's vocabulary happened.
///
/// **Seam.** `xi-sources-bootstrap` and `xi-sources-cache-prestage` own
/// the action names and know how to prove each one. They land in
/// parallel, so [`RepairContext::for_scope`] supplies only the evidence
/// this capability can gather on its own; those capabilities extend the
/// list by pushing onto [`RepairContext::receipt_evidence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptEvidence {
  /// The receipt action this evidence is for.
  pub action:   String,
  /// What was observed, for the report.
  pub evidence: String,
}

/// Everything repair looks at, so every path is injectable in tests.
#[derive(Debug, Clone)]
pub struct RepairContext {
  /// Which half of the machine is being repaired.
  pub scope:            Scope,
  /// The managed clone.
  pub sources_dir:      PathBuf,
  /// The flat metadata directory.
  pub state_dir:        PathBuf,
  /// The `nix.conf` whose `!include` line is checked, if any.
  pub nix_conf:         Option<PathBuf>,
  /// The `xi.conf` fragment that include should point at, if any.
  pub xi_conf:          Option<PathBuf>,
  /// Receipt actions whose physical effect is observable on disk.
  pub receipt_evidence: Vec<ReceiptEvidence>,
  /// Leave an abandoned sync for the sync actor instead of rolling it
  /// back. See [`SyncResolution`].
  pub resume_sync:      bool,
}

impl RepairContext {
  /// The context for `scope`, resolved against the process environment.
  #[must_use]
  pub fn for_scope(scope: Scope) -> Self {
    Self::for_scope_with(scope, &|key| std::env::var(key).ok())
  }

  /// The context for `scope`, resolved against an arbitrary environment.
  ///
  /// `nix.conf` lives under the same configuration root as this scope's
  /// `sources.toml`  -  `/etc` for the system scope, the XDG config home
  /// for the user scope  -  so it is derived from [`ScopePaths`] rather
  /// than resolving XDG a second time and risking the two disagreeing.
  #[must_use]
  pub fn for_scope_with(
    scope: Scope,
    lookup: &dyn Fn(&str) -> Option<String>,
  ) -> Self {
    let paths = ScopePaths::resolve_with(scope, lookup);
    let nix_dir = paths
      .config_file
      .parent()
      .and_then(Path::parent)
      .map(|root| root.join("nix"));

    let mut context = Self {
      scope,
      sources_dir: paths.sources_dir,
      state_dir: paths.state_dir,
      nix_conf: nix_dir.as_ref().map(|dir| dir.join("nix.conf")),
      xi_conf: nix_dir.map(|dir| dir.join("xi.conf")),
      receipt_evidence: Vec::new(),
      resume_sync: false,
    };
    context.receipt_evidence = context.builtin_receipt_evidence();
    context
  }

  /// The one receipt action this capability can prove on its own.
  ///
  /// A referenced, existing `xi.conf` is proof that cache-prestage got
  /// as far as amending `nix.conf`; if the receipt does not say so, the
  /// write-and-commit sequence was interrupted and `xi system uninstall`
  /// would leave the fragment behind.
  fn builtin_receipt_evidence(&self) -> Vec<ReceiptEvidence> {
    let Some((nix_conf, xi_conf)) =
      self.nix_conf.as_deref().zip(self.xi_conf.as_deref())
    else {
      return Vec::new();
    };
    if !xi_conf.is_file() || !references(nix_conf, xi_conf) {
      return Vec::new();
    }
    vec![ReceiptEvidence {
      action:   WRITE_XI_CONF_ACTION.to_owned(),
      evidence: format!(
        "{} exists and is referenced by {}",
        xi_conf.display(),
        nix_conf.display()
      ),
    }]
  }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// The findings and the repairs they imply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPlan {
  /// What discovery saw, in the order it was found.
  pub findings: Vec<Finding>,
  /// The repair each finding implies, index for index.
  pub actions:  Vec<Action>,
}

impl RepairPlan {
  /// True when there is nothing to do.
  #[must_use]
  pub const fn is_empty(&self) -> bool {
    self.findings.is_empty()
  }
}

/// Read the world and decide what is broken, without changing anything.
///
/// # Errors
///
/// Propagates I/O errors from the state-dir and receipt reads. A
/// missing state dir, sources dir, or `nix.conf` is not an error: all
/// three are legitimate states on a machine that was never adopted.
pub fn discover(ctx: &RepairContext) -> io::Result<RepairPlan> {
  let mut findings = Vec::new();

  // The sentinel first, so the report reads in the order an operator
  // would reason: who died, then what they left behind.
  if let Some(sentinel) = Sentinel::load(&ctx.state_dir)?
    && !sentinel.owner_is_live()
  {
    findings.push(Finding::AbandonedTransaction {
      actor:      sentinel.actor,
      intent:     sentinel.intent,
      pid:        sentinel.pid,
      started_at: sentinel.started_at,
    });
  }

  let incoming = incoming_path(&ctx.sources_dir);
  if incoming.exists() {
    findings.push(Finding::DanglingIncoming { path: incoming });
  }

  // A `.previous` means a promote was interrupted. Which half depends
  // on whether the sources dir is back yet: absent, and `.previous`
  // holds the only copy and must be restored; present, and the promote
  // finished and only its cleanup did not.
  let previous = previous_path(&ctx.sources_dir);
  let restoring = previous.exists() && !ctx.sources_dir.exists();
  if previous.exists() {
    findings.push(if restoring {
      Finding::SourcesDirParked {
        previous:    previous.clone(),
        sources_dir: ctx.sources_dir.clone(),
      }
    } else {
      Finding::OrphanedPrevious {
        path: previous.clone(),
      }
    });
  }

  findings.extend(stale_temp_files(&ctx.state_dir)?);

  // Predict against the directory that *will* be the sources dir once
  // the restore above runs, so one pass produces a plan that is
  // internally consistent and a second pass finds nothing.
  let effective = if restoring {
    previous
  } else {
    ctx.sources_dir.clone()
  };
  if let Some(mismatch) = sources_head_mismatch(&ctx.state_dir, &effective)? {
    findings.push(mismatch);
  }

  findings.extend(include_findings(ctx));

  let sync = ctx.state_dir.join(SYNC_SENTINEL_NAME);
  if sync.exists() {
    findings.push(Finding::AbandonedSync {
      path:        sync,
      working_dir: read_state_file(&ctx.state_dir, "working-dir")?
        .map(PathBuf::from),
    });
  }

  let receipt = Receipt::load(&ctx.state_dir)?;
  for evidence in &ctx.receipt_evidence {
    if !receipt.records(&evidence.action) {
      findings.push(Finding::MissingReceiptEntry {
        action:   evidence.action.clone(),
        evidence: evidence.evidence.clone(),
      });
    }
  }

  let actions = findings
    .iter()
    .map(|finding| action_for(finding, ctx.resume_sync))
    .collect();
  Ok(RepairPlan { findings, actions })
}

/// Temp files in the state dir whose writer is gone.
///
/// A temp file belonging to a *live* pid is a write in flight, not
/// damage: deleting it would make a concurrent actor's atomic write
/// fail. Only a dead owner's leftover is swept.
fn stale_temp_files(state_dir: &Path) -> io::Result<Vec<Finding>> {
  let entries = match fs::read_dir(state_dir) {
    Ok(entries) => entries,
    Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(err) => return Err(err),
  };

  let mut found: Vec<Finding> = entries
    .filter_map(|entry| {
      let entry = entry.ok()?;
      let name = entry.file_name().to_string_lossy().into_owned();
      let pid = atomic::temp_owner_pid(&name)?;
      (!pid_is_live(pid)).then(|| {
        Finding::StaleTempFile {
          path: entry.path(),
          pid,
        }
      })
    })
    .collect();
  found.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
  Ok(found)
}

/// `sources-head` against the clone it claims to describe.
///
/// A sources dir that is not a git repo yet, or has no commit, yields
/// no finding: there is nothing to compare against, and inventing a
/// repair from an absence is how a bug becomes data loss.
fn sources_head_mismatch(
  state_dir: &Path,
  sources_dir: &Path,
) -> io::Result<Option<Finding>> {
  if !sources_dir.join(".git").exists() {
    return Ok(None);
  }
  let on_disk = match head_sha(sources_dir) {
    Ok(sha) => sha,
    Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(err) => return Err(err),
  };
  let recorded = read_state_file(state_dir, "sources-head")?;
  Ok(
    (recorded.as_deref() != Some(on_disk.as_str()))
      .then_some(Finding::SourcesHeadMismatch { recorded, on_disk }),
  )
}

/// The two ways an `xi.conf` and its `nix.conf` reference can disagree.
fn include_findings(ctx: &RepairContext) -> Vec<Finding> {
  let Some(nix_conf) = ctx.nix_conf.as_deref() else {
    return Vec::new();
  };
  if !nix_conf.is_file() {
    return Vec::new();
  }

  let mut findings = Vec::new();

  // A reference to a file that is not there: cache-prestage was killed
  // after amending nix.conf, or the fragment was deleted by hand. Nix
  // itself fails to start on a dangling `!include`, so this one is not
  // cosmetic.
  for target in include_targets(nix_conf) {
    if target.file_name().is_some_and(|name| name == "xi.conf")
      && !target.exists()
    {
      findings.push(Finding::DanglingInclude {
        nix_conf: nix_conf.to_path_buf(),
        missing:  target,
      });
    }
  }

  // The mirror image: the fragment was written but nix.conf never
  // amended, so nothing reads it and nothing will ever clean it up.
  if let Some(xi_conf) = ctx.xi_conf.as_deref()
    && xi_conf.is_file()
    && !references(nix_conf, xi_conf)
  {
    findings.push(Finding::UnreferencedXiConf {
      path:     xi_conf.to_path_buf(),
      nix_conf: nix_conf.to_path_buf(),
    });
  }

  findings
}

/// Every path an `include` / `!include` line in `conf` points at.
///
/// Relative targets resolve against the including file's directory,
/// which is what Nix itself does.
fn include_targets(conf: &Path) -> Vec<PathBuf> {
  let Ok(raw) = fs::read_to_string(conf) else {
    return Vec::new();
  };
  let dir = conf.parent().unwrap_or_else(|| Path::new("."));
  raw
    .lines()
    .filter_map(|line| {
      let trimmed = line.trim();
      let rest = trimmed.strip_prefix('!').unwrap_or(trimmed);
      let target = rest.strip_prefix("include")?.trim();
      (!target.is_empty()).then(|| {
        let target = Path::new(target);
        if target.is_absolute() {
          target.to_path_buf()
        } else {
          dir.join(target)
        }
      })
    })
    .collect()
}

/// Whether `conf` includes `target`.
fn references(conf: &Path, target: &Path) -> bool {
  include_targets(conf).iter().any(|path| path == target)
}

// ---------------------------------------------------------------------------
// Report envelope
// ---------------------------------------------------------------------------

/// What a repair run amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
  /// Discovery found nothing. The second of two consecutive runs.
  NoOp,
  /// Actions were listed but not carried out (`--dry-run`).
  Planned,
  /// Actions were carried out.
  Repaired,
}

/// One action that could not be carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairFailure {
  /// The action that failed.
  pub action: Action,
  /// Why, in the underlying error's own words.
  pub error:  String,
}

/// The single JSON envelope `xi <scope> sources repair` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairReport {
  /// Envelope schema version; see [`REPAIR_SCHEMA_VERSION`].
  pub schema_version: u32,
  /// Which half of the machine was repaired.
  pub scope:          Scope,
  /// Whether the action pass was skipped.
  pub dry_run:        bool,
  /// What the run amounted to.
  pub outcome:        Outcome,
  /// What discovery saw.
  pub findings:       Vec<Finding>,
  /// The planned repairs, or the ones carried out.
  pub actions:        Vec<Action>,
  /// Actions that failed. Empty on a clean run.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub failures:       Vec<RepairFailure>,
}

impl RepairReport {
  /// True when every planned action succeeded.
  #[must_use]
  pub const fn is_clean(&self) -> bool {
    self.failures.is_empty()
  }

  /// The actions that destroyed something, for the daemon's WARN lines.
  pub fn destructive(&self) -> impl Iterator<Item = &Action> {
    self.actions.iter().filter(|action| action.is_destructive())
  }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Discover, then repair unless `dry_run`.
///
/// Repair runs inside its own transaction, so a crash of repair is
/// itself recoverable: the next run sees repair's abandoned sentinel and
/// clears it before doing anything else.
///
/// The one action that happens *before* that transaction opens is
/// clearing a dead actor's sentinel, because opening ours would
/// overwrite theirs and the report would then claim a rollback that is
/// indistinguishable from our own bookkeeping.
///
/// # Errors
///
/// Propagates discovery errors and the failure to open or commit
/// repair's own transaction. Individual action failures are collected
/// into [`RepairReport::failures`] rather than aborting the run: one
/// unfixable finding must not stop the other nine from being fixed.
pub fn execute(ctx: &RepairContext, dry_run: bool) -> io::Result<RepairReport> {
  let plan = discover(ctx)?;

  if plan.is_empty() {
    return Ok(RepairReport {
      schema_version: REPAIR_SCHEMA_VERSION,
      scope: ctx.scope,
      dry_run,
      outcome: Outcome::NoOp,
      findings: Vec::new(),
      actions: Vec::new(),
      failures: Vec::new(),
    });
  }

  if dry_run {
    return Ok(RepairReport {
      schema_version: REPAIR_SCHEMA_VERSION,
      scope: ctx.scope,
      dry_run,
      outcome: Outcome::Planned,
      findings: plan.findings,
      actions: plan.actions,
      failures: Vec::new(),
    });
  }

  let mut failures = Vec::new();

  // Dead actors' sentinels go first, outside our transaction.
  for action in &plan.actions {
    if let Action::RollbackTransaction { actor, pid } = action {
      debug!(%actor, pid, "clearing abandoned transaction sentinel");
      if let Err(err) = remove_if_present(&ctx.state_dir.join(SENTINEL_NAME)) {
        failures.push(RepairFailure {
          action: action.clone(),
          error:  err.to_string(),
        });
      }
    }
  }

  let mut txn = Transaction::begin(&ctx.state_dir, "repair", "sources repair")?;
  for action in &plan.actions {
    if matches!(action, Action::RollbackTransaction { .. }) {
      continue;
    }
    if let Err(err) = apply(ctx, &mut txn, action) {
      failures.push(RepairFailure {
        action: action.clone(),
        error:  err.to_string(),
      });
    }
  }
  txn.commit()?;

  Ok(RepairReport {
    schema_version: REPAIR_SCHEMA_VERSION,
    scope: ctx.scope,
    dry_run,
    outcome: Outcome::Repaired,
    findings: plan.findings,
    actions: plan.actions,
    failures,
  })
}

fn apply(
  ctx: &RepairContext,
  txn: &mut Transaction,
  action: &Action,
) -> io::Result<()> {
  match action {
    Action::RemoveIncoming { path }
    | Action::RemovePrevious { path }
    | Action::RemoveStaleTemp { path }
    | Action::RemoveXiConf { path } => remove_if_present(path),

    Action::RestorePrevious { from, to } => fs::rename(from, to),

    // Handled before the transaction opens; see `execute`.
    Action::RollbackTransaction { .. } => Ok(()),

    Action::CorrectSourcesHead { to, .. } => {
      txn.stage_state("sources-head", to);
      Ok(())
    },

    Action::RemoveInclude { nix_conf, missing } => {
      strip_include(nix_conf, missing)
    },

    Action::ResolveSync { path, resolution } => {
      match resolution {
        SyncResolution::LeftToResume => Ok(()),
        SyncResolution::RolledBack => {
          if let Some(working_dir) =
            read_state_file(&ctx.state_dir, "working-dir")?.map(PathBuf::from)
            && working_dir.join(".git").exists()
          {
            reset_hard(&working_dir)?;
          }
          remove_if_present(path)
        },
      }
    },

    Action::ReconstructReceiptEntry { receipt_action } => {
      txn.stage_receipt(
        ReceiptEntry::new(receipt_action.clone()).reconstructed(),
      );
      Ok(())
    },
  }
}

/// Drop every line of `nix_conf` that includes `missing`.
///
/// The file's mode is carried over explicitly: `nix.conf` is normally
/// world-readable, and inheriting the umask instead would silently
/// tighten it and make Nix unreadable for unprivileged users.
fn strip_include(nix_conf: &Path, missing: &Path) -> io::Result<()> {
  let raw = fs::read_to_string(nix_conf)?;
  let dir = nix_conf.parent().unwrap_or_else(|| Path::new("."));

  let kept: Vec<&str> = raw
    .lines()
    .filter(|line| !line_includes(line, dir, missing))
    .collect();
  let mut body = kept.join("\n");
  if raw.ends_with('\n') && !body.is_empty() {
    body.push('\n');
  }

  write_atomic(nix_conf, body.as_bytes(), file_mode(nix_conf))
}

fn line_includes(line: &str, dir: &Path, target: &Path) -> bool {
  let trimmed = line.trim();
  let rest = trimmed.strip_prefix('!').unwrap_or(trimmed);
  rest.strip_prefix("include").is_some_and(|raw| {
    let raw = raw.trim();
    !raw.is_empty()
      && (if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
      } else {
        dir.join(raw)
      }) == target
  })
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
  use std::os::unix::fs::PermissionsExt as _;
  Some(fs::metadata(path).ok()?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
  None
}

/// `git reset --hard HEAD` in a working dir an abandoned sync left dirty.
fn reset_hard(working_dir: &Path) -> io::Result<()> {
  let out = std::process::Command::new("git")
    .arg("-C")
    .arg(working_dir)
    .args(["reset", "--hard", "HEAD"])
    .output()?;
  if out.status.success() {
    return Ok(());
  }
  Err(io::Error::other(format!(
    "git reset --hard HEAD failed in {}: {}",
    working_dir.display(),
    String::from_utf8_lossy(&out.stderr).trim()
  )))
}

// ---------------------------------------------------------------------------
// Auto-repair on startup
// ---------------------------------------------------------------------------

/// Repair `scope` as a daemon's first action, logging plan and report.
///
/// **Seam.** The gitops daemon this exists for is owned by
/// `xi-sources-gitops` and lands in parallel. When it does, its `main`
/// calls this *before acquiring the lock* and before the first tick:
/// repair's whole job is to make the pre-lock world consistent, and a
/// crashed predecessor's state has to be cleaned up before anything
/// decides whether it is safe to fetch.
///
/// Destructive actions are logged at WARN, because a daemon startup is
/// the only place an operator ever learns that a fetch was killed.
///
/// # Errors
///
/// Propagates [`execute`]'s errors.
pub fn auto_repair(scope: Scope) -> io::Result<RepairReport> {
  let ctx = RepairContext::for_scope(scope);
  let report = execute(&ctx, false)?;

  match report.outcome {
    Outcome::NoOp => debug!(%scope, "startup repair: nothing to do"),
    Outcome::Planned | Outcome::Repaired => {
      info!(
        %scope,
        actions = report.actions.len(),
        "startup repair completed"
      );
      for action in report.destructive() {
        warn!(?action, "startup repair destroyed crash leftovers");
      }
      for failure in &report.failures {
        warn!(?failure.action, error = %failure.error, "startup repair failed");
      }
    },
  }
  Ok(report)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  fn ctx(root: &Path) -> RepairContext {
    RepairContext {
      scope:            Scope::User,
      sources_dir:      root.join("sources"),
      state_dir:        root.join("state"),
      nix_conf:         Some(root.join("nix/nix.conf")),
      xi_conf:          Some(root.join("nix/xi.conf")),
      receipt_evidence: Vec::new(),
      resume_sync:      false,
    }
  }

  #[test]
  fn an_untouched_machine_is_a_no_op() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let report = execute(&ctx(tmp.path()), false).expect("repair");
    assert_eq!(report.outcome, Outcome::NoOp);
    assert!(report.findings.is_empty());
  }

  #[test]
  fn include_targets_resolve_against_the_conf_dir() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let conf = tmp.path().join("nix.conf");
    fs::write(&conf, "experimental-features = flakes\n!include xi.conf\n")
      .expect("write conf");
    assert_eq!(include_targets(&conf), vec![tmp.path().join("xi.conf")]);
  }

  #[test]
  fn stripping_an_include_keeps_every_other_line() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let conf = tmp.path().join("nix.conf");
    fs::write(&conf, "a = 1\n!include xi.conf\nb = 2\n").expect("write conf");

    strip_include(&conf, &tmp.path().join("xi.conf")).expect("strip");
    assert_eq!(fs::read_to_string(&conf).expect("read"), "a = 1\nb = 2\n");
  }

  #[test]
  fn a_live_writers_temp_file_is_not_swept() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state = tmp.path().join("state");
    fs::create_dir_all(&state).expect("state dir");
    fs::write(
      state.join(format!("sources-head.tmp.{}", std::process::id())),
      b"partial",
    )
    .expect("live temp");

    assert!(stale_temp_files(&state).expect("scan").is_empty());
  }
}
