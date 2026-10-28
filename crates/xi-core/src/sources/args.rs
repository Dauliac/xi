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

use clap::{Args, Subcommand};
use color_eyre::Result;

use super::{
  GitopsHealth,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatusResponse,
  config::{Layers, Provenance, ResolvedSources, ScopePaths, SourcesConfig},
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
    }
  }
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

    let context =
      SourcesContext::load(sources_id, paths.sources_dir, paths.state_dir)?;

    // Gitops health is owned by `xi-sources-gitops`; until that daemon
    // exists there is nothing tracking this source.
    let status = context.status(&GitopsHealth::Disabled);

    let mut response = SourcesStatusResponse::new(status);
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
