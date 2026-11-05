//! Pluggable deployment backends for xi.
//!
//! This crate provides a [`DeployBackend`] trait that abstracts over
//! different Nix deployment tools (deploy-rs, colmena, xi's built-in
//! remote rebuild, etc.).  The main entry point is [`detect_and_deploy`],
//! which probes the flake for known deployment outputs and dispatches
//! to the appropriate backend.

pub mod args;
pub mod backend;
pub mod decision;
pub mod fleet_digest;
pub mod plan;
pub mod push;
pub mod scope;
pub mod target;
pub mod unified_args;

use color_eyre::{Result, eyre::bail};
// Re-exports for convenience.
pub use target::{DeployProfile, DeployTarget, ProfileKind};
use tracing::info;

use crate::{
  args::DeployArgs,
  backend::DeployBackend,
  push::{
    args::PushBackend,
    discover::{self, ResolvedBackend},
    engine::CliNix,
  },
};

/// Detect the deployment backend from the flake and run the deployment.
///
/// Detection order (first match wins):
/// 1. Explicit `--backend` CLI flag
/// 2. `deploy` output → deploy-rs
/// 3. `colmenaHive` output → colmena
/// 4. `nixosConfigurations` with `--target-host` → builtin
///
/// # Errors
///
/// Returns an error if no backend can be detected or if deployment fails.
pub fn detect_and_deploy(args: &DeployArgs) -> Result<()> {
  let flake_ref = args.flake_ref.as_deref().unwrap_or(".");

  let backend: Box<dyn DeployBackend> = if let Some(ref name) = args.backend {
    resolve_explicit_backend(name, flake_ref)?
  } else {
    detect_backend(flake_ref)
  };

  info!("Using {} backend", backend.name());

  backend.deploy(flake_ref, args)
}

/// Resolve a backend by explicit name.
fn resolve_explicit_backend(
  name: &str,
  flake_ref: &str,
) -> Result<Box<dyn DeployBackend>> {
  match name {
    "deploy-rs" => {
      Ok(Box::new(backend::deploy_rs::DeployRsBackend::new(
        flake_ref,
      )?))
    },
    "colmena" => Ok(Box::new(backend::colmena::ColmenaBackend::new()?)),
    "builtin" => Ok(Box::new(backend::builtin::BuiltinBackend)),
    other => {
      bail!(
        "Unknown deploy backend '{other}'. Available: deploy-rs, colmena, \
         builtin"
      )
    },
  }
}

/// Auto-detect backend by discovering the flake's targets and applying
/// the one precedence table `push::discover::detect_backend` owns, so
/// this entrypoint and `xi push`'s never disagree on the same flake.
fn detect_backend(flake_ref: &str) -> Box<dyn DeployBackend> {
  let nix = CliNix;
  let targets = discover::discover(&nix, flake_ref).unwrap_or_default();
  let colmena_hive_present =
    discover::has_colmena_hive(&nix, flake_ref).unwrap_or(false);

  match discover::detect_backend(
    PushBackend::Auto,
    &targets,
    colmena_hive_present,
  ) {
    ResolvedBackend::DeployRs => {
      if let Ok(backend) = backend::deploy_rs::DeployRsBackend::new(flake_ref) {
        return Box::new(backend);
      }
    },
    ResolvedBackend::Colmena => {
      if let Ok(backend) = backend::colmena::ColmenaBackend::new() {
        return Box::new(backend);
      }
    },
    ResolvedBackend::Builtin => {},
  }

  // Always available as a fallback, including when the detected
  // backend's own construction fails (e.g. a malformed `deploy`
  // config, or `colmena` missing from `PATH`).
  Box::new(backend::builtin::BuiltinBackend)
}
