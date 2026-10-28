//! The `xi sources` command surface.
//!
//! Currently exposes the read-only `status` verb defined by the
//! `xi-sources-model` capability. The envelope it prints is
//! [`xi_core::sources::SourcesStatusResponse`].
//!
//! # Provisional path resolution
//!
//! `xi-sources-config` owns the real per-scope path tables. Until it
//! lands, [`resolve_dirs`] derives both directories from the XDG state dir
//! plus the source's stable slug. The shape of the answer (which dirs
//! exist, what the status is) is already correct; only the concrete
//! prefix will move.

pub mod args;

use color_eyre::Result;
use xi_core::sources::{
  GitopsHealth,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatusResponse,
  verbless_switch_error,
};

use self::args::{SourcesCommand, SourcesProxy, StatusArgs};

impl SourcesProxy {
  /// Dispatch the selected `xi sources` verb.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from loading the state dir and serialisation
  /// errors from rendering the envelope.
  pub fn run(self) -> Result<()> {
    match self.command {
      SourcesCommand::Status(args) => args.run(),
    }
  }
}

impl StatusArgs {
  /// Print the `xi sources status` envelope on stdout.
  ///
  /// Nothing is written to stdout except the envelope, preserving the
  /// existing xi stdout/stderr split: diagnostics go to stderr via
  /// `tracing`, machine output goes here.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from the state-dir load and JSON
  /// serialisation errors.
  pub fn run(self) -> Result<()> {
    let scope = Scope::from(self.scope);

    // With no remote configured there is no identity to resolve, so the
    // answer is `Missing` by definition. Synthesising an empty id keeps
    // that path on the same code as every other status.
    let remote = self.remote.unwrap_or_default();
    let sources_id = SourcesId::new(scope, &remote);
    let (sources_dir, state_dir) = resolve_dirs(&sources_id);

    let context = SourcesContext::load(sources_id, sources_dir, state_dir)?;

    // Gitops health is owned by `xi-sources-gitops`; until that daemon
    // exists there is nothing tracking this source.
    let status = context.status(&GitopsHealth::Disabled);

    let response = SourcesStatusResponse::new(status);
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
  }
}

/// Provisional per-scope directory resolution.
///
/// Returns `(sources_dir, state_dir)`. Both live under the xi XDG state
/// dir, separated by the source's slug so two scopes tracking the same
/// remote never collide.
fn resolve_dirs(
  sources_id: &SourcesId,
) -> (std::path::PathBuf, std::path::PathBuf) {
  let root = xi_core::dirs::xdg_state_dir().join("sources");
  let slug = sources_id.slug();
  (
    root.join(&slug).join("sources"),
    root.join(&slug).join("state"),
  )
}

/// Handler for the verb-less `xi switch`.
///
/// `switch` is accepted by the parser purely so this message can name the
/// four concrete verbs; clap's own "unrecognized subcommand" error would
/// not. No sources state is touched on this path.
///
/// # Errors
///
/// Always returns an error  -  that is the entire contract.
pub fn verbless_switch() -> Result<()> {
  Err(color_eyre::eyre::eyre!(verbless_switch_error()))
}
