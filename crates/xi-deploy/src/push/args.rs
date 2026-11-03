//! The `xi push` command surface.
//!
//! Every flag here is named by the `xi-sources-push` spec's "Verb
//! `xi push`" requirement, with one addition: `--scope`. `push` is a
//! top-level verb, so unlike `xi <scope> sources` it has no parent verb
//! to inherit the scope from and has to be able to say it.

use std::{fmt, str::FromStr};

use clap::Args;
use color_eyre::{Result, eyre::eyre};
use xi_core::sources::Scope;

/// Which deployment backend carries the push.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum PushBackend {
  /// Probe the flake: `deploy.*` → `colmenaHive` → `builtin`.
  #[default]
  Auto,
  /// deploy-rs, via its own CLI.
  DeployRs,
  /// Colmena, via its own CLI.
  Colmena,
  /// xi's own SSH pipeline.
  Builtin,
}

impl PushBackend {
  /// The name `xi-deploy`'s backend registry knows this choice by.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Auto => "auto",
      Self::DeployRs => "deploy-rs",
      Self::Colmena => "colmena",
      Self::Builtin => "builtin",
    }
  }
}

/// Whether the operator or the target does the evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum PushMode {
  /// Operator evaluates and builds; the target receives a closure.
  #[default]
  Closure,
  /// The sources dir is synced to the target, which rebuilds from it.
  Sources,
}

/// Who pulls the closure's store paths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum CopyVia {
  /// The operator uploads the closure. Correct on a slow target link.
  #[default]
  Local,
  /// The target substitutes from its own binary caches.
  Target,
}

/// Which half of the machine the push governs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum PushScope {
  /// Host-wide configuration.
  #[default]
  System,
  /// Per-user configuration.
  User,
}

impl From<PushScope> for Scope {
  fn from(scope: PushScope) -> Self {
    match scope {
      PushScope::System => Self::System,
      PushScope::User => Self::User,
    }
  }
}

/// One stage of the bring-up sequence.
///
/// Only [`Phase::Activate`] is implemented by this capability. The other
/// three are the fresh-machine path and belong to `xi-sources-bootstrap`,
/// which drives them through this same verb; they parse here so the flag
/// does not have to change shape when that change lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
  /// Boot the target into a kexec'd installer image.
  Kexec,
  /// Partition and format the target's disks.
  Disko,
  /// Install a first generation onto fresh disks.
  Install,
  /// Switch a running system to a new generation.
  Activate,
}

impl Phase {
  /// Every phase, in execution order.
  pub const ALL: &'static [Self] =
    &[Self::Kexec, Self::Disko, Self::Install, Self::Activate];

  /// The CSV spelling of this phase.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Kexec => "kexec",
      Self::Disko => "disko",
      Self::Install => "install",
      Self::Activate => "activate",
    }
  }

  /// Whether `xi-sources-push` implements this phase itself.
  #[must_use]
  pub const fn is_implemented(self) -> bool {
    matches!(self, Self::Activate)
  }
}

impl fmt::Display for Phase {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

impl FromStr for Phase {
  type Err = String;

  fn from_str(raw: &str) -> Result<Self, String> {
    match raw.trim() {
      "kexec" => Ok(Self::Kexec),
      "disko" => Ok(Self::Disko),
      "install" => Ok(Self::Install),
      "activate" => Ok(Self::Activate),
      other => {
        Err(format!(
          "unknown phase '{other}'; expected one of {}",
          Self::ALL
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(", ")
        ))
      },
    }
  }
}

/// Parse the `--phase` CSV into a deduplicated, ordered set.
///
/// # Errors
///
/// Returns an error naming the unknown token, or reporting an empty list.
pub fn parse_phases(raw: &str) -> Result<Vec<Phase>, String> {
  let mut phases: Vec<Phase> = raw
    .split(',')
    .map(str::trim)
    .filter(|token| !token.is_empty())
    .map(Phase::from_str)
    .collect::<Result<Vec<_>, _>>()?;
  if phases.is_empty() {
    return Err("--phase needs at least one phase".to_owned());
  }
  phases.sort_unstable();
  phases.dedup();
  Ok(phases)
}

/// Push the resolved sources configuration to remote machines
///
/// Resolves the sources context for the scope first, so every target is
/// deployed from the same commit the local machine would switch to. Picks
/// the deployment backend from the flake unless told otherwise.
#[derive(Debug, Clone, Args)]
pub struct PushArgs {
  /// Targets to push to (pushes to every discovered target if none given)
  #[arg(trailing_var_arg = true)]
  pub targets: Vec<String>,

  /// Filter discovered targets by tag (`@tag`) or by regex
  #[arg(long)]
  pub on: Option<String>,

  /// Force a specific deployment backend
  #[arg(long, value_enum, default_value_t = PushBackend::Auto)]
  pub backend: PushBackend,

  /// Bring-up phases to run, comma separated
  #[arg(long, default_value = "activate", value_name = "CSV")]
  pub phase: String,

  /// Evaluate locally and copy a closure, or sync the sources dir
  #[arg(long, value_enum, default_value_t = PushMode::Closure)]
  pub mode: PushMode,

  /// Seconds to confirm the new generation in before rolling back (0 = off)
  #[arg(long, default_value_t = 0, value_name = "SECONDS")]
  pub magic_rollback: u64,

  /// Number of targets to activate concurrently
  #[arg(long, default_value_t = 1, value_name = "N")]
  pub parallel: usize,

  /// Allocate a PTY so a remote sudo password prompt reaches the terminal
  #[arg(long)]
  pub interactive_sudo: bool,

  /// Upload the closure from here, or let the target substitute it
  #[arg(long, value_enum, default_value_t = CopyVia::Local)]
  pub copy_via: CopyVia,

  /// Stop at the first failing target instead of finishing the rest
  #[arg(long)]
  pub fail_fast: bool,

  /// Push even though the working dir has uncommitted changes
  #[arg(long)]
  pub force_dirty: bool,

  /// Which half of the machine to push
  ///
  /// `push` is top-level, so it cannot take the scope from a parent verb
  /// the way `xi <scope> sources` does.
  #[arg(long, value_enum, default_value_t = PushScope::System)]
  pub scope: PushScope,
}

impl PushArgs {
  /// The parsed `--phase` set.
  ///
  /// # Errors
  ///
  /// Returns an error for an unknown or empty phase list.
  pub fn phases(&self) -> Result<Vec<Phase>> {
    parse_phases(&self.phase).map_err(|e| eyre!(e))
  }

  /// The resolved sources scope.
  #[must_use]
  pub fn sources_scope(&self) -> Scope {
    self.scope.into()
  }

  /// Reject flag combinations that cannot both be honoured.
  ///
  /// # Errors
  ///
  /// Returns an error naming the two flags and why they conflict.
  pub fn validate(&self) -> Result<()> {
    if self.parallel == 0 {
      return Err(eyre!("--parallel needs at least 1"));
    }

    let phases = self.phases()?;
    if let Some(unimplemented) =
      phases.iter().find(|phase| !phase.is_implemented())
    {
      return Err(eyre!(
        "--phase {unimplemented} is owned by the xi-sources-bootstrap \
         capability, which is not implemented yet. Only `activate` is \
         available today."
      ));
    }

    if self.mode == PushMode::Sources && self.copy_via == CopyVia::Target {
      return Err(eyre!(
        "--mode sources never copies a closure from this machine, so \
         --copy-via target has nothing to choose; drop one of the two flags"
      ));
    }

    Ok(())
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn phases_parse_deduplicated_and_ordered() {
    assert_eq!(
      parse_phases("activate,disko,activate"),
      Ok(vec![Phase::Disko, Phase::Activate])
    );
  }

  #[test]
  fn an_unknown_phase_names_the_valid_set() {
    let err = parse_phases("activaet").expect_err("unknown phase");
    assert!(err.contains("activate"), "got: {err}");
  }

  #[test]
  fn an_empty_phase_list_is_rejected() {
    assert!(parse_phases(" , ").is_err());
  }

  #[test]
  fn only_activate_is_implemented_today() {
    assert!(Phase::Activate.is_implemented());
    for phase in [Phase::Kexec, Phase::Disko, Phase::Install] {
      assert!(!phase.is_implemented(), "{phase} should defer to bootstrap");
    }
  }
}
