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
