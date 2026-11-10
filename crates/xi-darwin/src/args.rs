use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use xi_core::{
  args::{
    CacheArgs,
    CommonRebuildArgs,
    HasBuildArgs,
    HasCacheArgs,
    HasInstallableArgs,
    NixBuildPassthroughArgs,
  },
  checks::{
    FeatureRequirements,
    FlakeFeatures,
    LegacyFeatures,
    NoFeatures,
    ReplFeatures,
    ReplVariant,
  },
  complete,
  installable::{CommandContext, InstallableArgs},
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
      // `sources` only reads config and the state dir; it never
      // evaluates a flake, so it needs no Nix features, and critically
      // no working `nix` binary either.
      DarwinSubcommand::Sources(_) => Box::new(NoFeatures),
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
  ///
  /// Removed from the CLI (xi-fxgx.5.4, past the deprecation window
  /// xi-fxgx.5.2 opened): `xi darwin switch` no longer parses, since no
  /// variant is named `switch` anymore - renamed rather than deleted
  /// because `xi deploy`'s internal dispatch
  /// (`crates/xi/src/deploy.rs::darwin_args`) still constructs this
  /// exact variant and calls `.run()` on it directly in Rust, bypassing
  /// CLI parsing entirely. Deleting the variant outright would break
  /// that.
  #[command(hide = true, name = "__deploy_internal_switch")]
  Switch(DarwinRebuildArgs),
  /// Build a nix-darwin configuration
  Build(DarwinRebuildArgs),
  /// Load a nix-darwin configuration in a Nix REPL
  Repl(DarwinReplArgs),

  /// Inspect and manage the config repo this scope deploys from
  Sources(xi_core::sources::args::SourcesProxy),
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

impl HasInstallableArgs for DarwinArgs {
  fn installable(&self) -> Option<&InstallableArgs> {
    match &self.subcommand {
      DarwinSubcommand::Switch(a) | DarwinSubcommand::Build(a) => {
        Some(&a.common.installable)
      },
      DarwinSubcommand::Repl(_) | DarwinSubcommand::Sources(_) => None,
    }
  }
}

impl HasCacheArgs for DarwinArgs {
  fn cache_args_mut(&mut self) -> Option<&mut CacheArgs> {
    match &mut self.subcommand {
      DarwinSubcommand::Switch(a) | DarwinSubcommand::Build(a) => {
        Some(&mut a.common.cache)
      },
      DarwinSubcommand::Repl(_) | DarwinSubcommand::Sources(_) => None,
    }
  }
}

impl HasBuildArgs for DarwinArgs {
  fn build_passthrough_mut(&mut self) -> Option<&mut NixBuildPassthroughArgs> {
    match &mut self.subcommand {
      DarwinSubcommand::Switch(a) | DarwinSubcommand::Build(a) => {
        Some(&mut a.common.passthrough)
      },
      DarwinSubcommand::Repl(_) | DarwinSubcommand::Sources(_) => None,
    }
  }

  fn no_nom_mut(&mut self) -> Option<&mut bool> {
    match &mut self.subcommand {
      DarwinSubcommand::Switch(a) | DarwinSubcommand::Build(a) => {
        Some(&mut a.common.no_nom)
      },
      DarwinSubcommand::Repl(_) | DarwinSubcommand::Sources(_) => None,
    }
  }
}

impl DarwinArgs {
  /// The sources scope `xi darwin switch` operates in.
  ///
  /// Declared at parse time so the sources resolver never has to
  /// infer scope from the installable or the CWD. The mapping is
  /// fixed by the xi-sources-model scope table.
  #[must_use]
  pub const fn scope() -> xi_core::sources::Scope {
    xi_core::sources::Scope::System
  }
}
