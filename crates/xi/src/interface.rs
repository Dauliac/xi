use std::io;

use crate::config::{BuildConfig, CacheConfig};
use anstyle::Style;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum, builder::Styles};
use clap_verbosity_flag::InfoLevel;
use xi_core::{
  args::CacheArgs,
  checks::{FeatureRequirements, NoFeatures},
  command::ElevationStrategy,
};
use xi_nixos;

use crate::Result;

const fn make_style() -> Styles {
  Styles::plain().header(Style::new().bold()).literal(
    Style::new()
      .bold()
      .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
  )
}

#[derive(Parser, Debug)]
#[command(
    version = crate::long_version(),
    about,
    long_about = None,
    styles=make_style(),
    propagate_version = false,
    help_template = "
{name} {version}
{about-with-newline}
{usage-heading} {usage}

{all-args}{after-help}
"
)]
/// Yet another nix helper
pub struct Main {
  #[command(flatten)]
  /// Increase logging verbosity, can be passed multiple times for
  /// more detailed logs.
  pub verbosity: clap_verbosity_flag::Verbosity<InfoLevel>,

  #[arg(
    short,
    long,
    global = true,
    env = "XI_ELEVATION_STRATEGY",
    value_hint = clap::ValueHint::CommandName,
    alias = "elevation-program"
  )]
  /// Choose the privilege elevation strategy.
  ///
  /// Can be a path to an elevation program (e.g., /usr/bin/sudo),
  /// or one of: 'none' (no elevation),
  /// 'passwordless' (use elevation without password prompt for remote hosts
  /// with NOPASSWD configured), or 'auto' (automatically detect available
  /// elevation programs in order: doas, sudo, run0, pkexec)
  pub elevation_strategy: Option<xi_core::command::ElevationStrategyArg>,

  #[command(subcommand)]
  pub command: NHCommand,
}

#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub enum NHCommand {
  Os(xi_nixos::args::OsArgs),
  Home(xi_home::args::HomeArgs),
  Darwin(xi_darwin::args::DarwinArgs),
  System(crate::system::args::SystemArgs),
  Build(xi_flake::args::BuildArgs),
  Check(xi_flake::args::CheckArgs),
  Run(xi_flake::args::RunArgs),
  Fmt(xi_flake::args::FmtArgs),
  #[command(name = "show")]
  FlakeShow(xi_flake::args::ShowArgs),
  Develop(xi_develop::args::DevelopArgs),
  Init(xi_flake::args::InitArgs),
  Update(xi_flake::args::UpdateArgs),
  Ci(xi_flake::args::CiArgs),
  Lib(xi_flake::args::LibArgs),
  Test(xi_flake::args::TestArgs),
  Doctor(xi_flake::args::DoctorArgs),
  Materialize(xi_flake::args::MaterializeArgs),
  Search(xi_search::args::SearchArgs),
  Cache(crate::cache::args::CacheProxy),
  Clean(crate::clean::args::CleanProxy),
  Nix(crate::proxy::NixProxyArgs),
  /// Generate shell completions for xi
  Completions(CompletionsArgs),
}

#[derive(Parser, Debug)]
pub struct CompletionsArgs {
  /// Shell to generate completions for
  #[arg(value_enum)]
  pub shell: CompletionShell,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum CompletionShell {
  Bash,
  Elvish,
  Fish,
  #[value(name = "powershell")]
  PowerShell,
  Zsh,
  Nushell,
}

impl NHCommand {
  #[must_use]
  pub fn get_feature_requirements(&self) -> Box<dyn FeatureRequirements> {
    match self {
      Self::Os(args) => args.get_feature_requirements(),
      Self::Home(args) => args.get_feature_requirements(),
      Self::Darwin(args) => args.get_feature_requirements(),
      Self::System(args) => args.get_feature_requirements(),
      Self::Build(..)
      | Self::Check(..)
      | Self::Run(..)
      | Self::Fmt(..)
      | Self::FlakeShow(..)
      | Self::Develop(..)
      | Self::Init(..)
      | Self::Update(..)
      | Self::Ci(..)
      | Self::Lib(..)
      | Self::Test(..)
      | Self::Doctor(..)
      | Self::Materialize(..) => Box::new(xi_core::checks::FlakeFeatures),
      Self::Search(..)
      | Self::Cache(..)
      | Self::Clean(..)
      | Self::Nix(..)
      | Self::Completions(..) => Box::new(NoFeatures),
    }
  }

  /// Apply cache defaults from config.toml to all commands that have
  /// cache args. Config values are lowest priority (CLI > env > config).
  pub fn apply_cache_config(&mut self, cache_config: &CacheConfig) {
    let apply = |cache: &mut CacheArgs| {
      cache
        .apply_config_defaults(&cache_config.targets, cache_config.async_push);
    };

    match self {
      Self::Os(args) => match &mut args.subcommand {
        xi_nixos::args::OsSubcommand::Switch(a)
        | xi_nixos::args::OsSubcommand::Boot(a)
        | xi_nixos::args::OsSubcommand::Test(a) => {
          apply(&mut a.rebuild.common.cache);
        },
        xi_nixos::args::OsSubcommand::Build(a) => {
          apply(&mut a.common.cache);
        },
        xi_nixos::args::OsSubcommand::BuildVm(a) => {
          apply(&mut a.common.common.cache);
        },
        xi_nixos::args::OsSubcommand::BuildImage(a) => {
          apply(&mut a.common.common.cache);
        },
        xi_nixos::args::OsSubcommand::Repl(_)
        | xi_nixos::args::OsSubcommand::Info(_)
        | xi_nixos::args::OsSubcommand::Rollback(_) => {},
      },
      Self::Home(args) => match &mut args.subcommand {
        xi_home::args::HomeSubcommand::Switch(a)
        | xi_home::args::HomeSubcommand::Build(a) => {
          apply(&mut a.common.cache);
        },
        xi_home::args::HomeSubcommand::Repl(_) => {},
      },
      Self::Darwin(args) => match &mut args.subcommand {
        xi_darwin::args::DarwinSubcommand::Switch(a)
        | xi_darwin::args::DarwinSubcommand::Build(a) => {
          apply(&mut a.common.cache);
        },
        xi_darwin::args::DarwinSubcommand::Repl(_) => {},
      },
      Self::System(args) => match &mut args.subcommand {
        crate::system::args::SystemSubcommand::Switch(a)
        | crate::system::args::SystemSubcommand::Build(a) => {
          apply(&mut a.common.cache);
        },
      },
      Self::Build(args) => apply(&mut args.cache),
      Self::Check(_)
      | Self::Run(_)
      | Self::Fmt(_)
      | Self::FlakeShow(_)
      | Self::Develop(_)
      | Self::Init(_)
      | Self::Update(_)
      | Self::Ci(_)
      | Self::Lib(_)
      | Self::Test(_)
      | Self::Doctor(_)
      | Self::Materialize(_)
      | Self::Search(_)
      | Self::Cache(_)
      | Self::Clean(_)
      | Self::Nix(_)
      | Self::Completions(_) => {},
    }
  }

  /// Apply build defaults from config.toml to all commands that have
  /// build-related args. Config values are lowest priority (CLI > env > config).
  pub fn apply_build_config(&mut self, build_config: &BuildConfig) {
    let no_nom = !build_config.nom;
    let show_trace = build_config.show_trace;
    let keep_going = build_config.keep_going;
    let impure = build_config.impure;
    let accept_flake_config = build_config.accept_flake_config;
    let offline = build_config.offline;
    let max_jobs = build_config.max_jobs;
    let connect_timeout = build_config.connect_timeout;

    // Helper: apply passthrough defaults to NixBuildPassthroughArgs (xi-core)
    let apply_core =
      |passthrough: &mut xi_core::args::NixBuildPassthroughArgs| {
        passthrough.apply_build_defaults(
          show_trace,
          keep_going,
          impure,
          accept_flake_config,
          offline,
          max_jobs,
          connect_timeout,
        );
      };

    // Helper: apply passthrough defaults to NixPassthroughArgs (xi-flake)
    let apply_flake = |passthrough: &mut xi_flake::args::NixPassthroughArgs| {
      passthrough.apply_build_defaults(
        show_trace,
        keep_going,
        impure,
        accept_flake_config,
        offline,
        max_jobs,
        connect_timeout,
      );
    };

    // Helper: apply no_nom default (only if not already set by flag/env)
    let apply_nom = |no_nom_field: &mut bool| {
      if !*no_nom_field {
        *no_nom_field = no_nom;
      }
    };

    // Helper: apply ci_backend default (only if CLI is Auto = not overridden)
    let apply_ci_backend = |backend_field: &mut xi_flake::args::CiBackend| {
      if matches!(backend_field, xi_flake::args::CiBackend::Auto) {
        *backend_field = match build_config.ci_backend.as_str() {
          "devour-flake" => xi_flake::args::CiBackend::DevourFlake,
          "nix-fast-build" => xi_flake::args::CiBackend::NixFastBuild,
          _ => xi_flake::args::CiBackend::Auto,
        };
      }
    };

    match self {
      Self::Os(args) => match &mut args.subcommand {
        xi_nixos::args::OsSubcommand::Switch(a)
        | xi_nixos::args::OsSubcommand::Boot(a)
        | xi_nixos::args::OsSubcommand::Test(a) => {
          apply_nom(&mut a.rebuild.common.no_nom);
          apply_core(&mut a.rebuild.common.passthrough);
        },
        xi_nixos::args::OsSubcommand::Build(a) => {
          apply_nom(&mut a.common.no_nom);
          apply_core(&mut a.common.passthrough);
        },
        xi_nixos::args::OsSubcommand::BuildVm(a) => {
          apply_nom(&mut a.common.common.no_nom);
          apply_core(&mut a.common.common.passthrough);
        },
        xi_nixos::args::OsSubcommand::BuildImage(a) => {
          apply_nom(&mut a.common.common.no_nom);
          apply_core(&mut a.common.common.passthrough);
        },
        xi_nixos::args::OsSubcommand::Repl(_)
        | xi_nixos::args::OsSubcommand::Info(_)
        | xi_nixos::args::OsSubcommand::Rollback(_) => {},
      },
      Self::Home(args) => match &mut args.subcommand {
        xi_home::args::HomeSubcommand::Switch(a)
        | xi_home::args::HomeSubcommand::Build(a) => {
          apply_nom(&mut a.common.no_nom);
          apply_core(&mut a.common.passthrough);
        },
        xi_home::args::HomeSubcommand::Repl(_) => {},
      },
      Self::Darwin(args) => match &mut args.subcommand {
        xi_darwin::args::DarwinSubcommand::Switch(a)
        | xi_darwin::args::DarwinSubcommand::Build(a) => {
          apply_nom(&mut a.common.no_nom);
          apply_core(&mut a.common.passthrough);
        },
        xi_darwin::args::DarwinSubcommand::Repl(_) => {},
      },
      Self::System(args) => match &mut args.subcommand {
        crate::system::args::SystemSubcommand::Switch(a)
        | crate::system::args::SystemSubcommand::Build(a) => {
          apply_nom(&mut a.common.no_nom);
          apply_core(&mut a.common.passthrough);
        },
      },
      Self::Build(args) => {
        apply_nom(&mut args.no_nom);
        apply_ci_backend(&mut args.backend);
        apply_flake(&mut args.passthrough);
      },
      Self::Check(args) => {
        apply_nom(&mut args.no_nom);
        apply_flake(&mut args.passthrough);
      },
      Self::Run(args) => {
        apply_nom(&mut args.no_nom);
        apply_flake(&mut args.passthrough);
      },
      Self::Fmt(args) => {
        apply_nom(&mut args.no_nom);
        apply_flake(&mut args.passthrough);
      },
      Self::Ci(args) => {
        apply_nom(&mut args.no_nom);
        apply_ci_backend(&mut args.backend);
        apply_flake(&mut args.passthrough);
      },
      Self::Test(args) => {
        apply_nom(&mut args.no_nom);
        apply_flake(&mut args.passthrough);
      },
      Self::Develop(args) => {
        apply_nom(&mut args.no_nom);
        apply_core(&mut args.passthrough);
      },
      Self::FlakeShow(_)
      | Self::Init(_)
      | Self::Update(_)
      | Self::Lib(_)
      | Self::Doctor(_)
      | Self::Materialize(_)
      | Self::Search(_)
      | Self::Cache(_)
      | Self::Clean(_)
      | Self::Nix(_)
      | Self::Completions(_) => {},
    }
  }

  /// Run the selected subcommand.
  ///
  /// # Errors
  ///
  /// Returns an error if required Nix features are unavailable or if the
  /// selected subcommand fails.
  pub fn run(self, elevation: ElevationStrategy) -> Result<()> {
    // Check features specific to this command
    let requirements = self.get_feature_requirements();
    requirements.check_features()?;

    match self {
      Self::Os(args) => args.run(elevation),
      Self::Home(args) => args.run(),
      Self::Darwin(args) => args.run(elevation),
      Self::System(args) => args.run(elevation),
      Self::Build(args) => args.run(),
      Self::Check(args) => args.run(),
      Self::Run(args) => args.run(),
      Self::Fmt(args) => args.run(),
      Self::FlakeShow(args) => args.run(),
      Self::Develop(args) => args.run(),
      Self::Init(args) => args.run(),
      Self::Update(args) => args.run(),
      Self::Ci(args) => args.run(),
      Self::Lib(args) => args.run(),
      Self::Test(args) => args.run(),
      Self::Doctor(args) => args.run(),
      Self::Materialize(args) => args.run(),
      Self::Search(args) => args.run(),
      Self::Cache(proxy) => proxy.run(),
      Self::Clean(proxy) => proxy.command.run(elevation),
      Self::Nix(args) => args.run(),
      Self::Completions(args) => {
        args.run();
        Ok(())
      },
    }
  }
}

impl CompletionsArgs {
  pub fn run(&self) {
    let mut cmd = Main::command();
    match self.shell {
      CompletionShell::Bash => {
        clap_complete::generate(
          clap_complete::Shell::Bash,
          &mut cmd,
          "xi",
          &mut io::stdout(),
        );
      },
      CompletionShell::Elvish => {
        clap_complete::generate(
          clap_complete::Shell::Elvish,
          &mut cmd,
          "xi",
          &mut io::stdout(),
        );
      },
      CompletionShell::Fish => {
        clap_complete::generate(
          clap_complete::Shell::Fish,
          &mut cmd,
          "xi",
          &mut io::stdout(),
        );
      },
      CompletionShell::PowerShell => {
        clap_complete::generate(
          clap_complete::Shell::PowerShell,
          &mut cmd,
          "xi",
          &mut io::stdout(),
        );
      },
      CompletionShell::Zsh => {
        clap_complete::generate(
          clap_complete::Shell::Zsh,
          &mut cmd,
          "xi",
          &mut io::stdout(),
        );
      },
      CompletionShell::Nushell => {
        clap_complete::generate(
          clap_complete_nushell::Nushell,
          &mut cmd,
          "xi",
          &mut io::stdout(),
        );
      },
    }
  }
}
