use clap::{Args, Subcommand};

/// Manage the binary cache push queue
#[derive(Debug, Clone, Args)]
pub struct CacheProxy {
  #[clap(subcommand)]
  pub command: CacheCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CacheCommand {
  /// Show the cache push queue status
  Status,
  /// Retry all queued cache pushes
  Retry(RetryArgs),
  /// Clear the cache push queue
  Clear,
  /// Diagnose every configured `[cache.<name>]` target before a real
  /// push or pull hits the same problem.
  Doctor,
  /// Register/unregister xi as the Nix daemon's `post-build-hook`, so
  /// every build on the machine  -  not only ones run through an `xi`
  /// subcommand  -  gets pushed through the configured `[cache.*]`
  /// targets.
  #[command(subcommand)]
  Posthook(PosthookCommand),
  /// Internal: invoked by the generated hook script on every Nix
  /// build. Not meant to be run by hand.
  #[command(hide = true)]
  PosthookRun,
}

#[derive(Debug, Clone, Subcommand)]
pub enum PosthookCommand {
  /// Write the wrapper script and point `nix.conf`'s `post-build-hook`
  /// at it. Refuses, naming the existing value, if a different program
  /// is already registered there.
  Enable,
  /// Undo `enable`: remove the generated script and the `nix.conf`
  /// line, if xi is the one that added them.
  Disable,
}

#[derive(Debug, Clone, Args)]
pub struct RetryArgs {
  /// Clear the queue even if some pushes still fail
  #[arg(long)]
  pub clear_on_failure: bool,

  /// Maximum entry age before expiry (days).
  /// Overrides config.toml `queue_expiry_days`.
  #[arg(long, env = "XI_CACHE_QUEUE_EXPIRY_DAYS")]
  pub max_age_days: Option<u64>,

  /// Maximum queue size.
  /// Overrides config.toml `queue_max_size`.
  #[arg(long, env = "XI_CACHE_QUEUE_MAX_SIZE")]
  pub max_size: Option<usize>,
}
