use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use xi_core::installable::CommandContext;
use xi_core::{
  args::CommonRebuildArgs,
  checks::{FeatureRequirements, FlakeFeatures},
  complete,
  update::UpdateArgs,
};

/// system-manager functionality
///
/// Manages non-NixOS Linux systems using system-manager
#[derive(Debug, Args)]
pub struct SystemArgs {
  #[command(subcommand)]
  pub subcommand: SystemSubcommand,
}

impl SystemArgs {
  #[must_use]
  pub fn get_feature_requirements(&self) -> Box<dyn FeatureRequirements> {
    match &self.subcommand {
      SystemSubcommand::Switch(_) | SystemSubcommand::Build(_) => {
        Box::new(FlakeFeatures)
      },
    }
  }
}

#[derive(Debug, Subcommand)]
pub enum SystemSubcommand {
  /// Build and activate a system-manager configuration
  Switch(SystemRebuildArgs),
  /// Build a system-manager configuration
  Build(SystemRebuildArgs),
}

#[derive(Debug, Args)]
pub struct SystemRebuildArgs {
  #[command(flatten)]
  pub common: CommonRebuildArgs,

  #[command(flatten)]
  pub update_args: UpdateArgs,

  /// When using a flake installable, select this hostname from
  /// systemConfigs
  ///
  /// When unspecified, defaults to the local hostname
  #[arg(long, short = 'H', global = true, add = ArgValueCompleter::new(complete::complete_system_configs))]
  pub hostname: Option<String>,

  /// Extra arguments passed to nix build
  #[arg(last = true)]
  pub extra_args: Vec<String>,

  /// Don't panic if calling xi as root
  #[arg(short = 'R', long, env = "XI_BYPASS_ROOT_CHECK")]
  pub bypass_root_check: bool,

  /// Show activation logs
  #[arg(long, env = "XI_SHOW_ACTIVATION_LOGS", value_parser = clap::builder::BoolishValueParser::new())]
  pub show_activation_logs: bool,
}

impl SystemRebuildArgs {
  #[must_use]
  pub fn uses_flakes(&self) -> bool {
    self.common.installable.uses_flakes(CommandContext::System)
  }
}
