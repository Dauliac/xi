use std::path::PathBuf;

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

  /// Configure a config repo for this scope and perform the first clone
  Init(InitArgs),
}

#[derive(Debug, Clone, Args)]
pub struct StatusArgs {
  /// Which half of the machine to report on.
  #[arg(long, value_enum, default_value_t = ScopeArg::User)]
  pub scope: ScopeArg,

  /// Remote URL of the config repo.
  ///
  /// Highest rung of the precedence ladder. Omit it to let
  /// `XI_SOURCES_REMOTE`, `.xi.toml [sources]`, or the Nix module answer.
  #[arg(long, env = "XI_SOURCES_REMOTE")]
  pub remote: Option<String>,

  /// Add a `provenance` map naming the rung that answered for each key.
  #[arg(long)]
  pub explain: bool,
}

#[derive(Debug, Clone, Args)]
pub struct InitArgs {
  /// Remote URL of the config repo (https, ssh, scp-style, or a path).
  pub remote: String,

  /// Branch to track. Defaults to `main`.
  #[arg(long, env = "XI_SOURCES_BRANCH")]
  pub branch: Option<String>,

  /// Absolute path to your editable clone, so `switch` works anywhere.
  #[arg(long, env = "XI_SOURCES_WORKING_DIR")]
  pub working_dir: Option<PathBuf>,

  /// Which half of the machine to configure.
  ///
  /// `auto` picks `system` when running as root and `user` otherwise.
  #[arg(long, value_enum, default_value_t = InitScopeArg::Auto)]
  pub scope: InitScopeArg,

  /// Replace an existing `[sources]` block instead of refusing.
  #[arg(long)]
  pub force: bool,

  /// Write the config and create the directories, but skip the clone.
  #[arg(long)]
  pub no_clone: bool,
}

/// Scope selector for `init`, which can additionally infer from the uid.
///
/// Separate from [`ScopeArg`] because `auto` is only meaningful here:
/// `status` reports on one named scope and must not silently switch
/// which half of the machine it is describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InitScopeArg {
  /// `system` when the effective uid is 0, `user` otherwise.
  Auto,
  /// Host-wide configuration.
  System,
  /// Per-user configuration.
  User,
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
