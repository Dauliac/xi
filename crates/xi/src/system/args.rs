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
  checks::{FeatureRequirements, FlakeFeatures, NoFeatures},
  complete,
  installable::CommandContext,
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
      // `sources` and `status` only read config and the state dir; they
      // never evaluate a flake, so (unlike `switch`/`build`) they need no
      // Nix features, and critically no working `nix` binary either -
      // `sudo xi system status` must succeed even when `nix` isn't on
      // root's `PATH`.
      SystemSubcommand::Sources(_) | SystemSubcommand::Status(_) => {
        Box::new(NoFeatures)
      },
      SystemSubcommand::Switch(_) | SystemSubcommand::Build(_) => {
        Box::new(FlakeFeatures)
      },
      // `uninstall` replays recorded shell-outs from the bootstrap
      // receipt; it never evaluates a flake either.
      SystemSubcommand::Uninstall => Box::new(NoFeatures),
    }
  }
}

#[derive(Debug, Subcommand)]
pub enum SystemSubcommand {
  /// Build and activate a system-manager configuration
  ///
  /// Removed from the CLI (xi-fxgx.5.4, past the deprecation window
  /// xi-fxgx.5.2 opened): `xi system switch` no longer parses, since no
  /// variant is named `switch` anymore - renamed rather than deleted
  /// because `xi deploy`'s internal dispatch
  /// (`crates/xi/src/deploy.rs::system_args`) still constructs this
  /// exact variant and calls `.run()` on it directly in Rust, bypassing
  /// CLI parsing entirely. Deleting the variant outright would break
  /// that.
  #[command(hide = true, name = "__deploy_internal_switch")]
  Switch(SystemRebuildArgs),
  /// Build a system-manager configuration
  Build(SystemRebuildArgs),

  /// Inspect and manage the config repo this scope deploys from
  Sources(xi_core::sources::args::SourcesProxy),

  /// Report this scope's deployment: xi's version, sources git hashes,
  /// and the active generation
  Status(xi_core::sources::deployment::DeploymentStatusArgs),

  /// Reverse a prior `xi deploy --mechanism system-manager --adopt
  /// <remote-url>` bootstrap
  ///
  /// Reads `bootstrap-receipt.json` and undoes every recorded action in
  /// last-applied-first order, including a delegated Nix install.
  /// Refuses with a clear error when this host has no receipt.
  Uninstall,
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

impl HasInstallableArgs for SystemArgs {
  fn installable(&self) -> Option<&xi_core::installable::InstallableArgs> {
    match &self.subcommand {
      SystemSubcommand::Sources(_)
      | SystemSubcommand::Status(_)
      | SystemSubcommand::Uninstall => None,
      SystemSubcommand::Switch(a) | SystemSubcommand::Build(a) => {
        Some(&a.common.installable)
      },
    }
  }
}

impl HasCacheArgs for SystemArgs {
  fn cache_args_mut(&mut self) -> Option<&mut CacheArgs> {
    match &mut self.subcommand {
      SystemSubcommand::Sources(_)
      | SystemSubcommand::Status(_)
      | SystemSubcommand::Uninstall => None,
      SystemSubcommand::Switch(a) | SystemSubcommand::Build(a) => {
        Some(&mut a.common.cache)
      },
    }
  }
}

impl HasBuildArgs for SystemArgs {
  fn build_passthrough_mut(&mut self) -> Option<&mut NixBuildPassthroughArgs> {
    match &mut self.subcommand {
      SystemSubcommand::Sources(_)
      | SystemSubcommand::Status(_)
      | SystemSubcommand::Uninstall => None,
      SystemSubcommand::Switch(a) | SystemSubcommand::Build(a) => {
        Some(&mut a.common.passthrough)
      },
    }
  }

  fn no_nom_mut(&mut self) -> Option<&mut bool> {
    match &mut self.subcommand {
      SystemSubcommand::Sources(_)
      | SystemSubcommand::Status(_)
      | SystemSubcommand::Uninstall => None,
      SystemSubcommand::Switch(a) | SystemSubcommand::Build(a) => {
        Some(&mut a.common.no_nom)
      },
    }
  }
}

impl SystemArgs {
  /// The sources scope `xi system switch` operates in.
  ///
  /// Declared at parse time so the sources resolver never has to
  /// infer scope from the installable or the CWD. The mapping is
  /// fixed by the xi-sources-model scope table.
  #[must_use]
  pub const fn scope() -> xi_core::sources::Scope {
    xi_core::sources::Scope::System
  }
}
