use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use xi_core::installable::{CommandContext, InstallableArgs};
use xi_core::{
  args::{
    CacheArgs, CommonRebuildArgs, HasBuildArgs, HasCacheArgs,
    NixBuildPassthroughArgs,
  },
  checks::{
    FeatureRequirements, FlakeFeatures, LegacyFeatures, ReplFeatures,
    ReplVariant,
  },
  complete,
  update::UpdateArgs,
};
use xi_remote::RemoteHost;

/// Nix-darwin functionality
///
/// Implements functionality mostly around but not exclusive to darwin-rebuild
#[derive(Debug, Args)]
pub struct DarwinArgs {
  #[command(subcommand)]
  pub subcommand: DarwinSubcommand,
}

impl DarwinArgs {
  #[must_use]
  pub fn get_feature_requirements(&self) -> Box<dyn FeatureRequirements> {
    match &self.subcommand {
      DarwinSubcommand::Repl(args) => {
        let is_flake = args.uses_flakes();
        Box::new(ReplFeatures {
          is_flake,
          variant: ReplVariant::Darwin,
        })
      },
      DarwinSubcommand::Switch(args) | DarwinSubcommand::Build(args) => {
        if args.uses_flakes() {
          Box::new(FlakeFeatures)
        } else {
          Box::new(LegacyFeatures)
        }
      },
    }
  }
}

#[derive(Debug, Subcommand)]
pub enum DarwinSubcommand {
  /// Build and activate a nix-darwin configuration
  Switch(DarwinRebuildArgs),
  /// Build a nix-darwin configuration
  Build(DarwinRebuildArgs),
  /// Load a nix-darwin configuration in a Nix REPL
  Repl(DarwinReplArgs),
}

#[derive(Debug, Args)]
pub struct DarwinRebuildArgs {
  #[command(flatten)]
  pub common: CommonRebuildArgs,

  #[command(flatten)]
  pub update_args: UpdateArgs,

  /// When using a flake installable, select this hostname from
  /// darwinConfigurations
  #[arg(long, short = 'H', global = true, add = ArgValueCompleter::new(complete::complete_darwin_configs))]
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

  /// Build the configuration on a different host over SSH
  #[arg(long)]
  pub build_host: Option<RemoteHost>,
}

impl DarwinRebuildArgs {
  #[must_use]
  pub fn uses_flakes(&self) -> bool {
    self.common.installable.uses_flakes(CommandContext::Darwin)
  }
}

#[derive(Debug, Args)]
pub struct DarwinReplArgs {
  #[command(flatten)]
  pub installable: InstallableArgs,

  /// When using a flake installable, select this hostname from
  /// darwinConfigurations
  #[arg(long, short = 'H', global = true, add = ArgValueCompleter::new(complete::complete_darwin_configs))]
  pub hostname: Option<String>,
}

impl DarwinReplArgs {
  #[must_use]
  pub fn uses_flakes(&self) -> bool {
    self.installable.uses_flakes(CommandContext::Darwin)
  }
}

impl HasCacheArgs for DarwinArgs {
  fn cache_args_mut(&mut self) -> Option<&mut CacheArgs> {
    match &mut self.subcommand {
      DarwinSubcommand::Switch(a) | DarwinSubcommand::Build(a) => {
        Some(&mut a.common.cache)
      },
      DarwinSubcommand::Repl(_) => None,
    }
  }
}

impl HasBuildArgs for DarwinArgs {
  fn build_passthrough_mut(&mut self) -> Option<&mut NixBuildPassthroughArgs> {
    match &mut self.subcommand {
      DarwinSubcommand::Switch(a) | DarwinSubcommand::Build(a) => {
        Some(&mut a.common.passthrough)
      },
      DarwinSubcommand::Repl(_) => None,
    }
  }

  fn no_nom_mut(&mut self) -> Option<&mut bool> {
    match &mut self.subcommand {
      DarwinSubcommand::Switch(a) | DarwinSubcommand::Build(a) => {
        Some(&mut a.common.no_nom)
      },
      DarwinSubcommand::Repl(_) => None,
    }
  }
}
