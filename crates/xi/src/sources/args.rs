use clap::{Args, Subcommand, ValueEnum};
use xi_core::sources::Scope;

/// Inspect and manage the config-repo sources model
#[derive(Debug, Clone, Args)]
pub struct SourcesProxy {
  #[clap(subcommand)]
  pub command: SourcesCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SourcesCommand {
  /// Report the sources model for a scope as a JSON envelope
  Status(StatusArgs),
}

#[derive(Debug, Clone, Args)]
pub struct StatusArgs {
  /// Which half of the machine to report on.
  #[arg(long, value_enum, default_value_t = ScopeArg::User)]
  pub scope: ScopeArg,

  /// Remote URL of the config repo.
  ///
  /// Until `xi-sources-config` lands its path tables, this (or the
  /// matching environment variable) is the only way to name the source.
  #[arg(long, env = "XI_SOURCES_REMOTE")]
  pub remote: Option<String>,
}

/// CLI spelling of [`Scope`].
///
/// A separate type so the clap surface can evolve (new value aliases,
/// help text) without touching the model enum in `xi-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
  /// Host-wide configuration.
  System,
  /// Per-user configuration.
  User,
}

impl From<ScopeArg> for Scope {
  fn from(arg: ScopeArg) -> Self {
    match arg {
      ScopeArg::System => Self::System,
      ScopeArg::User => Self::User,
    }
  }
}
