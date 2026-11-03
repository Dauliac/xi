//! The switch entrypoint: installer pre-check, then source adoption.
//!
//! `xi <scope> switch [<remote-url>]` is the single way a machine is
//! pointed at a config repo, on a bare machine and on a managed one
//! alike. [`prepare`] is the shared pre-step every scope's `switch`
//! runs before it resolves an installable, so all four verbs behave
//! identically. See `openspec/changes/xi-sources-config/design.md`.

use std::{
  path::{Path, PathBuf},
  process::Command,
};

use clap::Args;
use color_eyre::{Result, eyre::eyre};
use tracing::{debug, info, warn};

use super::{
  RefLineage,
  Scope,
  config::{
    DEFAULT_BRANCH,
    Installer,
    PositionalKind,
    ScopePaths,
    SourcesConfig,
    classify_positional,
  },
  conflict::{
    AutoSync,
    DIRTY_SOURCE_MARKER,
    EvaluationInput,
    EvaluationSource,
    apply_auto_sync,
    inspect_working_tree,
    resolve_evaluation_source,
  },
  lock::{ActorName, LockOptions, SourcesLock},
  read_state_file,
  write_state_file,
};
use crate::installable::{Installable, InstallableArgs};

/// A switch that has taken the sources lock, with the installable it
/// resolved.
///
/// The lock covers the whole switch rather than just the adoption
/// fetch: build, activate and the generation record all write state
/// another actor would also write, so holding it only for the fetch
/// would serialise the cheapest step and leave the rest racing. The
/// caller therefore keeps this value alive until after
/// [`record_activation`], and a failure anywhere in between releases
/// the lock by dropping it, which is why no error path has to clean up.
///
/// A scope with no adopted source takes no lock: there is nothing to
/// serialise, and [`record_activation`] writes nothing for it either.
#[must_use = "the sources lock is released when this is dropped, so it has to \
              outlive activation"]
pub struct PreparedSwitch {
  installable: InstallableArgs,
  /// `None` when this scope has no source to contend over.
  _lock:       Option<SourcesLock>,
}

impl PreparedSwitch {
  /// The installable this switch should build.
  ///
  /// Borrowed rather than moved out: taking it by value would drop the
  /// lock at the point the caller starts using it, which is exactly
  /// backwards.
  #[must_use]
  pub const fn installable(&self) -> &InstallableArgs {
    &self.installable
  }
}

/// Source-related flags shared by every scope's `switch`.
#[derive(Debug, Clone, Args)]
pub struct SwitchSourcesArgs {
  /// Branch to track on the adopted remote.
  ///
  /// Top rung of the ladder for this key. Needed on the command line
  /// because a machine being adopted for the first time has no
  /// `.xi.toml` yet, and the bootstrap one-liner has nowhere else to
  /// put it.
  #[arg(long)]
  pub branch: Option<String>,

  /// Read the positional as a config repo to adopt, whatever its shape.
  #[arg(long, conflicts_with = "no_adopt")]
  pub adopt: bool,

  /// Read the positional as a flake installable, whatever its shape.
  #[arg(long)]
  pub no_adopt: bool,

  /// Allow re-pointing this scope at a different remote.
  #[arg(long)]
  pub force: bool,

  /// Evaluate the working dir even when its tree is dirty.
  ///
  /// Owned by `xi-sources-conflict`. Without it, uncommitted edits are a
  /// refusal: building from a tree that no sha describes produces a
  /// generation nobody can reproduce, so xi makes the operator say they
  /// meant it. The resulting generation is marked `dirty-source`.
  #[arg(long)]
  pub force_dirty: bool,
}

impl SwitchSourcesArgs {
  /// How to read `raw`, honouring an explicit override.
  fn kind_of(&self, raw: &str) -> PositionalKind {
    if self.adopt {
      return PositionalKind::ConfigRemote;
    }
    if self.no_adopt {
      return PositionalKind::Installable;
    }
    classify_positional(raw)
  }
}

/// Run the switch pre-steps and return the installable to build from.
///
/// Three cases, in order:
///
/// 1. The positional is a config-repo remote: adopt it, then build from the
///    sources dir.
/// 2. No positional, but this scope has a recorded source: build from whichever
///    tree the conflict policy nominates - the working dir when one is
///    configured and usable, the sources dir otherwise.
/// 3. Anything else: hand back the installable untouched, so `xi <scope> switch
///    .` and `xi <scope> switch github:you/cfg` behave exactly as they did
///    before this capability existed.
///
/// The installer pre-check runs first in every case, including case 3,
/// because reaching a switch without Nix is the same problem however
/// the user spelled the argument.
///
/// Cases 1 and 2 take the sources lock before their first write and
/// hand it back inside the [`PreparedSwitch`], which the caller must
/// keep alive until after [`record_activation`]. Case 3 takes no lock.
///
/// # Errors
///
/// Propagates config-load errors, the installer pre-check failure, clone
/// or state-write failures, a sources lock held by another actor (the
/// message names the holder and the estimated wait), and the conflict
/// policy's refusals: a dirty working dir without `--force-dirty`, a
/// diverged working dir, and a force-pushed remote.
pub fn prepare(
  scope: Scope,
  installable: InstallableArgs,
  opts: &SwitchSourcesArgs,
) -> Result<PreparedSwitch> {
  let paths = ScopePaths::resolve(scope);
  // `--branch` is the top rung for that key. It has to be reachable from
  // the command line: a machine being adopted for the first time has no
  // `.xi.toml`, and the bootstrap one-liner has nowhere else to put it.
  let cli = SourcesConfig {
    branch: opts.branch.clone(),
    ..SourcesConfig::default()
  };
  let resolved = super::args::resolve_layers(cli, &paths)?;

  ensure_nix_available(resolved.installer)?;

  let working_dir = super::args::working_dir_for(&resolved, &paths);

  // Case 1: an explicit remote on the command line.
  if let InstallableArgs::Specified(Installable::Flake {
    ref reference,
    ref attribute,
  }) = installable
    && opts.kind_of(reference) == PositionalKind::ConfigRemote
  {
    // Before `adopt`, which fetches into the sources dir: the lock has
    // to be held before the first write, not after it.
    let lock = take_lock(scope, &paths)?;
    warn_on_dirty_adoption(reference, working_dir.as_deref());
    adopt(scope, reference, &resolved.branch, &paths, opts.force)?;
    return Ok(PreparedSwitch {
      installable: sources_installable(&paths.sources_dir, attribute.clone()),
      _lock:       Some(lock),
    });
  }

  // Case 2: nothing on the command line, but this scope is adopted.
  if matches!(installable, InstallableArgs::Unspecified)
    && let Some(recorded) = recorded_remote(&paths.state_dir)?
  {
    debug!("using recorded source for {scope}: {recorded}");
    if !paths.sources_dir.join(".git").is_dir() {
      warn!(
        "{scope} records {recorded} but {} is not a clone; run `xi {} switch \
         {recorded}` to restore it",
        paths.sources_dir.display(),
        scope.as_str()
      );
      // Case 3 fallthrough: with no clone there is nothing to apply the
      // conflict policy to.
      return Ok(PreparedSwitch {
        installable,
        _lock: None,
      });
    }

    // Before the auto-copy sync, which writes the working dir.
    let lock = take_lock(scope, &paths)?;

    // `auto-copy` reconciles before the main action, by spec, so the
    // evaluation below sees the tree the operator asked xi to maintain.
    apply_queued_sync(
      scope,
      resolved.sync_mode,
      &paths,
      working_dir.as_deref(),
    );

    return Ok(PreparedSwitch {
      installable: evaluate_from(scope, &paths, working_dir.as_deref(), opts)?,
      _lock:       Some(lock),
    });
  }

  // Case 3: this scope has no source, so there is nothing to serialise.
  Ok(PreparedSwitch {
    installable,
    _lock: None,
  })
}

/// Take the sources lock for the whole switch, or say who has it.
///
/// `ActorName::Switch` is the top of the spec's priority order, so an
/// operator waiting at a terminal is granted ahead of a daemon tick
/// that queues behind them. The wait budget is the library default of
/// one lease: a switch that cannot get the lock inside that reports the
/// holder's record and the estimated wait rather than failing bare.
fn take_lock(scope: Scope, paths: &ScopePaths) -> Result<SourcesLock> {
  SourcesLock::acquire(
    &paths.state_dir,
    &LockOptions::new(
      ActorName::Switch,
      format!("xi {} switch", scope.as_str()),
    ),
  )
  .map_err(|err| eyre!("{err}"))
}

/// Ask the conflict policy which tree to build, and turn it into an
/// installable.
///
/// This is where "the current sources are always right" meets "a local
/// non-pushed version is a normal state": the policy answers with the
/// working dir whenever one is usable, so unpushed commits are deployed
/// rather than treated as a conflict to resolve.
fn evaluate_from(
  scope: Scope,
  paths: &ScopePaths,
  working_dir: Option<&Path>,
  opts: &SwitchSourcesArgs,
) -> Result<InstallableArgs> {
  let recorded = recorded_refs(&paths.state_dir)?;
  let what = format!("xi {} switch", scope.as_str());

  match resolve_evaluation_source(&EvaluationInput {
    sources_dir: &paths.sources_dir,
    working_dir,
    recorded: &recorded,
    force_dirty: opts.force_dirty,
    what: &what,
  }) {
    EvaluationSource::WorkingDir { path, dirty_source } => {
      if dirty_source {
        // The generation about to be built is not reproducible from any
        // sha, so the state dir has to say so; a later `sources status`
        // naming a sha that never produced the running system would be
        // worse than naming nothing.
        mark_dirty_source(&paths.state_dir);
      }
      Ok(sources_installable(&path, Vec::new()))
    },
    EvaluationSource::SourcesDir { path } => {
      Ok(sources_installable(&path, Vec::new()))
    },
    EvaluationSource::Error { reason } => Err(eyre!(reason)),
  }
}

/// The three refs as last recorded in the state dir.
fn recorded_refs(state_dir: &Path) -> Result<RefLineage> {
  Ok(RefLineage {
    remote_head:       read_state_file(state_dir, "remote-head")?,
    working_head:      read_state_file(state_dir, "working-head")?,
    sources_head:      read_state_file(state_dir, "sources-head")?,
    active_generation: read_state_file(state_dir, "active-generation")?,
  })
}

/// Record that the next generation came from uncommitted edits.
///
/// Best-effort on purpose: a state dir xi cannot write to is a problem
/// worth a warning, but refusing the switch over a marker would make
/// `--force-dirty` useless exactly where it is needed - on a machine
/// whose state dir is not set up yet.
fn mark_dirty_source(state_dir: &Path) {
  let existing = read_state_file(state_dir, "active-generation")
    .ok()
    .flatten()
    .unwrap_or_default();
  let marked = if existing.is_empty() {
    DIRTY_SOURCE_MARKER.to_owned()
  } else if existing.contains(DIRTY_SOURCE_MARKER) {
    existing
  } else {
    format!("{existing} ({DIRTY_SOURCE_MARKER})")
  };

  if let Err(err) = write_state_file(state_dir, "active-generation", &marked) {
    warn!(
      "could not mark the generation as {DIRTY_SOURCE_MARKER} in {}: {err}",
      state_dir.display()
    );
  }
}

/// Run the sync `sync_mode = "auto-copy"` queued, if any.
///
/// Never fatal: the operator ran a switch, not a sync, and a failed
/// reconciliation must not take down the command they asked for. Every
/// outcome is logged instead.
fn apply_queued_sync(
  scope: Scope,
  sync_mode: super::config::SyncMode,
  paths: &ScopePaths,
  working_dir: Option<&Path>,
) {
  let what = format!("xi {} sources sync", scope.as_str());
  match apply_auto_sync(
    sync_mode,
    &paths.sources_dir,
    &paths.state_dir,
    working_dir,
    &what,
  ) {
    Ok(AutoSync::Applied(outcome)) => {
      info!(
        "auto-copy synced {} file(s) into {}; HEAD now {}",
        outcome.written.len(),
        working_dir.map_or_else(
          || "the working dir".to_owned(),
          |dir| dir.display().to_string()
        ),
        outcome.sources_head
      );
    },
    Ok(
      AutoSync::DeferredDirty | AutoSync::NotRequested | AutoSync::UpToDate,
    ) => {},
    Err(err) => warn!("auto-copy sync failed, continuing anyway: {err}"),
  }
}

/// Warn when adopting a remote whose local checkout has uncommitted work.
///
/// Adoption deploys the *committed* state of the remote. An operator
/// with a staged-but-uncommitted fix in their own checkout has already
/// been burned by this: `xi home build` passed in the working tree while
/// the adopted clone of the same branch failed, because the fix was
/// never committed. The divergence being fatal was luck; a merely
/// different branch would have silently activated a configuration
/// nobody was looking at.
///
/// Warning only, no prompt: adoption is often the first command on a
/// fresh machine, and a blocking question there would be worse than the
/// surprise it prevents. `sync_mode` is what resolves this properly once
/// the machine is managed.
fn warn_on_dirty_adoption(remote: &str, working_dir: Option<&Path>) {
  if let Some(message) = dirty_adoption_warning(remote, working_dir) {
    warn!("{message}");
  }
}

/// The candidates checked for a dirty checkout tracking `remote`.
///
/// `working_dir` first, when configured, then the process's own CWD -
/// adoption is frequently run from inside the checkout being adopted,
/// before any `.xi.toml` has recorded a working dir at all.
fn dirty_adoption_candidates(working_dir: Option<&Path>) -> [Option<&Path>; 2] {
  [working_dir, Some(Path::new("."))]
}

/// The warning text for [`warn_on_dirty_adoption`], or `None` when
/// nothing qualifies.
///
/// Separated from the `tracing` call so the detection logic - which
/// candidate tracks `remote`, and whether it is dirty - is testable
/// without capturing log output. Returns the *first* matching candidate
/// only: `working_dir` and CWD are usually the same checkout, and
/// stacking two warnings about one tree would read as two problems
/// instead of one.
///
/// # Errors
///
/// None returned as an error: a candidate that is not a readable git
/// checkout, or whose `origin` cannot be read, is silently skipped
/// rather than surfaced, because adoption itself is what is being
/// attempted and a probe of an unrelated directory failing is not its
/// business.
fn dirty_adoption_warning(remote: &str, working_dir: Option<&Path>) -> Option<String> {
  for dir in dirty_adoption_candidates(working_dir).into_iter().flatten() {
    if !tracks_remote(dir, remote) {
      continue;
    }
    let Ok(tree) = inspect_working_tree(dir) else {
      continue;
    };
    if !tree.is_dirty() {
      continue;
    }

    return Some(format_dirty_adoption_warning(dir, remote, tree.paths.len()));
  }
  None
}

/// Render the dirty-adoption warning for one checkout.
///
/// `count` is the number of dirty paths rather than the full
/// [`super::conflict::DirtyTree`], so this stays a pure string builder a
/// test can call without a filesystem.
fn format_dirty_adoption_warning(dir: &Path, remote: &str, count: usize) -> String {
  format!(
    "{} has {count} uncommitted change(s) against {remote}.\nAdoption deploys \
     the committed state of that remote, not what is in that tree. Commit \
     and push first if the difference matters.",
    dir.display()
  )
}

/// Whether `dir` is a git checkout whose `origin` is `remote`.
///
/// Compared through the model's canonicalisation, so `git@host:o/r.git`
/// and `ssh://host/o/r` are recognised as the same remote rather than
/// silently skipping the warning.
fn tracks_remote(dir: &Path, remote: &str) -> bool {
  let Ok(out) = Command::new("git")
    .arg("-C")
    .arg(dir)
    .args(["remote", "get-url", "origin"])
    .output()
  else {
    return false;
  };
  if !out.status.success() {
    return false;
  }

  let origin = String::from_utf8_lossy(&out.stdout);
  super::canonicalize_url(origin.trim()) == super::canonicalize_url(remote)
}

/// A flake installable rooted at the managed sources dir.
fn sources_installable(
  sources_dir: &Path,
  attribute: Vec<String>,
) -> InstallableArgs {
  InstallableArgs::Specified(Installable::Flake {
    reference: sources_dir.display().to_string(),
    attribute,
  })
}

/// The remote this scope is adopted to, if any.
fn recorded_remote(state_dir: &Path) -> Result<Option<String>> {
  Ok(read_state_file(state_dir, "remote-url")?)
}

/// Adopt `remote` as this scope's source and make the clone current.
///
/// Idempotent for the remote already recorded: that case fast-forwards
/// and returns, which is what lets one command be correct on a fresh
/// machine and on a managed one. A *different* remote is refused
/// without `force`, because silently repointing what a host deploys
/// from is not something to do on a typo.
fn adopt(
  scope: Scope,
  remote: &str,
  branch: &str,
  paths: &ScopePaths,
  force: bool,
) -> Result<()> {
  super::config::validate_remote_url(remote).map_err(|e| eyre!(e))?;

  let recorded = recorded_remote(&paths.state_dir)?;
  let repointing = recorded.as_deref().is_some_and(|r| r != remote);

  if repointing && !force {
    let recorded = recorded.unwrap_or_default();
    return Err(eyre!(
      "{scope} scope is already adopted to {recorded}; refusing to re-point \
       it at {remote} without --force"
    ));
  }

  for dir in [&paths.sources_dir, &paths.state_dir] {
    create_private_dir(dir)?;
  }

  if paths.sources_dir.join(".git").is_dir() {
    // Under --force the existing clone still points at the old origin.
    // Recording the new URL without moving origin would leave the two
    // silently disagreeing, and every later fetch would quietly keep
    // pulling the remote the operator just replaced.
    if repointing {
      info!("re-pointing origin at {remote}");
      set_origin(&paths.sources_dir, remote)?;
    }
    info!("updating {}", paths.sources_dir.display());
    fetch_and_reset(&paths.sources_dir, branch)?;
  } else {
    info!("cloning {remote} into {}", paths.sources_dir.display());
    clone(remote, branch, &paths.sources_dir)?;
  }

  write_state_file(&paths.state_dir, "remote-url", remote)?;

  // The model says `sources_head` is `None` "only on a freshly-
  // initialised sources dir before any clone", so the clone that just
  // landed has to record it here.
  //
  // `active-generation` is deliberately *not* written: adoption is
  // steps 1-5 of the entrypoint and activation is step 6. Until step 6
  // succeeds the honest answer from `xi <scope> sources status` is
  // `Pending` - see [`record_activation`].
  if let Some(head) = git_head(&paths.sources_dir)? {
    write_state_file(&paths.state_dir, "sources-head", &head)?;
  }
  Ok(())
}

/// Record a successful activation against this scope's adopted source.
///
/// This is the only writer of `active-generation`, and therefore the
/// only thing that moves `xi <scope> sources status` from `Pending` to
/// `Ready`. Adoption clones and records the remote but does not claim a
/// generation, so the window between a successful clone and a
/// successful activation - a build that fails, a user who declines the
/// `--ask` prompt - reports `Pending`, which is exactly the model's
/// "sources dir exists but `active_generation` is unknown".
///
/// A no-op on a scope with no recorded remote: a one-off
/// `xi home switch .` is not a sources-managed deployment, and minting
/// state for it would make `status` describe a source nobody adopted.
///
/// Best-effort by design. The machine is switched whether or not the
/// metadata lands, so a write failure warns rather than turning a
/// successful activation into a failed command.
pub fn record_activation(scope: Scope, activated_profile: &Path) {
  let paths = ScopePaths::resolve(scope);

  match recorded_remote(&paths.state_dir) {
    Ok(Some(_)) => {},
    Ok(None) => {
      debug!("{scope} has no recorded source; not recording a generation");
      return;
    },
    Err(err) => {
      warn!("could not read {scope} sources state: {err}");
      return;
    },
  }

  if let Err(err) = record_activation_at(&paths, activated_profile) {
    warn!(
      "activation succeeded but recording it under {} failed: {err}",
      paths.state_dir.display()
    );
  }
}

/// The writer half of [`record_activation`], against an explicit path
/// table rather than the process environment.
///
/// Separated so the state it produces is testable without mutating
/// `$HOME` or `$XDG_*` in a process shared with other tests.
///
/// # Errors
///
/// Propagates state-write failures and `git rev-parse` failures.
pub fn record_activation_at(
  paths: &ScopePaths,
  activated_profile: &Path,
) -> Result<()> {
  // The out-link normally lives in a tempdir that is gone by the time
  // anyone reads this file, so resolve it to the store path behind it.
  let generation = activated_profile
    .canonicalize()
    .unwrap_or_else(|_| activated_profile.to_path_buf());

  write_state_file(
    &paths.state_dir,
    "active-generation",
    &generation.display().to_string(),
  )?;

  // The activated closure was built from whatever the clone pointed at,
  // so the two refs belong to the same moment and are written together.
  if let Some(head) = git_head(&paths.sources_dir)? {
    write_state_file(&paths.state_dir, "sources-head", &head)?;
  }
  Ok(())
}

/// HEAD of a clone, or `None` when the path is not one.
///
/// "Not a clone" is not an error here: both callers reach this on paths
/// that are legitimately absent on a machine mid-adoption.
fn git_head(repo: &Path) -> Result<Option<String>> {
  if !repo.join(".git").is_dir() {
    return Ok(None);
  }

  let out = Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["rev-parse", "HEAD"])
    .output()?;
  if !out.status.success() {
    return Err(eyre!(
      "git rev-parse HEAD failed in {}: {}",
      repo.display(),
      String::from_utf8_lossy(&out.stderr).trim()
    ));
  }

  Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()))
}

/// Create a directory owner-only, not whatever the umask allows.
///
/// `create_dir_all` honours the process umask, which on a typical
/// desktop is 002 and yields 0775. A config repo clone is private by
/// contract (the path table requires 0700), and adoption is the path
/// that creates it first on a fresh machine, before any Nix module
/// activation gets a chance to chmod it.
fn create_private_dir(dir: &Path) -> Result<()> {
  std::fs::create_dir_all(dir)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

/// `git clone` into a directory that may already exist but is empty.
fn clone(remote: &str, branch: &str, dest: &Path) -> Result<()> {
  // `GIT_TERMINAL_PROMPT=0` is deliberate: without it a missing
  // credential makes git block on a prompt, turning a scripted switch
  // into a hang instead of a clean failure.
  let out = Command::new("git")
    .env("GIT_TERMINAL_PROMPT", "0")
    .args(["clone", "--branch", branch, "--"])
    .arg(remote)
    .arg(dest)
    .output()?;
  if out.status.success() {
    return Ok(());
  }

  let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
  // A missing branch is the single most likely adoption failure, because
  // xi defaults to `main` and plenty of repos are still on `master`.
  // Git's own message never says where the branch name came from.
  if stderr.contains("Remote branch") && stderr.contains("not found") {
    return Err(eyre!(
      "the remote has no branch `{branch}`.\n\n{stderr}\n\nxi tracks \
       `{DEFAULT_BRANCH}` unless told otherwise. Pick the branch with \
       `--branch <name>`, `XI_SOURCES_BRANCH`, or `[sources] branch` in \
       .xi.toml."
    ));
  }
  Err(eyre!("git clone failed: {stderr}"))
}

/// Point an existing clone at a different origin.
fn set_origin(repo: &Path, remote: &str) -> Result<()> {
  let out = Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["remote", "set-url", "origin"])
    .arg(remote)
    .output()?;
  if out.status.success() {
    return Ok(());
  }
  Err(eyre!(
    "git remote set-url failed: {}",
    String::from_utf8_lossy(&out.stderr).trim()
  ))
}

/// Fast-forward an existing clone to the tracked branch.
fn fetch_and_reset(repo: &Path, branch: &str) -> Result<()> {
  for args in [
    vec!["fetch", "origin", branch],
    vec!["checkout", branch],
    vec!["reset", "--hard", &format!("origin/{branch}")],
  ] {
    let out = Command::new("git")
      .env("GIT_TERMINAL_PROMPT", "0")
      .arg("-C")
      .arg(repo)
      .args(&args)
      .output()?;
    if !out.status.success() {
      return Err(eyre!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
      ));
    }
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Installer pre-check
// ---------------------------------------------------------------------------

/// Resolve this scope's `installer` setting and run the pre-check.
///
/// # Errors
///
/// Propagates config-load errors and the pre-check failure.
pub fn ensure_nix_available_for(scope: Scope) -> Result<()> {
  // Resolving config needs no Nix, so this is safe to do first.
  let paths = ScopePaths::resolve(scope);
  let resolved = super::args::resolve_layers(SourcesConfig::default(), &paths)?;
  ensure_nix_available(resolved.installer)
}

/// Ensure a usable `nix` exists before any evaluation work starts.
///
/// This runs on *every* switch, not only the bootstrap path: arriving
/// at a switch without Nix is the same problem whether the operator
/// passed a remote, a local flake, or nothing, and a bare
/// `nix: command not found` is a worse answer than naming the fix.
///
/// When Nix is present this is a pure PATH lookup: no network, no
/// prompt, no measurable cost on the everyday switch.
///
/// # Errors
///
/// Returns an error naming the exact installer command when Nix is
/// absent, or naming the `installer` config key when it is `none`.
pub fn ensure_nix_available(installer: Option<Installer>) -> Result<()> {
  if which::which("nix").is_ok() {
    debug!("nix found on PATH; installer pre-check is a no-op");
    return Ok(());
  }

  match installer {
    Some(Installer::None) => {
      Err(eyre!(
        "nix is not on PATH and `installer = \"none\"` opts out of installing \
         it.\nSet `installer` to `detsys` or `lix` in [sources], or install \
         Nix yourself and re-run."
      ))
    },
    // Receipt-backed auto-install is owned by xi-sources-bootstrap and
    // is not implemented yet. Printing the exact command the operator
    // can paste is honest; running an OS-level installer with no
    // receipt to reverse it would not be.
    chosen => {
      let backend = chosen.unwrap_or(Installer::Detsys);
      Err(eyre!(
        "nix is not on PATH.\n\nxi can install it for you once \
         xi-sources-bootstrap lands. Until then, run:\n\n  {}\n\nthen re-run \
         this command.",
        install_command(backend)
      ))
    },
  }
}

/// The documented one-liner for each installer backend.
#[must_use]
pub const fn install_command(backend: Installer) -> &'static str {
  match backend {
    Installer::Lix => {
      "curl -sSf -L https://install.lix.systems/lix | sh -s -- install \
       --no-confirm"
    },
    Installer::Detsys | Installer::None => {
      "curl -LsSf https://install.determinate.systems/nix | sh -s -- install \
       --no-confirm --determinate=false"
    },
  }
}

/// Where the managed clone for `scope` lives.
#[must_use]
pub fn sources_dir_for(scope: Scope) -> PathBuf {
  ScopePaths::resolve(scope).sources_dir
}

// ---------------------------------------------------------------------------
// Tests  -  xi-zbll: warn on dirty adoption
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use std::fs;

  use tempfile::TempDir;

  use super::*;

  const REMOTE: &str = "https://git.example/config";

  fn git(at: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
      .arg("-C")
      .arg(at)
      .args([
        "-c",
        "user.email=t@e",
        "-c",
        "user.name=t",
        "-c",
        "commit.gpgsign=false",
      ])
      .args(args)
      .output()
      .expect("run git");
    assert!(
      out.status.success(),
      "git {args:?} failed in {}: {}",
      at.display(),
      String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
  }

  /// A git checkout with one commit, `origin` pointed at `origin_url`.
  fn checkout(root: &Path, origin_url: &str) -> PathBuf {
    let dir = root.join("checkout");
    fs::create_dir_all(&dir).expect("mkdir checkout");
    fs::write(dir.join("flake.nix"), "{ outputs = _: { }; }\n")
      .expect("write flake");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["remote", "add", "origin", origin_url]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    dir
  }

  // -- format_dirty_adoption_warning: pure formatting --------------------

  #[test]
  fn the_warning_names_the_remote_and_the_file_count() {
    let message =
      format_dirty_adoption_warning(Path::new("/home/op/dotfiles"), REMOTE, 3);
    assert!(message.contains(REMOTE), "{message}");
    assert!(message.contains('3'), "{message}");
    assert!(message.contains("/home/op/dotfiles"), "{message}");
    assert!(
      message.contains("deploys"),
      "must say adoption deploys committed state, got: {message}"
    );
  }

  // -- tracks_remote -------------------------------------------------------

  #[test]
  fn tracks_remote_is_true_for_the_exact_origin() {
    let tmp = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), REMOTE);
    assert!(tracks_remote(&dir, REMOTE));
  }

  #[test]
  fn tracks_remote_canonicalises_equivalent_url_shapes() {
    let tmp = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "git@git.example:config.git");
    assert!(
      tracks_remote(&dir, "ssh://git.example/config"),
      "scp-style and ssh:// spellings of the same remote must match"
    );
  }

  #[test]
  fn tracks_remote_is_false_for_a_different_remote() {
    let tmp = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), REMOTE);
    assert!(!tracks_remote(&dir, "https://git.example/other"));
  }

  #[test]
  fn tracks_remote_is_false_for_a_non_repository() {
    let tmp = TempDir::new().unwrap();
    let not_a_repo = tmp.path().join("not-a-repo");
    fs::create_dir_all(&not_a_repo).unwrap();
    assert!(!tracks_remote(&not_a_repo, REMOTE));
  }

  // -- dirty_adoption_warning: the end-to-end check -----------------------

  #[test]
  fn a_dirty_checkout_tracking_the_remote_produces_a_warning() {
    let tmp = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), REMOTE);
    fs::write(dir.join("flake.nix"), "{ outputs = _: { v = 2; }; }\n")
      .expect("dirty the checkout");

    let message = dirty_adoption_warning(REMOTE, Some(&dir))
      .expect("a dirty checkout tracking the remote must warn");
    assert!(message.contains(REMOTE), "{message}");
    assert!(message.contains("1 uncommitted"), "{message}");
  }

  #[test]
  fn a_clean_checkout_tracking_the_remote_is_silent() {
    let tmp = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), REMOTE);
    assert_eq!(dirty_adoption_warning(REMOTE, Some(&dir)), None);
  }

  #[test]
  fn a_dirty_checkout_tracking_a_different_remote_is_silent() {
    let tmp = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "https://git.example/unrelated");
    fs::write(dir.join("flake.nix"), "{ outputs = _: { v = 2; }; }\n")
      .expect("dirty the checkout");

    assert_eq!(
      dirty_adoption_warning(REMOTE, Some(&dir)),
      None,
      "adoption of `{REMOTE}` must not warn about an unrelated checkout"
    );
  }

  #[test]
  fn with_no_working_dir_and_a_non_tracking_cwd_nothing_warns() {
    // The CWD fallback is exercised by passing `None`; the test process's
    // own CWD does not track a fake `https://git.example` remote, so this
    // must fall through to `None` rather than panic or false-positive.
    assert_eq!(dirty_adoption_warning(REMOTE, None), None);
  }
}
