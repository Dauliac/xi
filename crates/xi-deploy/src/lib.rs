//! Pluggable deployment backends for xi.
//!
//! This crate provides a [`DeployBackend`] trait that abstracts over
//! different Nix deployment tools (deploy-rs, colmena, xi's built-in
//! remote rebuild, etc.).  The main entry point is [`detect_and_deploy`],
//! which probes the flake for known deployment outputs and dispatches
//! to the appropriate backend.

pub mod args;
pub mod backend;
pub mod target;

use color_eyre::Result;
use color_eyre::eyre::bail;
use tracing::info;

use crate::args::DeployArgs;
use crate::backend::DeployBackend;

// Re-exports for convenience.
pub use target::{DeployProfile, DeployTarget, ProfileKind};

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
pub fn detect_and_deploy(args: DeployArgs) -> Result<()> {
  let flake_ref = args.flake_ref.as_deref().unwrap_or(".");

  let backend: Box<dyn DeployBackend> = if let Some(ref name) = args.backend {
    resolve_explicit_backend(name, flake_ref)?
  } else {
    detect_backend(flake_ref)?
  };

  info!("Using {} backend", backend.name());

  backend.deploy(flake_ref, &args)
}

/// Resolve a backend by explicit name.
fn resolve_explicit_backend(
  name: &str,
  flake_ref: &str,
) -> Result<Box<dyn DeployBackend>> {
  match name {
    "deploy-rs" => Ok(Box::new(backend::deploy_rs::DeployRsBackend::new(
      flake_ref,
    )?)),
    "colmena" => Ok(Box::new(backend::colmena::ColmenaBackend::new()?)),
    "builtin" => Ok(Box::new(backend::builtin::BuiltinBackend)),
    other => bail!(
      "Unknown deploy backend '{other}'. \
       Available: deploy-rs, colmena, builtin"
    ),
  }
}

/// Auto-detect backend by probing flake outputs.
fn detect_backend(flake_ref: &str) -> Result<Box<dyn DeployBackend>> {
  // 1. deploy-rs: check for `deploy` output
  if let Ok(backend) = backend::deploy_rs::DeployRsBackend::new(flake_ref) {
    return Ok(Box::new(backend));
  }

  // 2. colmena: check for `colmenaHive` output
  if let Ok(backend) = backend::colmena::ColmenaBackend::new() {
    return Ok(Box::new(backend));
  }

  // 3. builtin: always available as fallback
  Ok(Box::new(backend::builtin::BuiltinBackend))
}
