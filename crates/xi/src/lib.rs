use std::str::FromStr;

use color_eyre::Result;
use xi_core::command::{ElevationStrategy, ElevationStrategyArg};

pub mod cache;
pub mod clean;
pub mod config;
pub mod interface;
pub mod logging;
pub mod proxy;
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

  let mut args = <crate::interface::Main as clap::Parser>::parse();

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
  if let crate::interface::NHCommand::Completions(ref comp_args) = args.command
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

  // Check Nix version upfront
  xi_core::checks::verify_nix_environment()?;

  // Once we assert required Nix features, validate NH environment checks
  // For now, this is just XI_* variables being set. More checks may be
  // added to setup_environment in the future.
  xi_core::checks::verify_variables()?;

  // Load config.toml and apply cache defaults to command args
  match crate::config::ConfigStore::load_default() {
    Ok(store) => match store.config() {
      Ok(config) => {
        tracing::debug!("Loaded config from {}", store.path().display());
        args.command.apply_cache_config(&config.cache);
        args.command.apply_build_config(&config.build);
      },
      Err(e) => {
        tracing::warn!("Failed to parse config: {e}");
      },
    },
    Err(e) => {
      tracing::debug!("No config loaded: {e}");
    },
  }

  let elevation =
    args
      .elevation_strategy
      .as_ref()
      .map_or(ElevationStrategy::Auto, |arg| match arg {
        ElevationStrategyArg::Auto => ElevationStrategy::Auto,
        ElevationStrategyArg::None => ElevationStrategy::None,
        ElevationStrategyArg::Passwordless => ElevationStrategy::Passwordless,
        ElevationStrategyArg::Program(path) => {
          ElevationStrategy::Prefer(path.clone())
        },
      });

  args.command.run(elevation)
}
