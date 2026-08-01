use std::io;

use crate::config::{BuildConfig, CacheConfig};
use anstyle::Style;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum, builder::Styles};
use clap_verbosity_flag::InfoLevel;
use xi_core::{
  args::{HasBuildArgs, HasCacheArgs},
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
  pub command: XiCommand,
}

#[derive(Subcommand, Debug)]
#[command(
  disable_help_subcommand = true,
  subcommand_help_heading = "Commands",
  after_help = "\x1b[1mCommand groups:\x1b[0m
  Config mgmt:  os, home, darwin, system
  Flake ops:    build, check, run, fmt, show, init, update, ci, lib, test, doctor, materialize
  Deployment:   deploy
  Development:  develop, search
  Auth:         auth
  Maintenance:  cache, clean, nix, completions"
)]
pub enum XiCommand {
  // -- Configuration Management --
  Os(xi_nixos::args::OsArgs),
  Home(xi_home::args::HomeArgs),
  Darwin(xi_darwin::args::DarwinArgs),
  System(crate::system::args::SystemArgs),

  // -- Flake Operations --
  Build(xi_flake::args::BuildArgs),
  Check(xi_flake::args::CheckArgs),
  Run(xi_flake::args::RunArgs),
  Fmt(xi_flake::args::FmtArgs),
  #[command(name = "show")]
  FlakeShow(xi_flake::args::ShowArgs),
  Init(xi_flake::args::InitArgs),
  Update(xi_flake::args::UpdateArgs),
  Ci(xi_flake::args::CiArgs),
  Lib(xi_flake::args::LibArgs),
  Test(xi_flake::args::TestArgs),
  Doctor(xi_flake::args::DoctorArgs),
  Materialize(xi_flake::args::MaterializeArgs),

  // -- Deployment --
  Deploy(xi_deploy::args::DeployArgs),

  // -- Development --
  Develop(xi_develop::args::DevelopArgs),
  Search(xi_search::args::SearchArgs),

  // -- Auth --
  Auth(xi_auth::args::AuthArgs),

  // -- Maintenance --
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

impl XiCommand {
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
      | Self::Materialize(..)
      | Self::Deploy(..) => Box::new(xi_core::checks::FlakeFeatures),
      Self::Search(..)
      | Self::Auth(..)
      | Self::Cache(..)
      | Self::Clean(..)
      | Self::Nix(..)
      | Self::Completions(..) => Box::new(NoFeatures),
    }
  }

  /// Apply cache defaults from config.toml to all commands that have
  /// cache args. Config values are lowest priority (CLI > env > config).
  pub fn apply_cache_config(&mut self, cache_config: &CacheConfig) {
    let cache = match self {
      Self::Os(args) => args.cache_args_mut(),
      Self::Home(args) => args.cache_args_mut(),
      Self::Darwin(args) => args.cache_args_mut(),
      Self::System(args) => args.cache_args_mut(),
      Self::Build(args) => Some(&mut args.cache),
      _ => None,
    };

    if let Some(cache) = cache {
      cache
        .apply_config_defaults(&cache_config.targets, cache_config.async_push);
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

    // Config-management commands: use HasBuildArgs trait for passthrough
    {
      let passthrough = match self {
        Self::Os(args) => args.build_passthrough_mut(),
        Self::Home(args) => args.build_passthrough_mut(),
        Self::Darwin(args) => args.build_passthrough_mut(),
        Self::System(args) => args.build_passthrough_mut(),
        Self::Develop(args) => Some(&mut args.passthrough),
        _ => None,
      };
      if let Some(pt) = passthrough {
        pt.apply_build_defaults(
          show_trace,
          keep_going,
          impure,
          accept_flake_config,
          offline,
          max_jobs,
          connect_timeout,
        );
      }
    }

    // Config-management commands: use HasBuildArgs trait for no_nom
    {
      let no_nom_field = match self {
        Self::Os(args) => args.no_nom_mut(),
        Self::Home(args) => args.no_nom_mut(),
        Self::Darwin(args) => args.no_nom_mut(),
        Self::System(args) => args.no_nom_mut(),
        Self::Develop(args) => Some(&mut args.no_nom),
        _ => None,
      };
      if let Some(field) = no_nom_field {
        apply_nom(field);
      }
    }

    // Flake commands: use xi-flake's NixPassthroughArgs directly
    match self {
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
      _ => {},
    }
  }

  /// Apply locate defaults from config.toml to the run command.
  /// Config values are lowest priority (CLI > env > config).
  pub const fn apply_locate_config(
    &mut self,
    locate_config: &crate::config::LocateConfig,
  ) {
    if let Self::Run(args) = self
      && args.cache_level.is_none()
    {
      args.cache_level = Some(locate_config.cache_level);
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
      Self::Deploy(args) => xi_deploy::detect_and_deploy(args),
      Self::Search(args) => args.run(),
      Self::Auth(args) => xi_auth::run(args),
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
    if matches!(self.shell, CompletionShell::Nushell) {
      clap_complete::generate(
        clap_complete_nushell::Nushell,
        &mut cmd,
        "xi",
        &mut io::stdout(),
      );
      return;
    }

    // Emit a *dynamic* registration script so completions call back into xi
    // at completion time. This is required for `ArgValueCompleter`-based
    // completers (e.g. package/devShell/config name completion) to work —
    // the static AOT generator falls back to `_default` (file completion)
    // for those args.
    let shell_name = match self.shell {
      CompletionShell::Bash => "bash",
      CompletionShell::Elvish => "elvish",
      CompletionShell::Fish => "fish",
      CompletionShell::PowerShell => "powershell",
      CompletionShell::Zsh => "zsh",
      CompletionShell::Nushell => unreachable!("handled above"),
    };
    let shells = clap_complete::env::Shells::builtins();
    let completer = shells
      .completer(shell_name)
      .expect("built-in shell completer");
    let _ = completer.write_registration(
      "COMPLETE",
      "xi",
      "xi",
      "xi",
      &mut io::stdout(),
    );
  }
}
