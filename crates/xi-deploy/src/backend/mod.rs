//! Deployment backend trait and implementations.
//!
//! Each backend translates its native protocol into the common
//! [`DeployTarget`] model and implements the deployment pipeline.

pub mod builtin;
pub mod colmena;
pub mod deploy_rs;

use color_eyre::Result;

use crate::args::DeployArgs;
use crate::target::DeployTarget;

/// Trait implemented by each deployment backend.
///
/// A backend is responsible for:
/// 1. **Detection** — probing the flake to see if it's configured
/// 2. **Discovery** — listing available deployment targets
/// 3. **Deployment** — building, pushing, and activating configurations
pub trait DeployBackend {
  /// Human-readable name for this backend (e.g. "deploy-rs", "colmena").
  fn name(&self) -> &'static str;

  /// Discover available deployment targets from the flake.
  ///
  /// # Errors
  ///
  /// Returns an error if the flake cannot be evaluated.
  fn discover_targets(&self, flake_ref: &str) -> Result<Vec<DeployTarget>>;

  /// Deploy to the specified targets (or all if `args.targets` is empty).
  ///
  /// # Errors
  ///
  /// Returns an error if deployment fails.
  fn deploy(&self, flake_ref: &str, args: &DeployArgs) -> Result<()>;
}
