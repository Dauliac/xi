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
};
use xi_remote::RemoteHost;

#[derive(Debug, Subcommand)]
pub enum HomeSubcommand {
  /// Build and activate a home-manager configuration
  Switch(HomeRebuildArgs),

  /// Build a home-manager configuration
  Build(HomeRebuildArgs),

  /// Load a home-manager configuration in a Nix REPL
  Repl(HomeReplArgs),
}

#[derive(Debug, Args)]
/// Home-manager functionality
pub struct HomeArgs {
  #[command(subcommand)]
  pub subcommand: HomeSubcommand,
}

impl HomeArgs {
  #[must_use]
  pub fn get_feature_requirements(&self) -> Box<dyn FeatureRequirements> {
    match &self.subcommand {
      HomeSubcommand::Repl(args) => {
        let is_flake = args.uses_flakes();
        Box::new(ReplFeatures {
          is_flake,
          variant: ReplVariant::Home,
        })
      },
      HomeSubcommand::Switch(args) | HomeSubcommand::Build(args) => {
        if args.uses_flakes() {
          Box::new(FlakeFeatures)
        } else {
          Box::new(LegacyFeatures)
        }
      },
    }
  }
}

#[derive(Debug, Args)]
pub struct HomeRebuildArgs {
  #[command(flatten)]
  pub common: CommonRebuildArgs,

  #[command(flatten)]
  pub update_args: xi_core::update::UpdateArgs,

  /// Name of the flake homeConfigurations attribute, like username@hostname
  ///
  /// If unspecified, will try `<username>@<hostname>` and `<username>`
  #[arg(long, short, add = ArgValueCompleter::new(complete::complete_home_configs))]
  pub configuration: Option<String>,

  /// Explicitly select some specialisation
  #[arg(long, short)]
  pub specialisation: Option<String>,

  /// Ignore specialisations
  #[arg(long, short = 'S')]
  pub no_specialisation: bool,

  /// Extra arguments passed to nix build
  #[arg(last = true)]
  pub extra_args: Vec<String>,

  /// Move existing files by backing up with this file extension
  #[arg(long, short = 'b')]
  pub backup_extension: Option<String>,

  /// Show activation logs
  #[arg(long, env = "XI_SHOW_ACTIVATION_LOGS", value_parser = clap::builder::BoolishValueParser::new())]
  pub show_activation_logs: bool,

  /// Build the configuration on a different host over SSH
  #[arg(long)]
  pub build_host: Option<RemoteHost>,
}

impl HomeRebuildArgs {
  #[must_use]
  pub fn uses_flakes(&self) -> bool {
    self.common.installable.uses_flakes(CommandContext::Home)
  }
}

#[derive(Debug, Args)]
pub struct HomeReplArgs {
  #[command(flatten)]
  pub installable: InstallableArgs,

  /// Name of the flake homeConfigurations attribute, like username@hostname
  ///
  /// If unspecified, will try `<username>@<hostname>` and `<username>`
  #[arg(long, short, add = ArgValueCompleter::new(complete::complete_home_configs))]
  pub configuration: Option<String>,

  /// Extra arguments passed to nix repl
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

impl HomeReplArgs {
  #[must_use]
  pub fn uses_flakes(&self) -> bool {
    self.installable.uses_flakes(CommandContext::Home)
  }
}

impl HasCacheArgs for HomeArgs {
  fn cache_args_mut(&mut self) -> Option<&mut CacheArgs> {
    match &mut self.subcommand {
      HomeSubcommand::Switch(a) | HomeSubcommand::Build(a) => {
        Some(&mut a.common.cache)
      },
      HomeSubcommand::Repl(_) => None,
    }
  }
}

impl HasBuildArgs for HomeArgs {
  fn build_passthrough_mut(&mut self) -> Option<&mut NixBuildPassthroughArgs> {
    match &mut self.subcommand {
      HomeSubcommand::Switch(a) | HomeSubcommand::Build(a) => {
        Some(&mut a.common.passthrough)
      },
      HomeSubcommand::Repl(_) => None,
    }
  }

  fn no_nom_mut(&mut self) -> Option<&mut bool> {
    match &mut self.subcommand {
      HomeSubcommand::Switch(a) | HomeSubcommand::Build(a) => {
        Some(&mut a.common.no_nom)
      },
      HomeSubcommand::Repl(_) => None,
    }
  }
}
