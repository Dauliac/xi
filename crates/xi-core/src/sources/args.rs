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
  cache_prestage::{self, PrestageOptions, PrestageStatus},
  config::{Layers, Provenance, ResolvedSources, ScopePaths, SourcesConfig},
  conflict::{
    ObservedRefs,
    SourcesDiffResponse,
    SourcesSyncResponse,
    SyncRequest,
    sync,
    three_way_diff,
  },
  events::{self, EventKind, EventsQuery},
  lock::{ActorName, LockOptions, SourcesLock},
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
  /// Prestage, unstage or inspect this scope's binary-cache config
  Cache(CacheProxy),
  /// Read the gitops daemon's structured log for this scope
  Log(LogArgs),
  /// List the gitops daemon's structured events for this scope
  Events(EventsArgs),
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

/// `xi <scope> sources events` - the event-store readout verb.
#[derive(Debug, Clone, Args)]
pub struct EventsArgs {
  /// Only show events at or after this RFC 3339 timestamp.
  #[arg(long)]
  pub since: Option<String>,

  /// Only show events of this kind (fetch|advance|verify|build|activate|
  /// hook|lock-wait).
  #[arg(long)]
  pub kind: Option<String>,

  /// Show at most this many events.
  #[arg(long)]
  pub limit: Option<u32>,

  /// Emit the house JSON envelope instead of a plain table.
  #[arg(long)]
  pub json: bool,
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

/// `xi <scope> sources cache ...`: prestage, unstage, status.
#[derive(Debug, Clone, Args)]
pub struct CacheProxy {
  #[clap(subcommand)]
  pub command: CacheCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CacheCommand {
  /// Discover this scope's declared caches and write them before the
  /// first build
  Prestage(CachePrestageArgs),
  /// Reverse a prestage, restoring `nix.conf` byte-for-byte
  Unstage(CacheUnstageArgs),
  /// Report whether a prestage is active and what it put in effect
  Status(CacheStatusArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CachePrestageArgs {
  /// `<flake-ref>[#<attr>]` to pure-eval the cache settings from.
  ///
  /// Defaults to this scope's own sources dir, and the attribute path
  /// defaults to the well-known candidates for this scope's hostname
  /// and user when no `#<attr>` is given.
  pub target: Option<String>,

  /// Skip the HTTPS key check and take the flake's declared keys on
  /// faith.
  ///
  /// For airgapped setups and caches with no metadata endpoint. Prints
  /// a warning naming every key it waves through.
  #[arg(long)]
  pub trust_declared_keys: bool,

  /// Emit the house JSON envelope instead of a human summary.
  #[arg(long)]
  pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct CacheUnstageArgs {
  /// Emit the house JSON envelope instead of a human summary.
  #[arg(long)]
  pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct CacheStatusArgs {
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
      SourcesCommand::Cache(proxy) => proxy.run(scope),
      SourcesCommand::Log(args) => args.run(scope),
      SourcesCommand::Events(args) => args.run(scope),
    }
  }
}

impl CacheProxy {
  /// Dispatch the selected cache verb at `scope`.
  ///
  /// # Errors
  ///
  /// Propagates lock, discovery, I/O and serialisation errors.
  pub fn run(self, scope: Scope) -> Result<()> {
    match self.command {
      CacheCommand::Prestage(args) => args.run(scope),
      CacheCommand::Unstage(args) => args.run(scope),
      CacheCommand::Status(args) => args.run(scope),
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

/// Take the sources lock for a cache mutation, or say who has it.
///
/// `ActorName::CachePrestage` is the lowest-priority actor: it only
/// warms a cache, so it is correct for it to queue behind a human at a
/// terminal, an inbound push, bootstrap or a daemon tick. See
/// `openspec/changes/xi-sources-cache-prestage/design.md` and xi-wtrq.
fn take_cache_lock(paths: &ScopePaths, intent: String) -> Result<SourcesLock> {
  SourcesLock::acquire(
    &paths.state_dir,
    &LockOptions::new(ActorName::CachePrestage, intent),
  )
  .map_err(|err| eyre!("{err}"))
}

impl CachePrestageArgs {
  /// Discover and write this scope's cache config.
  ///
  /// The sources lock is taken inside [`cache_prestage::Prestage::run`],
  /// not here: it covers the read-only discovery too, so a write cannot
  /// race a build triggered the moment it lands. Taking it a second time
  /// at this layer would self-deadlock against the same lock file.
  ///
  /// # Errors
  ///
  /// Propagates the lock, the hostname lookup (when no `#<attr>` is
  /// given), the discovery reader, the key guard and the write.
  pub fn run(self, scope: Scope) -> Result<()> {
    let target = self
      .target
      .unwrap_or_else(|| cache_prestage::default_flake_ref(scope));
    let (flake_ref, attr) = cache_prestage::split_target(&target);
    let candidates = cache_prestage::candidates_for(attr, None)?;

    let live = cache_prestage::Prestage::live(scope);
    let report = live
      .as_prestage()
      .run(&flake_ref, &candidates, &PrestageOptions {
        trust_declared_keys: self.trust_declared_keys,
      })
      .map_err(|err| eyre!("{err}"))?;

    if self.json {
      println!("{}", serde_json::to_string_pretty(&report)?);
      return Ok(());
    }

    match report.status {
      PrestageStatus::Applied => {
        println!(
          "prestaged {} substituter(s) from {} ({}): wrote {}",
          report.settings.substituters.len(),
          flake_ref,
          report.attr_path.as_deref().unwrap_or("?"),
          report.paths.xi_conf.display()
        );
      },
      PrestageStatus::Unchanged => {
        println!(
          "cache config from {flake_ref} already matches {}; nothing to do",
          report.paths.xi_conf.display()
        );
      },
      PrestageStatus::Missing => {
        println!(
          "{flake_ref} declares no cache settings at any well-known attribute \
           path; nothing was written"
        );
      },
    }
    Ok(())
  }
}

impl CacheUnstageArgs {
  /// Reverse a prestage, restoring `nix.conf` byte-for-byte.
  ///
  /// # Errors
  ///
  /// Propagates the lock and [`cache_prestage::PrestageError`]: no
  /// receipt to reverse, or `nix.conf` was edited since the prestage.
  pub fn run(self, scope: Scope) -> Result<()> {
    let paths = ScopePaths::resolve(scope);
    let _lock = take_cache_lock(
      &paths,
      format!("xi {} sources cache unstage", scope.as_str()),
    )?;

    let live = cache_prestage::Prestage::live(scope);
    let report = live.as_prestage().unstage().map_err(|err| eyre!("{err}"))?;

    if self.json {
      println!("{}", serde_json::to_string_pretty(&report)?);
      return Ok(());
    }

    if report.include_removed || report.xi_conf_removed {
      println!(
        "restored {}; removed {}",
        report.nix_conf.display(),
        if report.xi_conf_removed {
          "xi.conf"
        } else {
          "the !include line"
        }
      );
    } else {
      println!(
        "{} was already clean; nothing to undo",
        report.nix_conf.display()
      );
    }
    Ok(())
  }
}

impl CacheStatusArgs {
  /// Report whether a prestage is active for this scope.
  ///
  /// Reads the fragment and the receipt rather than taking the lock:
  /// like `sources status`, this is the reader path the race spec
  /// exempts, so it stays answerable while a prestage or another
  /// actor holds the lock.
  ///
  /// # Errors
  ///
  /// Propagates read failures from [`cache_prestage::Prestage::status`].
  pub fn run(self, scope: Scope) -> Result<()> {
    let live = cache_prestage::Prestage::live(scope);
    let report = live.as_prestage().status().map_err(|err| eyre!("{err}"))?;

    if self.json {
      println!("{}", serde_json::to_string_pretty(&report)?);
      return Ok(());
    }

    if report.active {
      println!(
        "cache prestage active: {} substituter(s) via {}",
        report.settings.substituters.len(),
        report.xi_conf.display()
      );
    } else {
      println!("cache prestage inactive for this scope");
    }
    Ok(())
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

impl EventsArgs {
  /// List events from the resolved scope's event store.
  ///
  /// # Errors
  ///
  /// Returns an error if `--since` or `--kind` fail to parse, if reading
  /// the store fails, or if JSON serialisation fails.
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
    let kind = self
      .kind
      .as_deref()
      .map(EventKind::parse)
      .transpose()
      .map_err(|e| eyre!(e))?;

    let query = EventsQuery {
      since,
      kind,
      limit: self.limit,
    };
    let paths = ScopePaths::resolve(scope);
    let response = events::events(scope, &paths.state_dir, &query)
      .map_err(|e| eyre!("reading the {scope} sources events failed: {e}"))?;

    if self.json {
      println!("{}", serde_json::to_string_pretty(&response)?);
      return Ok(());
    }

    if response.events.is_empty() {
      println!("no events recorded for {scope} scope yet");
      return Ok(());
    }

    println!(
      "{:<24} {:<10} {:<10} {:<8} {:<10}  MESSAGE",
      "TIMESTAMP", "KIND", "OUTCOME", "MS", "SHA"
    );
    for event in &response.events {
      let sha = event
        .new_sha
        .as_deref()
        .map_or_else(String::new, |sha| sha.chars().take(10).collect());
      println!(
        "{:<24} {:<10} {:<10} {:<8} {:<10}  {}",
        event.ts,
        event.kind,
        event.outcome.as_deref().unwrap_or("-"),
        event
          .duration_ms
          .map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
        sha,
        event.message.as_deref().unwrap_or(""),
      );
    }
    Ok(())
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
  /// Takes the sources lock (`ActorName::Sync`) before the copy, which is
  /// the first write: a daemon tick or a switch could otherwise observe
  /// or produce a half-written working dir. Released by `Drop` once this
  /// returns, success or failure alike.
  ///
  /// # Errors
  ///
  /// Returns an error when no working dir is configured (there is
  /// nothing to sync into), when the sources lock is held by another
  /// actor (the error names the holder and the estimated wait), when the
  /// working dir is dirty and `--force-dirty` was not given, and on any
  /// git or filesystem failure during the copy.
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

    let intent = format!("xi {} sources sync", scope.as_str());
    let opts = LockOptions::from_env(ActorName::Sync, intent)
      .map_err(|err| eyre!("{err}"))?
      .with_progress();
    let _lock = SourcesLock::acquire(&paths.state_dir, &opts)
      .map_err(|err| eyre!("{err}"))?;

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
