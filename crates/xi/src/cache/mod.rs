pub mod args;

use color_eyre::Result;
use xi_core::{
  args::CacheTarget,
  cache,
  cache_queue,
  cache_queue::QueueConfig,
  sources::credentials::Secret,
  style::{self, Icon},
};

use self::args::{CacheCommand, CacheProxy, PosthookCommand, RetryArgs};

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
      CacheCommand::Doctor => doctor(),
      CacheCommand::Posthook(PosthookCommand::Enable) => posthook_enable(),
      CacheCommand::Posthook(PosthookCommand::Disable) => posthook_disable(),
      CacheCommand::PosthookRun => posthook_run(),
    }
  }
}

/// `xi cache doctor`: for each configured target, check credential
/// files, endpoint reachability, signing key readability, and that the
/// effective pull key is actually resolvable. Exits non-zero (via an
/// error) when any check fails, naming the specific problem.
fn doctor() -> Result<()> {
  let targets = crate::config::ConfigStore::load_default()
    .and_then(|store| store.config())
    .map(|config| config.cache.targets)
    .unwrap_or_default();

  if targets.is_empty() {
    println!(
      "{} no [cache.<name>] targets configured",
      Icon::Warn.render()
    );
    return Ok(());
  }

  let mut problems = 0_usize;
  for target in &targets {
    println!("{}", style::bold(&target.name));
    problems += doctor_credentials(target);
    problems += doctor_endpoint(target);
    problems += doctor_signing_key(target);
    problems += doctor_pull_key(target);
  }

  if problems == 0 {
    println!(
      "{} every configured target looks healthy",
      Icon::Success.render()
    );
    Ok(())
  } else {
    color_eyre::eyre::bail!(
      "{problems} problem(s) found across {} target(s)",
      targets.len()
    )
  }
}

fn doctor_credentials(target: &CacheTarget) -> usize {
  let Some(credentials) = &target.credentials else {
    return 0;
  };

  let mut problems = 0;
  for (label, path) in [
    ("access-key-id-file", &credentials.access_key_id_file),
    (
      "secret-access-key-file",
      &credentials.secret_access_key_file,
    ),
  ] {
    let Some(path) = path else {
      continue;
    };
    match Secret::from_file(std::path::Path::new(path)) {
      Ok(_) => {
        println!("  {} {label} mounted and readable", Icon::Success.render())
      },
      Err(e) => {
        println!("  {} {label} ({path}): {e}", Icon::Error.render());
        problems += 1;
      },
    }
  }
  problems
}

fn doctor_endpoint(target: &CacheTarget) -> usize {
  let Some(pull_url) = &target.pull_url else {
    return 0;
  };
  if !pull_url.starts_with("http://") && !pull_url.starts_with("https://") {
    println!(
      "  {} endpoint reachability: skipped (pull_url is not http(s))",
      style::dim("-")
    );
    return 0;
  }

  match cache::check_endpoint_reachable(pull_url) {
    Ok(()) => {
      println!(
        "  {} endpoint reachable ({pull_url})",
        Icon::Success.render()
      );
      0
    },
    Err(e) => {
      println!("  {} endpoint {pull_url}: {e}", Icon::Error.render());
      1
    },
  }
}

fn doctor_signing_key(target: &CacheTarget) -> usize {
  match cache::check_signing_key(target) {
    None => 0,
    Some(Ok(_)) => {
      println!(
        "  {} signing key readable and converts to a public key",
        Icon::Success.render()
      );
      0
    },
    Some(Err(e)) => {
      println!("  {} signing key: {e}", Icon::Error.render());
      1
    },
  }
}

fn doctor_pull_key(target: &CacheTarget) -> usize {
  if target.pull_url.is_none() {
    return 0;
  }
  cache::resolve_pull_public_key(target).map_or_else(
    || {
      println!(
        "  {} pull_url is configured but no trusted public key could be \
         resolved (set pull_public_key or signing_key)",
        Icon::Error.render()
      );
      1
    },
    |key| {
      println!(
        "  {} effective trusted public key resolved ({key})",
        Icon::Success.render()
      );
      0
    },
  )
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

/// Where the posthook reversal receipt lives, alongside the prestage
/// receipt's own state directory.
fn posthook_receipt_path() -> std::path::PathBuf {
  let prestage_paths = xi_core::sources::cache_prestage::ConfPaths::resolve(
    xi_core::sources::Scope::System,
  );
  prestage_paths.receipt.parent().map_or_else(
    || {
      std::path::PathBuf::from("/var/lib/xi")
        .join(xi_core::cache_posthook::POSTHOOK_RECEIPT_NAME)
    },
    |dir| dir.join(xi_core::cache_posthook::POSTHOOK_RECEIPT_NAME),
  )
}

/// `xi cache posthook enable`: write the wrapper script and point
/// `nix.conf`'s `post-build-hook` at it.
///
/// Targets the system-scope `nix.conf` (`/etc/nix/nix.conf`, or
/// `$XI_NIX_SYSROOT/etc/nix/nix.conf` under test)  -  `post-build-hook` is
/// a daemon-wide setting, so a per-user `nix.conf` would only apply when
/// that user runs Nix without a daemon at all. Writing there requires
/// the invoking user to already have permission to edit that file
/// (e.g. running under `sudo`); this does not yet elevate on its own.
fn posthook_enable() -> Result<()> {
  use xi_core::cache_posthook::{
    AmendOutcome,
    PostHookReceipt,
    RECEIPT_WIRE_VERSION,
    amend_nix_conf_for_posthook,
    render_hook_script,
  };

  let paths = xi_core::sources::cache_prestage::ConfPaths::resolve(
    xi_core::sources::Scope::System,
  );
  let xi_binary = std::env::current_exe().map_err(|e| {
    color_eyre::eyre::eyre!("failed to resolve xi's own executable path: {e}")
  })?;
  let hook_script = paths
    .nix_conf
    .parent()
    .unwrap_or_else(|| std::path::Path::new("/"))
    .join(xi_core::cache_posthook::HOOK_SCRIPT_NAME);

  xi_core::sources::cache_prestage::write_atomic(
    &hook_script,
    &render_hook_script(&xi_binary),
  )
  .map_err(|e| {
    color_eyre::eyre::eyre!("failed to write {}: {e}", hook_script.display())
  })?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
      &hook_script,
      std::fs::Permissions::from_mode(0o755),
    )
    .map_err(|e| {
      color_eyre::eyre::eyre!(
        "failed to make {} executable: {e}",
        hook_script.display()
      )
    })?;
  }

  match amend_nix_conf_for_posthook(&paths.nix_conf, &hook_script) {
    Ok((outcome, appended)) => {
      let receipt = PostHookReceipt {
        wire_version: RECEIPT_WIRE_VERSION,
        nix_conf: paths.nix_conf.clone(),
        appended,
        hook_script: hook_script.clone(),
      };
      receipt.store(&posthook_receipt_path()).map_err(|e| {
        color_eyre::eyre::eyre!("failed to write posthook receipt: {e}")
      })?;

      match outcome {
        AmendOutcome::Added => {
          println!(
            "{} registered xi as post-build-hook in {}",
            Icon::Success.render(),
            paths.nix_conf.display()
          )
        },
        AmendOutcome::AlreadyPresent => {
          println!(
            "{} xi is already registered as post-build-hook",
            Icon::Success.render()
          )
        },
      }
      Ok(())
    },
    Err(e) => Err(color_eyre::eyre::eyre!(e)),
  }
}

/// `xi cache posthook disable`: undo `enable`.
fn posthook_disable() -> Result<()> {
  use xi_core::cache_posthook::{PostHookReceipt, remove_posthook_line};

  let receipt_path = posthook_receipt_path();
  let Some(receipt) = PostHookReceipt::load(&receipt_path).map_err(|e| {
    color_eyre::eyre::eyre!("failed to read posthook receipt: {e}")
  })?
  else {
    println!(
      "{} xi is not registered as post-build-hook (no receipt found)",
      Icon::Warn.render()
    );
    return Ok(());
  };

  let body = std::fs::read_to_string(&receipt.nix_conf).unwrap_or_default();
  let updated = remove_posthook_line(&body, receipt.appended.as_deref());
  xi_core::sources::cache_prestage::write_atomic(&receipt.nix_conf, &updated)
    .map_err(|e| {
    color_eyre::eyre::eyre!(
      "failed to update {}: {e}",
      receipt.nix_conf.display()
    )
  })?;

  let _ = std::fs::remove_file(&receipt.hook_script);
  PostHookReceipt::delete(&receipt_path).map_err(|e| {
    color_eyre::eyre::eyre!("failed to remove posthook receipt: {e}")
  })?;

  println!(
    "{} unregistered xi as post-build-hook from {}",
    Icon::Success.render(),
    receipt.nix_conf.display()
  );
  Ok(())
}

/// Internal: invoked by the generated hook script on every Nix build.
///
/// Reads `$OUT_PATHS` (space-separated store paths, the protocol Nix's
/// `post-build-hook` itself defines) and enqueues/pushes each one
/// against the currently-configured `[cache.*]` targets. Runs with no
/// meaningful CWD (the nix-daemon has none), so match resolution has no
/// flake-ref/git-remote candidate: only targets with an empty `match`
/// list (the back-compat default) or `default = true` apply, exactly
/// like any other invocation with no resolvable remote.
#[allow(clippy::unnecessary_wraps)]
fn posthook_run() -> Result<()> {
  let Ok(out_paths) = std::env::var("OUT_PATHS") else {
    return Ok(());
  };

  let targets = crate::config::ConfigStore::load_default()
    .and_then(|store| store.config())
    .map(|config| config.cache.targets)
    .unwrap_or_default();
  if targets.is_empty() {
    return Ok(());
  }

  let cache_args = xi_core::args::CacheArgs {
    config_targets: targets,
    ..Default::default()
  };

  for path in out_paths.split_whitespace() {
    let push_result =
      cache::push_to_cache(&cache_args, std::path::Path::new(path));
    if push_result.required_failed() {
      eprintln!(
        "{} required cache target failed to push {path} from post-build-hook",
        Icon::Error.render()
      );
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
