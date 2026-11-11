use std::str::FromStr;

use color_eyre::Result;
use xi_core::command::{ElevationStrategy, ElevationStrategyArg};

pub mod bootstrap;
pub mod cache;
pub mod clean;
pub mod config;
pub mod deploy;
pub mod interface;
pub mod logging;
pub mod proxy;
pub mod sources;
pub mod system;

pub const XI_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const XI_REV: Option<&str> = option_env!("XI_REV");

/// Version string including commit rev when available (e.g. "4.4.0 (abc1234)").
pub fn long_version() -> &'static str {
  use std::sync::OnceLock;
  static VERSION: OnceLock<String> = OnceLock::new();
  VERSION.get_or_init(|| {
    XI_REV.map_or_else(
      || XI_VERSION.to_string(),
      |rev| format!("{XI_VERSION} ({rev})"),
    )
  })
}

/// Whether any argument on the command line names a known top-level
/// subcommand (by its primary name or an alias).
///
/// Used to tell a top-level `xi --help` from a subcommand one like `xi
/// build --help`: both fail `Main::try_parse()` with the same
/// `ErrorKind::DisplayHelp`, but only the former should get the
/// bounded-context-grouped help from `interface::grouped_help`.
fn has_known_subcommand_token() -> bool {
  use clap::CommandFactory;

  let cmd = crate::interface::Main::command();
  let known: std::collections::HashSet<&str> = cmd
    .get_subcommands()
    .flat_map(|sub| {
      std::iter::once(sub.get_name()).chain(sub.get_all_aliases())
    })
    .collect();
  std::env::args()
    .skip(1)
    .any(|arg| known.contains(arg.as_str()))
}

/// Render the full help of whichever command in the tree failed with a
/// missing-subcommand error, e.g. `xi cache` or `xi develop daemon`.
///
/// Every group command (`cache`, `clean`, `os`, `auth`, `develop daemon`,
/// ...) only holds a required subcommand field with no other args, so
/// there is nothing command-specific to special-case here: the failing
/// command's path comes straight from clap's own error context, and the
/// command itself is found by walking that path down the static command
/// tree built from [`crate::interface::Main`]. Returns `None` for a
/// top-level failure (bare `xi`) so the caller falls back to
/// [`crate::interface::grouped_help`].
fn missing_subcommand_help(err: &clap::Error) -> Option<String> {
  use clap::{
    CommandFactory,
    error::{ContextKind, ContextValue},
  };

  let path = err.context().find_map(|(kind, value)| {
    match (kind, value) {
      (ContextKind::InvalidSubcommand, ContextValue::String(path)) => {
        Some(path.as_str())
      },
      _ => None,
    }
  })?;

  let mut segments = path.split_whitespace().skip(1).peekable();
  segments.peek()?;

  let mut cmd = crate::interface::Main::command();
  for name in segments {
    cmd = cmd.find_subcommand(name)?.clone();
  }
  // The cloned-out subcommand never went through the parent's build
  // pass that would otherwise set its bin_name to the full path, so
  // the usage line would show just "cache" instead of "xi cache". Use
  // the path clap's own error already resolved.
  cmd.set_bin_name(path);
  // `.ansi()`, not the plain `Display`/`to_string()`: see
  // `interface::grouped_help`'s doc comment for why.
  Some(cmd.render_long_help().ansi().to_string())
}

/// Print a help string already rendered with ANSI styling
/// (`grouped_help`/`missing_subcommand_help`), stripping that styling if
/// stdout is not a terminal.
///
/// Mirrors clap's own `err.exit()`/`print_help()` behavior, which this
/// bypasses by rendering help as a plain `String` first: the styling
/// itself is unconditional (`StyledStr::ansi()`, not `Display`), so
/// *something* downstream has to decide terminal-vs-not, same as any
/// other colored CLI output.
fn print_help(help: &str) {
  use std::io::Write as _;

  let mut out = anstream::AutoStream::auto(std::io::stdout());
  let _ = writeln!(out, "{help}");
}

/// Run xi with arguments parsed from the process environment.
///
/// # Errors
///
/// Returns an error if logging setup, Nix environment validation, environment
/// checks, or the selected command fails.
pub fn main() -> Result<()> {
  // Handle dynamic shell completions before anything else.
  // When COMPLETE=<shell> is set, this outputs completion candidates and exits.
  clap_complete::CompleteEnv::with_factory(
    <crate::interface::Main as clap::CommandFactory>::command,
  )
  .complete();

  let mut args = match <crate::interface::Main as clap::Parser>::try_parse() {
    Ok(args) => args,
    // `DisplayHelp` fires for ANY `--help`/`-h`, at any depth (e.g. `xi
    // build --help` too), with the subcommand's own help text already
    // rendered into `err`. Only override it when no subcommand token is
    // present at all, i.e. this is top-level `xi -h` / `xi --help` -
    // otherwise fall through to `err.exit()` so `xi build --help` keeps
    // showing build's help, not ours.
    Err(err)
      if err.kind() == clap::error::ErrorKind::DisplayHelp
        && !has_known_subcommand_token() =>
    {
      print_help(&crate::interface::grouped_help());
      std::process::exit(0)
    },
    // A required subcommand was missing, at ANY depth (`xi cache`, `xi
    // develop daemon`, ...). Rather than clap's terse one-liner + short
    // usage, print the full help of whichever command in the tree
    // actually failed - same DX as `-h` would give for that command.
    Err(err) if err.kind() == clap::error::ErrorKind::MissingSubcommand => {
      let help = missing_subcommand_help(&err)
        .unwrap_or_else(crate::interface::grouped_help);
      print_help(&help);
      std::process::exit(err.exit_code())
    },
    Err(err) => err.exit(),
  };

  // Backward compatibility: support XI_ELEVATION_PROGRAM env var if
  // XI_ELEVATION_STRATEGY is not set.
  // TODO: Remove this fallback in a future version
  if args.elevation_strategy.is_none()
    && let Some(old_value) = std::env::var("XI_ELEVATION_PROGRAM")
      .ok()
      .filter(|v| !v.is_empty())
  {
    tracing::warn!(
      "XI_ELEVATION_PROGRAM is deprecated, use XI_ELEVATION_STRATEGY instead. \
       Falling back to XI_ELEVATION_PROGRAM for backward compatibility. \
       Accepted values: none, passwordless, program:<path>"
    );
    match ElevationStrategyArg::from_str(&old_value) {
      Ok(strategy) => args.elevation_strategy = Some(strategy),
      Err(e) => {
        tracing::warn!(
          "Failed to parse XI_ELEVATION_PROGRAM value '{}': {}. Falling back \
           to none.",
          old_value,
          e
        );
      },
    }
  }

  // Completions need no logging, nix, or config — short-circuit early
  if let crate::interface::XiCommand::Completions(ref comp_args) = args.command
  {
    comp_args.run();
    return Ok(());
  }

  // Set up logging
  crate::logging::setup_logging(args.verbosity)?;
  tracing::debug!("{args:#?}");
  tracing::debug!(%XI_VERSION, ?XI_REV);

  // Drain any pending notification from a previous async cache push
  if let Some(notification) = xi_core::cache::drain_notification() {
    eprintln!("{notification}");
  }

  // The installer pre-check has to run before `verify_nix_environment`,
  // which shells out to `nix --version` and would otherwise fail first
  // with "No output from nix --version command". On a machine that has
  // Nix this is a PATH lookup and nothing more.
  if let Some(scope) = args.command.sources_scope() {
    xi_core::sources::switch::ensure_nix_available_for(scope)?;
  }

  // Check Nix version upfront
  xi_core::checks::verify_nix_environment()?;

  // Once we assert required Nix features, validate xi environment checks
  // For now, this is just XI_* variables being set. More checks may be
  // added to setup_environment in the future.
  xi_core::checks::verify_variables()?;

  // Load config.toml and apply cache defaults to command args
  match crate::config::ConfigStore::load_default() {
    Ok(store) => {
      match store.config() {
        Ok(config) => {
          tracing::debug!("Loaded config from {}", store.path().display());
          args.command.apply_cache_config(&config.cache);
          args.command.apply_build_config(&config.build);
          args.command.apply_override_input_config(&config.build);
          args.command.apply_locate_config(&config.locate);
          args.command.apply_repl_config(&config.repl);
        },
        Err(e) => {
          tracing::warn!("Failed to parse config: {e}");
        },
      }
    },
    Err(e) => {
      tracing::debug!("No config loaded: {e}");
    },
  }

  let elevation =
    args
      .elevation_strategy
      .as_ref()
      .map_or(ElevationStrategy::Auto, |arg| {
        match arg {
          ElevationStrategyArg::Auto => ElevationStrategy::Auto,
          ElevationStrategyArg::None => ElevationStrategy::None,
          ElevationStrategyArg::Passwordless => ElevationStrategy::Passwordless,
          ElevationStrategyArg::Program(path) => {
            ElevationStrategy::Prefer(path.clone())
          },
        }
      });

  args.command.run(elevation)
}
