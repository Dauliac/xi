pub mod args;

use color_eyre::Result;
use xi_core::cache_queue::QueueConfig;
use xi_core::style::{self, Icon};
use xi_core::{cache, cache_queue};

use self::args::{CacheCommand, CacheProxy, RetryArgs};

impl CacheProxy {
  /// Run the selected cache subcommand.
  ///
  /// # Errors
  /// Returns an error if the subcommand fails.
  pub fn run(self) -> Result<()> {
    match self.command {
      CacheCommand::Status => status(),
      CacheCommand::Retry(ref args) => retry(args),
      CacheCommand::Clear => {
        cache_queue::clear();
        println!("{} cache push queue cleared", Icon::Success.render());
        Ok(())
      },
    }
  }
}

/// Load queue config from config.toml, falling back to defaults.
fn load_queue_config() -> QueueConfig {
  crate::config::ConfigStore::load_default().map_or_else(
    |_| QueueConfig::default(),
    |store| {
      store
        .config()
        .map_or_else(|_| QueueConfig::default(), |config| config.cache.queue)
    },
  )
}

#[allow(clippy::unnecessary_wraps)]
fn status() -> Result<()> {
  let entries = cache_queue::load();
  let config = load_queue_config();

  if entries.is_empty() {
    println!("{} cache push queue is empty", Icon::Success.render());
    return Ok(());
  }

  println!(
    "{} {} pending cache push{}:",
    Icon::Warn.render(),
    entries.len(),
    if entries.len() == 1 { "" } else { "es" }
  );
  println!(
    "{}",
    style::dim(&format!(
      "  (max_size: {}, expiry: {}d, drain_interval: {}s)",
      config.max_size,
      config.expiry_secs / 86400,
      config.drain_interval_secs
    ))
  );

  for entry in &entries {
    let age = cache_queue::now_secs().saturating_sub(entry.enqueued_at);
    let age_str = format_age(age);
    let retries = if entry.retry_count > 0 {
      format!(", {} retries", entry.retry_count)
    } else {
      String::new()
    };

    let display_path = entry
      .store_path
      .strip_prefix("/nix/store/")
      .unwrap_or(&entry.store_path);

    println!(
      "  {} → {} ({age_str}{retries})",
      style::dim(display_path),
      style::bold(&entry.target.name),
    );

    if !entry.last_error.is_empty() {
      let first_line = entry.last_error.lines().next().unwrap_or("");
      println!("    {}", style::colored(first_line, style::color::RED));
    }
  }

  println!();
  println!("Run {} to flush the queue", style::bold("xi cache retry"));

  Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn retry(args: &RetryArgs) -> Result<()> {
  let count = cache_queue::pending_count();
  if count == 0 {
    println!(
      "{} cache push queue is empty, nothing to retry",
      Icon::Success.render()
    );
    return Ok(());
  }

  // Build config: config.toml < CLI flags
  let mut config = load_queue_config();
  if let Some(days) = args.max_age_days {
    config.expiry_secs = days * 24 * 3600;
  }
  if let Some(size) = args.max_size {
    config.max_size = size;
  }

  println!(
    "{} retrying {count} queued push{}...",
    Icon::Loading.render(),
    if count == 1 { "" } else { "es" }
  );

  let result = cache_queue::drain(
    &|target, path| cache::push_single_target(target, path),
    &config,
  );

  if result.succeeded > 0 {
    println!(
      "{} {} push{} succeeded",
      Icon::Success.render(),
      result.succeeded,
      if result.succeeded == 1 { "" } else { "es" }
    );
  }
  if result.expired > 0 {
    println!(
      "{}",
      style::dim(&format!(
        "  {} expired (>{}d)",
        result.expired,
        config.expiry_secs / 86400
      ))
    );
  }
  if result.missing > 0 {
    println!(
      "{}",
      style::dim(&format!("  {} store paths no longer exist", result.missing))
    );
  }
  if result.failed > 0 {
    println!(
      "{} {} push{} still failing",
      Icon::Warn.render(),
      result.failed,
      if result.failed == 1 { "" } else { "es" }
    );
    if args.clear_on_failure {
      cache_queue::clear();
      println!("{}", style::dim("  queue cleared (--clear-on-failure)"));
    }
  }

  Ok(())
}

fn format_age(secs: u64) -> String {
  if secs < 60 {
    format!("{secs}s ago")
  } else if secs < 3600 {
    format!("{}m ago", secs / 60)
  } else if secs < 86400 {
    format!("{}h ago", secs / 3600)
  } else {
    format!("{}d ago", secs / 86400)
  }
}
