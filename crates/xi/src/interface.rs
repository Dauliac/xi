use std::{fmt::Write as _, io, path::PathBuf};

use anstyle::Style;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum, builder::Styles};
use clap_verbosity_flag::InfoLevel;
use xi_core::{
  args::{HasBuildArgs, HasCacheArgs},
  checks::{FeatureRequirements, NoFeatures},
  command::ElevationStrategy,
};
use xi_nixos;

use crate::{
  Result,
  config::{BuildConfig, CacheConfig},
};

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
#[command(disable_help_subcommand = true)]
pub enum XiCommand {
  // -- Configuration Management --
  // `nixos` is the spelling the xi-sources-model scope table uses;
  // `os` stays the primary name.
  #[command(alias = "nixos")]
  Os(xi_nixos::args::OsArgs),
  Home(xi_home::args::HomeArgs),
  Darwin(xi_darwin::args::DarwinArgs),
  System(crate::system::args::SystemArgs),
  /// Inspect the config-repo sources model
  Sources(crate::sources::args::SourcesProxy),
  /// Rejected on purpose: names the four concrete switch verbs.
  #[command(hide = true)]
  Switch,

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
  Repl(xi_repl::args::ReplArgs),
  Vm(xi_vm::args::VmArgs),

  // -- Agent context --
  Agent(xi_agent::args::AgentArgs),

  // -- Auth --
  Auth(xi_auth::args::AuthArgs),

  // -- Maintenance --
  Cache(crate::cache::args::CacheProxy),
  Clean(crate::clean::args::CleanProxy),
  Nix(crate::proxy::NixProxyArgs),
  /// Generate shell completions for xi
  Completions(CompletionsArgs),
}

/// Bounded-context groups for the top-level command list, in display order.
///
/// clap has no native concept of subcommand groups (only a single
/// app-wide `subcommand_help_heading`), so [`grouped_help`] renders
/// clap's own help text and splices these headings into the `Commands:`
/// section by matching each line's command name against this table. Keep
/// it in sync with the `// -- Group --` comments on [`XiCommand`] above -
/// those comments are what a human reads when adding a command; this
/// table is what the renderer reads.
const COMMAND_GROUPS: &[(&str, &[&str])] = &[
  ("Config mgmt", &[
    "os", "home", "darwin", "system", "sources",
  ]),
  ("Flake ops", &[
    "build",
    "check",
    "run",
    "fmt",
    "show",
    "init",
    "update",
    "ci",
    "lib",
    "test",
    "doctor",
    "materialize",
  ]),
  ("Deployment", &["deploy"]),
  ("Development", &["develop", "search", "repl", "vm"]),
  ("Agentic", &["agent", "auth"]),
  ("Other", &["cache", "clean", "nix", "completions"]),
];

/// Render the top-level `xi` help with its `Commands:` section grouped by
/// bounded context, instead of the flat alphabetical-by-declaration list
/// clap renders by default.
///
/// Delegates all formatting (column alignment, wrapping, color styling)
/// to clap's own renderer and only splices group headings into the
/// `Commands:` block, so this stays correct as commands are added,
/// renamed, or re-described without any hand-maintained duplicate text.
#[must_use]
pub fn grouped_help() -> String {
  let mut cmd = <Main as CommandFactory>::command();
  // `.ansi()` is required to keep the color/bold styling from
  // `make_style()` in the rendered string: `StyledStr`'s plain
  // `Display`/`to_string()` strips all styling unconditionally,
  // regardless of whether the destination is a terminal - that
  // decision belongs to whoever ultimately writes this string (see
  // `main()`'s `anstream::AutoStream::auto` wrapping), not to the
  // renderer.
  let rendered = cmd.render_long_help().ansi().to_string();
  splice_command_groups(&rendered)
}

fn splice_command_groups(help: &str) -> String {
  let Some(commands_start) = help.find("Commands:") else {
    return help.to_owned();
  };
  let section_start = commands_start + "Commands:".len();
  let Some(options_rel) = help[section_start..].find("Options:") else {
    return help.to_owned();
  };
  let options_start = section_start + options_rel;

  let mut heading_before: std::collections::HashMap<&str, &str> =
    std::collections::HashMap::new();
  for (group, names) in COMMAND_GROUPS {
    if let Some(&first) = names.first() {
      heading_before.insert(first, group);
    }
  }

  let mut body = String::new();
  for line in help[section_start..options_start].lines() {
    if line.trim().is_empty() {
      continue;
    }
    // The styled command name token (e.g. `\x1b[1mos\x1b[0m`) still has
    // no internal whitespace, so `split_whitespace` still isolates it
    // correctly - it just needs stripping before matching against
    // `COMMAND_GROUPS`'s plain names.
    let name = line.split_whitespace().next().unwrap_or("");
    let plain_name = anstream::adapter::strip_str(name).to_string();
    if let Some(group) = heading_before.get(plain_name.as_str()) {
      if !body.is_empty() {
        body.push('\n');
      }
      let _ = writeln!(body, "  \x1b[1m[{group}]\x1b[0m");
    }
    body.push_str("  ");
    body.push_str(line);
    body.push('\n');
  }

  format!(
    "{}Commands:\n\n{body}\n{}",
    &help[..commands_start],
    &help[options_start..]
  )
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
      | Self::Deploy(..)
      | Self::Repl(..) => Box::new(xi_core::checks::FlakeFeatures),
      Self::Vm(args) => args.get_feature_requirements(),
      Self::Agent(..)
      | Self::Sources(..)
      | Self::Switch
      | Self::Search(..)
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
  /// build-related args. Config values are lowest priority (CLI > env >
  /// config).
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

  /// The nix command kind this xi command ends up invoking, plus the
  /// `nix flake <sub>` subcommand when there is one.
  ///
  /// The subcommand matters because `CommandKind::Flake` is too coarse to
  /// gate on: its single variant covers `update`, `lock`, `check`, `show`
  /// and `metadata` alike.
  const fn nix_command_shape(
    &self,
  ) -> Option<(nix_command::CommandKind, Option<&'static str>)> {
    use nix_command::CommandKind as K;

    match self {
      Self::Build(_) | Self::Ci(_) | Self::Test(_) => Some((K::Build, None)),
      Self::Check(_) => Some((K::Flake, Some("check"))),
      Self::Run(_) => Some((K::Run, None)),
      Self::Fmt(_) => Some((K::Fmt, None)),
      Self::Develop(_) => Some((K::Develop, None)),
      Self::FlakeShow(_) => Some((K::Flake, Some("show"))),
      // Lock-writing: must never receive the flag.
      Self::Update(_) => Some((K::Flake, Some("update"))),
      _ => None,
    }
  }

  /// Resolve `build.override_input` through the whole cascade and install
  /// the result on whichever passthrough args this command carries.
  ///
  /// Config is the lowest priority: a `--override-input` flag still wins
  /// per input name, resolved at emission time.
  pub fn apply_override_input_config(
    &mut self,
    build_config: &crate::config::BuildConfig,
  ) {
    use xi_core::override_input as oi;

    let Some((kind, flake_sub)) = self.nix_command_shape() else {
      return;
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let flake_dir = oi::discover_flake_dir(&cwd);

    // `.xi.toml` and `.xi.local.toml` live next to `flake.nix`.
    let project = flake_dir
      .as_deref()
      .map_or_else(Vec::new, |dir| xi_flake::project_override_input(Some(dir)));

    let env_raw = std::env::var("XI_OVERRIDE_INPUT").ok();

    // The CLI pairs already on the struct, so a flag can replace a
    // configured entry by name rather than being concatenated after it.
    let cli_pairs = self.override_input_pairs().unwrap_or_default();

    let ctx = oi::ResolveContext {
      global: &build_config.override_input,
      project: &project,
      env_raw: env_raw.as_deref(),
      cli_pairs: &cli_pairs,
      flake_dir: flake_dir.as_deref(),
      cwd: &cwd,
      kind,
      flake_subcommand: flake_sub,
    };

    let outcome = match oi::resolve_for_command(&ctx) {
      Ok(outcome) => outcome,
      Err(e) => {
        // Loud, not silently skipped: a misconfigured override changes
        // what gets built.
        tracing::error!("{e}");
        return;
      },
    };

    oi::report(&outcome);

    let resolved: std::collections::BTreeMap<String, String> = outcome
      .resolved
      .iter()
      .map(|r| (r.name.clone(), r.flakeref.clone()))
      .collect();

    if resolved.is_empty() {
      return;
    }

    self.install_override_input(&resolved);
  }

  /// The `--override-input` pairs already parsed from the command line.
  fn override_input_pairs(&self) -> Option<Vec<String>> {
    match self {
      Self::Build(args) => Some(args.passthrough.override_input.clone()),
      Self::Check(args) => Some(args.passthrough.override_input.clone()),
      Self::Run(args) => Some(args.passthrough.override_input.clone()),
      Self::Fmt(args) => Some(args.passthrough.override_input.clone()),
      Self::Ci(args) => Some(args.passthrough.override_input.clone()),
      Self::Test(args) => Some(args.passthrough.override_input.clone()),
      Self::Develop(args) => Some(args.passthrough.override_input.clone()),
      _ => None,
    }
  }

  fn install_override_input(
    &mut self,
    resolved: &std::collections::BTreeMap<String, String>,
  ) {
    match self {
      Self::Build(args) => {
        args.passthrough.apply_override_input_defaults(resolved);
      },
      Self::Check(args) => {
        args.passthrough.apply_override_input_defaults(resolved);
      },
      Self::Run(args) => {
        args.passthrough.apply_override_input_defaults(resolved);
      },
      Self::Fmt(args) => {
        args.passthrough.apply_override_input_defaults(resolved);
      },
      Self::Ci(args) => {
        args.passthrough.apply_override_input_defaults(resolved);
      },
      Self::Test(args) => {
        args.passthrough.apply_override_input_defaults(resolved);
      },
      Self::Develop(args) => {
        args.passthrough.apply_override_input_defaults(resolved);
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

  /// Apply the repl theme default from config.toml to `xi repl`.
  /// Config values are lowest priority (CLI > config).
  pub const fn apply_repl_config(
    &mut self,
    repl_config: &crate::config::ReplConfig,
  ) {
    if let Self::Repl(args) = self
      && args.theme.is_none()
    {
      args.theme = Some(repl_config.theme);
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
      Self::Deploy(args) => xi_deploy::detect_and_deploy(&args),
      Self::Agent(args) => xi_agent::run(args),
      Self::Search(args) => args.run(),
      Self::Repl(args) => args.run(),
      Self::Vm(args) => xi_vm::vm::run(args),
      Self::Auth(args) => xi_auth::run(&args),
      Self::Sources(proxy) => proxy.run(),
      Self::Switch => crate::sources::verbless_switch(),
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
  /// Emit shell completions for the selected shell.
  ///
  /// # Panics
  ///
  /// Cannot panic in practice: the `Nushell` variant returns before the
  /// `unreachable!()`, and every remaining variant maps to a built-in
  /// `clap_complete` shell whose registration is always registered.
  #[allow(clippy::unreachable, clippy::expect_used)]
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
