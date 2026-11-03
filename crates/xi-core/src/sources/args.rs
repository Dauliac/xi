//! The `xi <scope> sources` maintenance surface.
//!
//! This group lives in `xi-core` rather than in the `xi` binary crate
//! because every scope crate (`xi-nixos`, `xi-darwin`, `xi-home`) has to
//! mount it, and none of them can depend on `xi`.
//!
//! There is deliberately no `--scope` flag. Scope comes from the verb
//! that owns the group, resolved through [`Scope`], so it cannot be
//! stated twice and cannot contradict itself. See
//! `openspec/changes/xi-sources-config/design.md`.
//!
//! This is the operator-maintenance surface, not the way a machine is
//! set up: that is `xi <scope> switch [<remote-url>]`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use color_eyre::{Result, eyre::eyre};
use tracing::warn;

use super::{
  GitopsHealth,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatusResponse,
  config::{Layers, Provenance, ResolvedSources, ScopePaths, SourcesConfig},
  conflict::{
    ObservedRefs,
    SourcesDiffResponse,
    SourcesSyncResponse,
    SyncRequest,
    sync,
    three_way_diff,
  },
  log::{self, Level as LogLevelFilter, TailFilter, TailRequest},
};

/// Inspect and manage the config repo this scope deploys from
#[derive(Debug, Clone, Args)]
pub struct SourcesProxy {
  #[clap(subcommand)]
  pub command: SourcesCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SourcesCommand {
  /// Report the sources model for this scope as a JSON envelope
  Status(StatusArgs),
  /// Diff the remote, the sources dir and the working dir
  Diff(DiffArgs),
  /// Copy the sources dir over the working dir, one way
  Sync(SyncArgs),
  /// Read the gitops daemon's structured log for this scope
  Log(LogArgs),
}

/// `xi <scope> sources log` - the log-reading verb group.
///
/// A group of one today (`tail`); `xi-sources-observability` names no
/// second log-reading verb, so this stays a group rather than a bare
/// `xi <scope> sources log-tail` for the same reason `sources` itself
/// is a group: room to grow without a breaking rename.
#[derive(Debug, Clone, Args)]
pub struct LogArgs {
  #[clap(subcommand)]
  pub command: LogCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum LogCommand {
  /// Stream the resolved sink's log
  Tail(LogTailArgs),
}

#[derive(Debug, Clone, Args)]
pub struct LogTailArgs {
  /// Keep streaming as new lines arrive, like `tail -f`.
  #[arg(long)]
  pub follow: bool,

  /// Only show lines at or after this RFC 3339 timestamp.
  ///
  /// At system scope this is passed through to `journalctl --since`
  /// verbatim, which also accepts relative forms (`"1 hour ago"`); at
  /// user scope only RFC 3339 is understood, since there is no
  /// `journalctl` doing the parsing.
  #[arg(long)]
  pub since: Option<String>,

  /// Only show lines at or above this level (trace|debug|info|warn|error).
  #[arg(long)]
  pub level: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct StatusArgs {
  /// Remote URL of the config repo.
  ///
  /// Highest rung of the precedence ladder. Omit it to let
  /// `XI_SOURCES_REMOTE`, `.xi.toml [sources]`, or the Nix module
  /// answer.
  #[arg(long, env = "XI_SOURCES_REMOTE")]
  pub remote: Option<String>,

  /// Add a `provenance` map naming the rung that answered for each key.
  #[arg(long)]
  pub explain: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DiffArgs {
  /// Emit the house JSON envelope instead of the plain diff body.
  #[arg(long)]
  pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SyncArgs {
  /// Overwrite uncommitted edits in the working dir.
  ///
  /// The edits are lost without recovery. Without this flag a dirty
  /// working dir is a refusal, which is the default because the sync is
  /// a copy-with-erase and git cannot undo what was never committed.
  #[arg(long)]
  pub force_dirty: bool,

  /// Emit the house JSON envelope instead of a human summary.
  #[arg(long)]
  pub json: bool,
}

impl SourcesProxy {
  /// Dispatch the selected verb at `scope`.
  ///
  /// `scope` is supplied by the parent command, which is the only thing
  /// that knows it.
  ///
  /// # Errors
  ///
  /// Propagates config-load, I/O and serialisation errors.
  pub fn run(self, scope: Scope) -> Result<()> {
    match self.command {
      SourcesCommand::Status(args) => args.run(scope),
      SourcesCommand::Diff(args) => args.run(scope),
      SourcesCommand::Sync(args) => args.run(scope),
      SourcesCommand::Log(args) => args.run(scope),
    }
  }
}

impl LogArgs {
  /// Dispatch the selected log verb at `scope`.
  ///
  /// # Errors
  ///
  /// Propagates [`LogTailArgs::run`]'s errors.
  pub fn run(self, scope: Scope) -> Result<()> {
    match self.command {
      LogCommand::Tail(args) => args.run(scope),
    }
  }
}

impl LogTailArgs {
  /// Stream the resolved sink's log for `scope` to stdout.
  ///
  /// # Errors
  ///
  /// Returns an error if `--since` or `--level` fail to parse, or if
  /// reading the sink fails: the user-scope log file cannot be opened
  /// (most commonly because the daemon has never run, so no log exists
  /// yet), or `journalctl` cannot be spawned at system scope.
  pub fn run(self, scope: Scope) -> Result<()> {
    let since: Option<DateTime<Utc>> = self
      .since
      .as_deref()
      .map(|raw| {
        DateTime::parse_from_rfc3339(raw)
          .map(|dt| dt.with_timezone(&Utc))
          .map_err(|e| eyre!("invalid '--since' value '{raw}': {e}"))
      })
      .transpose()?;
    let level = self
      .level
      .as_deref()
      .map(LogLevelFilter::parse)
      .transpose()
      .map_err(|e| eyre!(e))?;

    let request = TailRequest {
      filter: TailFilter { since, level },
      follow: self.follow,
    };
    let paths = ScopePaths::resolve(scope);
    log::tail(scope, &paths.state_dir, &request, &mut std::io::stdout()).map_err(
      |e| {
        eyre!(
          "reading the {scope} sources log failed: {e}\nHas the gitops \
           daemon run yet? See [[xi-sources-gitops]]."
        )
      },
    )
  }
}

impl DiffArgs {
  /// Print the three-way diff for this scope.
  ///
  /// With no working dir configured this is the two-way `remote →
  /// sources` diff, which is the whole report rather than a degraded
  /// version of it: on a machine with no editable clone there is no
  /// third ref to compare against.
  ///
  /// # Errors
  ///
  /// Propagates config-load errors and JSON serialisation errors. An
  /// unreachable ref is *not* an error: it degrades its own leg and the
  /// envelope says so, because one missing object should not hide the
  /// legs that did resolve.
  pub fn run(self, scope: Scope) -> Result<()> {
    let paths = ScopePaths::resolve(scope);
    let resolved = resolve_layers(SourcesConfig::default(), &paths)?;
    let working_dir = working_dir_for(&resolved, &paths);

    let recorded = super::RefLineage {
      remote_head:       super::read_state_file(
        &paths.state_dir,
        "remote-head",
      )?,
      working_head:      super::read_state_file(
        &paths.state_dir,
        "working-head",
      )?,
      sources_head:      super::read_state_file(
        &paths.state_dir,
        "sources-head",
      )?,
      active_generation: None,
    };
    let refs = ObservedRefs::observe(
      &paths.sources_dir,
      working_dir.as_deref(),
      &recorded,
    );
    let report =
      three_way_diff(&paths.sources_dir, working_dir.as_deref(), &refs);

    if self.json {
      println!(
        "{}",
        serde_json::to_string_pretty(&SourcesDiffResponse::new(report))?
      );
      return Ok(());
    }

    if let Some(reason) = report.degraded_reason() {
      warn!("{reason}");
    }
    print!("{}", report.render());
    Ok(())
  }
}

impl SyncArgs {
  /// Copy the sources dir over the working dir.
  ///
  /// # Errors
  ///
  /// Returns an error when no working dir is configured (there is
  /// nothing to sync into), when the working dir is dirty and
  /// `--force-dirty` was not given, and on any git or filesystem
  /// failure during the copy.
  pub fn run(self, scope: Scope) -> Result<()> {
    let paths = ScopePaths::resolve(scope);
    let resolved = resolve_layers(SourcesConfig::default(), &paths)?;
    let Some(working_dir) = working_dir_for(&resolved, &paths) else {
      return Err(eyre!(
        "no working dir is configured for the {scope} scope, so there is \
         nothing to sync into.\nSet `[sources] working-dir` in .xi.toml or \
         `XI_SOURCES_WORKING_DIR` to the clone you edit by hand."
      ));
    };

    let outcome = sync(&SyncRequest {
      sources_dir: &paths.sources_dir,
      working_dir: &working_dir,
      state_dir:   &paths.state_dir,
      force_dirty: self.force_dirty,
      what:        &format!("xi {} sources sync", scope.as_str()),
    })
    .map_err(|e| eyre!(e.to_string()))?;

    if self.json {
      println!(
        "{}",
        serde_json::to_string_pretty(&SourcesSyncResponse::new(outcome))?
      );
      return Ok(());
    }

    println!(
      "synced {} file(s), deleted {}, HEAD now {}",
      outcome.written.len(),
      outcome.deleted.len(),
      outcome.sources_head
    );
    Ok(())
  }
}

/// The working dir for this scope, config rungs first.
///
/// The config ladder outranks the state dir for the same reason it
/// outranks it for `remote`: an explicit setting beats whatever a past
/// `switch` happened to record. A recorded path that no longer exists is
/// dropped rather than used, since every caller of this would otherwise
/// have to re-check it.
///
/// A state dir that cannot be read records nothing, which is the same
/// answer as a machine that never adopted a working dir, so there is no
/// error left for this to return.
pub(crate) fn working_dir_for(
  resolved: &ResolvedSources,
  paths: &ScopePaths,
) -> Option<PathBuf> {
  let configured = resolved.working_dir.clone().or_else(|| {
    super::read_state_file(&paths.state_dir, "working-dir")
      .ok()
      .flatten()
      .map(PathBuf::from)
  });

  configured.filter(|dir| {
    let live = dir.is_dir();
    if !live {
      warn!(
        "recorded working dir {} no longer exists; ignoring it",
        dir.display()
      );
    }
    live
  })
}

/// Where this scope's working dir is, for callers outside this module.
///
/// # Errors
///
/// Propagates config-load and state-dir read errors.
pub fn resolve_working_dir(scope: Scope) -> Result<Option<PathBuf>> {
  let paths = ScopePaths::resolve(scope);
  let resolved = resolve_layers(SourcesConfig::default(), &paths)?;
  Ok(working_dir_for(&resolved, &paths))
}

impl StatusArgs {
  /// Print the sources status envelope on stdout.
  ///
  /// Nothing but the envelope goes to stdout, preserving xi's
  /// stdout/stderr split: diagnostics go to stderr via `tracing`.
  ///
  /// # Errors
  ///
  /// Propagates config-load errors, I/O errors from the state-dir load,
  /// and JSON serialisation errors.
  pub fn run(self, scope: Scope) -> Result<()> {
    let paths = ScopePaths::resolve(scope);
    let cli = SourcesConfig {
      remote: self.remote,
      ..SourcesConfig::default()
    };
    let resolved = resolve_layers(cli, &paths)?;

    // The config ladder is not the only place a remote can come from:
    // `xi <scope> switch <remote-url>` records what it adopted in the
    // state dir, and that machine may have no `.xi.toml` at all. Fall
    // back to it so `status` can name the remote the machine is
    // actually deployed from, rather than reporting an empty identity.
    let mut resolved = resolved;
    let remote = if let Some(remote) = resolved.remote.clone() {
      remote
    } else {
      let recorded = super::read_state_file(&paths.state_dir, "remote-url")?;
      if recorded.is_some() {
        // Name the rung that actually answered. Leaving this as
        // `default` would be wrong twice over: there is no built-in
        // default remote, and `--explain` exists precisely to say
        // where a value came from.
        resolved
          .provenance
          .0
          .insert("remote".to_owned(), Provenance::StateDir);
      }
      recorded.unwrap_or_default()
    };
    let sources_id = SourcesId::new(scope, &remote);

    // Read the lock before the model, and without acquiring it: `status` is
    // the reader path `xi-sources-race` explicitly exempts, so it must stay
    // answerable while a daemon tick or a push holds the lock.
    let lock = super::lock::status(&paths.state_dir)?;

    let context =
      SourcesContext::load(sources_id, paths.sources_dir, paths.state_dir)?;

    // Gitops health is owned by `xi-sources-gitops`; until that daemon
    // exists there is nothing tracking this source.
    let status = context.status(&GitopsHealth::Disabled);

    let mut response = SourcesStatusResponse::new(status).with_lock(lock);
    if self.explain {
      response = response.with_provenance(resolved.provenance);
    }
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
  }
}

/// Build the precedence ladder for one invocation.
///
/// Rungs, highest first: the CLI flags the caller already parsed, the
/// `XI_SOURCES_*` environment, the project `.xi.toml [sources]`, and the
/// Nix-module-written per-scope `sources.toml`. The fifth rung
/// (built-in defaults) is supplied by [`Layers::resolve`].
///
/// # Errors
///
/// Propagates the load-time validation error from whichever surface is
/// malformed, so a bad value is reported with its file+line or variable
/// name rather than at first fetch.
pub fn resolve_layers(
  cli: SourcesConfig,
  paths: &ScopePaths,
) -> Result<ResolvedSources> {
  let layers = Layers {
    cli,
    env: SourcesConfig::from_env()?,
    toml: SourcesConfig::from_toml_file(std::path::Path::new(".xi.toml"))?,
    nix_module: SourcesConfig::from_toml_file(&paths.config_file)?,
  };

  let resolved = layers.resolve();
  resolved
    .check_consistency()
    .map_err(|e| color_eyre::eyre::eyre!(e))?;
  Ok(resolved)
}
