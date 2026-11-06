//! CLI wiring for the `xi-sources-bootstrap` capability.
//!
//! The planner, preflight, installer delegation, and the reversible
//! receipt all live in [`xi_core::sources::bootstrap`] - that module is
//! the capability's engine, fully exercised against [`RecordingRunner`]
//! so none of its tests touch a real machine. This module is the thin
//! binary-crate layer on top of it: it resolves this scope's paths,
//! hands the engine the one real [`SystemRunner`], and turns its
//! `Result` into the user-facing exit path every other `xi` command
//! uses.
//!
//! Today that layer is `xi system uninstall`
//! ([`xi_core::sources::bootstrap::uninstall`]); the forward bootstrap
//! dispatch (`xi system switch <remote-url>` on a bare machine) and the
//! `ssh://` remote form are planner-complete and unit-tested in the
//! engine but not yet reachable from this binary - see
//! `openspec/changes/xi-sources-bootstrap/specs/xi-sources-bootstrap/spec.md`.
//!
//! [`RecordingRunner`]: xi_core::sources::bootstrap::RecordingRunner

use color_eyre::{Result, eyre::eyre};
use tracing::info;
use xi_core::sources::{
  Scope,
  bootstrap::{SystemRunner, receipt_path, uninstall},
  config::ScopePaths,
};

/// Run `xi <scope> uninstall`: reverse a prior bootstrap's receipt.
///
/// Reads the receipt [`xi_core::sources::bootstrap::BootstrapPlan`]
/// wrote for `scope` and replays every recorded reversal in last-applied
/// first order, exactly as [`uninstall`] documents. The receipt path
/// honours `XI_SOURCES_STATE_DIR` like the rest of the sources family,
/// which is what lets this be exercised against a scratch directory in
/// tests instead of `/var/lib/xi`.
///
/// # Errors
///
/// Returns an error when no receipt exists for `scope`
/// (`BootstrapError::NoReceipt`), or when a reversal command fails
/// partway through - the receipt is left in place on that path so a
/// retry can pick up where it stopped.
pub fn run(scope: Scope) -> Result<()> {
  let paths = ScopePaths::resolve(scope);
  let path = receipt_path(scope, &paths);
  let runner = SystemRunner;

  let reversed = uninstall(&path, &runner).map_err(|err| eyre!("{err}"))?;

  for action in &reversed {
    info!("reversed {action}");
  }
  println!(
    "xi: reversed {} bootstrap action(s) that were recorded in {}",
    reversed.len(),
    path.display()
  );

  Ok(())
}
