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
    engine::{CliNix, Nix},
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
  detect_backend_with(&CliNix, flake_ref)
}

/// [`detect_backend`], parameterized over [`Nix`] so a test double can
/// stand in for the real `nix` CLI and prove this flake-probing
/// entrypoint resolves the same backend `push::discover::detect_backend`
/// (the target-list entrypoint) does for the same flake.
fn detect_backend_with(nix: &dyn Nix, flake_ref: &str) -> Box<dyn DeployBackend> {
  match resolve_backend(nix, flake_ref) {
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

/// The one shared backend-precedence decision: discover `flake_ref`'s
/// targets through `nix` and hand them to
/// [`discover::detect_backend`] exactly as the target-list entrypoint
/// (`xi push`) does. Kept separate from backend *construction* above,
/// which can fail independently of this decision (a malformed `deploy`
/// output, `colmena` missing from `PATH`) and is not what the
/// collapsed precedence table is about.
fn resolve_backend(nix: &dyn Nix, flake_ref: &str) -> ResolvedBackend {
  let targets = discover::discover(nix, flake_ref).unwrap_or_default();
  let colmena_hive_present =
    discover::has_colmena_hive(nix, flake_ref).unwrap_or(false);
  discover::detect_backend(PushBackend::Auto, &targets, colmena_hive_present)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  /// Answers flake evaluation from a fixture table keyed by attr,
  /// mirroring `bdd_push_builtin.rs`'s `FixtureNix`: absence from the
  /// table reads as "no such output", not an error.
  struct FixtureNix {
    outputs: Vec<(&'static str, serde_json::Value)>,
  }

  impl FixtureNix {
    fn only_deploy_node(name: &'static str) -> Self {
      Self {
        outputs: vec![(
          ".#deploy",
          serde_json::json!({
            "nodes": { name: { "hostname": name } }
          }),
        )],
      }
    }

    fn only_nixos_configuration(name: &'static str) -> Self {
      Self {
        outputs: vec![(".#nixosConfigurations", serde_json::json!([name]))],
      }
    }
  }

  impl Nix for FixtureNix {
    fn eval_json(
      &self,
      installable: &str,
      _apply: Option<&str>,
    ) -> Result<Option<serde_json::Value>> {
      Ok(
        self
          .outputs
          .iter()
          .find(|(attr, _)| *attr == installable)
          .map(|(_, value)| value.clone()),
      )
    }

    fn build(&self, installable: &str) -> Result<String> {
      Ok(format!("/nix/store/fake-{installable}"))
    }
  }

  /// SC-UA-005 / the spec's "Both callers agree on the same fleet"
  /// scenario: the flake-probing entrypoint (this crate's own
  /// `detect_backend`, used by the legacy `xi deploy` verb) and the
  /// target-list entrypoint (`push::discover::detect_backend`, used by
  /// `xi push`) must resolve the same backend for the same flake,
  /// because the former is now a thin wrapper over the latter rather
  /// than a second, independently maintained precedence check.
  #[test]
  fn flake_probing_and_target_list_resolve_the_same_backend_for_a_deploy_node() {
    let nix = FixtureNix::only_deploy_node("web1");

    let targets = discover::discover(&nix, ".").expect("discovers");
    let colmena_hive_present =
      discover::has_colmena_hive(&nix, ".").expect("checks colmena hive");
    let target_list_backend =
      discover::detect_backend(PushBackend::Auto, &targets, colmena_hive_present);
    assert_eq!(target_list_backend, ResolvedBackend::DeployRs);

    let flake_probing_backend = resolve_backend(&nix, ".");
    assert_eq!(flake_probing_backend, target_list_backend);
  }

  #[test]
  fn flake_probing_and_target_list_resolve_the_same_backend_for_a_bare_nixos_configuration()
   {
    let nix = FixtureNix::only_nixos_configuration("web2");

    let targets = discover::discover(&nix, ".").expect("discovers");
    let colmena_hive_present =
      discover::has_colmena_hive(&nix, ".").expect("checks colmena hive");
    let target_list_backend =
      discover::detect_backend(PushBackend::Auto, &targets, colmena_hive_present);
    assert_eq!(target_list_backend, ResolvedBackend::Builtin);

    let flake_probing_backend = resolve_backend(&nix, ".");
    assert_eq!(flake_probing_backend, target_list_backend);

    // Builtin never fails to construct, so the full wrapper agrees too.
    let flake_probing_backend_boxed = detect_backend_with(&nix, ".");
    assert_eq!(flake_probing_backend_boxed.name(), "builtin");
  }
}
