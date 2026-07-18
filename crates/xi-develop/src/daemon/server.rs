//! Daemon server — listens on Unix socket, handles requests, watches files.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use color_eyre::Result;
use nix_command::find_real_nix_binary;
use tracing::{debug, error, info, warn};

use super::notifications::NotificationQueue;
use super::protocol::{
  CachePushRequest, CachePushResponse, DaemonRequest, DaemonResponse,
  DaemonState, DeregisterRequest, DeregisterResponse, EvalRequest,
  EvalResponse, Notification, PackageChange, PromptRequest, PromptResponse,
  StatusResponse, read_message, write_message,
};
use super::shell_registry::ShellRegistry;
use super::watcher::GitWatcher;
use crate::shell::ShellType;
use crate::{dirs, env_file, meta, trust};

/// Default seconds between eval attempts (overridden by config).
const DEFAULT_EVAL_INTERVAL_SECS: u64 = 5;

/// Eval cache mode — controls how aggressively xi skips nix re-evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalCacheMode {
  /// Always re-eval on file change (safest, slowest).
  None,
  /// Skip eval if flake.lock + flake.nix unchanged (default, ~1ms).
  Lock,
  /// Skip eval if all watched nix files unchanged (~5ms).
  Inputs,
}

impl EvalCacheMode {
  #[must_use]
  pub fn parse(s: &str) -> Self {
    match s.trim().to_lowercase().as_str() {
      "none" => Self::None,
      "lock" => Self::Lock,
      "inputs" => Self::Inputs,
      _ => {
        tracing::warn!("Unknown eval_cache mode '{s}', falling back to 'lock'");
        Self::Lock
      },
    }
  }
}
/// Maximum idle time before daemon self-terminates.
const IDLE_TIMEOUT_SECS: u64 = 3600;
/// Watcher poll interval.
const WATCHER_POLL_MS: u64 = 200;

/// Base delay for exponential backoff on transient eval failures (seconds).
const RETRY_BASE_SECS: u64 = 30;
/// Maximum backoff delay (seconds).
const RETRY_MAX_SECS: u64 = 300;

/// Daemon state.
pub struct ServerState {
  pub flake_root: PathBuf,
  pub state_dir: PathBuf,
  pub current_env: RwLock<Option<env_file::DevEnv>>,
  pub current_target: RwLock<String>,
  pub eval_state: RwLock<DaemonState>,
  pub last_error: RwLock<Option<String>>,
  pub last_eval_time: Mutex<Option<Instant>>,
  pub notifications: Mutex<NotificationQueue>,
  pub eval_in_progress: Mutex<bool>,
  pub start_time: Instant,
  pub last_activity: Mutex<Instant>,
  pub version: String,
  /// Whether a file change was detected (set by watcher, cleared by eval).
  pub change_pending: Mutex<bool>,
  /// Configurable eval interval (from xi config.toml \[develop\] section).
  pub eval_interval_secs: u64,
  /// Extra watch patterns (from xi config.toml \[develop\] section).
  pub watch_extra: Vec<String>,
  /// Consecutive error count for exponential backoff.
  pub error_count: Mutex<u32>,
  /// Timestamp of last eval error (for backoff timing).
  pub last_error_time: Mutex<Option<Instant>>,
  /// Store paths currently being pushed to cache.
  pub active_cache_pushes: Mutex<Vec<String>>,
  /// Last time the cache queue was drained.
  pub last_queue_drain: Mutex<Option<Instant>>,
  /// Whether a queue drain is currently running.
  pub queue_drain_in_progress: Mutex<bool>,
  /// Cache queue configuration.
  pub queue_config: xi_core::cache_queue::QueueConfig,
  /// Per-PID shell instance tracking.
  pub shell_registry: Mutex<ShellRegistry>,
  /// Eval cache mode (from config.toml \[develop\] `eval_cache`).
  pub eval_cache_mode: EvalCacheMode,
  /// Cached hash of eval inputs (for eval cache fast-path).
  pub eval_input_hash: RwLock<Option<String>>,
}

impl ServerState {
  #[must_use]
  pub fn new(
    flake_root: PathBuf,
    state_dir: PathBuf,
    eval_interval_secs: u64,
    watch_extra: Vec<String>,
    queue_config: xi_core::cache_queue::QueueConfig,
    eval_cache_mode: EvalCacheMode,
  ) -> Self {
    // Warm-start: load previous state from meta.json
    let loaded_meta = meta::load(&state_dir).ok();
    let cached_env = loaded_meta.as_ref().map(|m| env_file::DevEnv {
      nix_paths: vec![],
      env_vars: std::collections::HashMap::new(),
      shell_hook: None,
      env_hash: m.env_hash.clone(),
      packages: m.packages.clone(),
    });
    let cached_input_hash = loaded_meta.and_then(|m| m.input_hash);

    Self {
      flake_root,
      state_dir,
      current_env: RwLock::new(cached_env),
      current_target: RwLock::new("default".into()),
      eval_state: RwLock::new(DaemonState::Starting),
      last_error: RwLock::new(None),
      last_eval_time: Mutex::new(None),
      notifications: Mutex::new(NotificationQueue::new()),
      eval_in_progress: Mutex::new(false),
      start_time: Instant::now(),
      last_activity: Mutex::new(Instant::now()),
      version: env!("CARGO_PKG_VERSION").to_string(),
      change_pending: Mutex::new(true), // trigger initial eval
      eval_interval_secs,
      watch_extra,
      error_count: Mutex::new(0),
      last_error_time: Mutex::new(None),
      active_cache_pushes: Mutex::new(Vec::new()),
      last_queue_drain: Mutex::new(None),
      queue_drain_in_progress: Mutex::new(false),
      queue_config,
      shell_registry: Mutex::new(ShellRegistry::new()),
      eval_cache_mode,
      eval_input_hash: RwLock::new(cached_input_hash),
    }
  }

  fn should_eval(&self) -> bool {
    let eval_state = self
      .eval_state
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    let pending = *self
      .change_pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);

    // In error state: retry with exponential backoff even without file changes.
    // This handles transient failures (network glitches, nix daemon restarts).
    if matches!(eval_state, DaemonState::BuildFailed { .. }) && !pending {
      let count = *self
        .error_count
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let last_err = *self
        .last_error_time
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if let Some(t) = last_err {
        let backoff = Duration::from_secs(
          RETRY_MAX_SECS
            .min(RETRY_BASE_SECS.saturating_mul(1u64 << count.min(4))),
        );
        if t.elapsed() >= backoff {
          debug!(
            "Retrying after error (attempt {}, backoff {}s)",
            count + 1,
            backoff.as_secs()
          );
          return true;
        }
      }
      return false;
    }

    // Normal path: must have a pending change
    if !pending {
      return false;
    }

    // Rate limit
    let last = self
      .last_eval_time
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let rate_limited = last.is_some_and(|t| {
      t.elapsed() < Duration::from_secs(self.eval_interval_secs)
    });
    drop(last);
    if rate_limited {
      return false;
    }

    true
  }

  fn is_evaluating(&self) -> bool {
    *self
      .eval_in_progress
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn mark_change_pending(&self) {
    // Clear error state so eval can retry after file change
    *self
      .eval_state
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) =
      DaemonState::Starting;
    *self
      .last_error
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *self
      .change_pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    // Reset backoff on explicit file change
    *self
      .error_count
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
    *self
      .last_error_time
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }
}

/// Run the daemon server (blocks until shutdown).
///
/// # Errors
/// Returns an error if socket binding fails.
pub fn run(socket_path: &Path, state: &Arc<ServerState>) -> Result<()> {
  // Remove stale socket
  let _ = std::fs::remove_file(socket_path);
  if let Some(parent) = socket_path.parent() {
    std::fs::create_dir_all(parent)?;
  }

  let listener = UnixListener::bind(socket_path)?;
  listener.set_nonblocking(true)?;

  info!("Daemon listening on {}", socket_path.display());

  // Set up git-aware file watcher
  let watcher = match GitWatcher::new(&state.flake_root, &state.watch_extra) {
    Ok(w) => Some(w),
    Err(e) => {
      warn!(
        "File watcher not available: {e}. Falling back to hook-triggered eval."
      );
      None
    },
  };

  // Run initial eval
  spawn_eval(Arc::clone(state));

  loop {
    // Check idle timeout
    {
      let last = state
        .last_activity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if last.elapsed() > Duration::from_secs(IDLE_TIMEOUT_SECS) {
        info!("Idle timeout reached, shutting down");
        break;
      }
    }

    // Check watcher for file changes
    if let Some(ref w) = watcher
      && w.try_recv(Duration::from_millis(0)).is_some()
    {
      debug!("File change detected by watcher");
      state.mark_change_pending();
    }

    // Maybe trigger eval (change pending + cooldown expired + not already running)
    if state.should_eval() && !state.is_evaluating() {
      spawn_eval(Arc::clone(state));
    }

    // Periodically drain the cache push queue
    maybe_drain_cache_queue(state);

    // Accept connection (non-blocking)
    match listener.accept() {
      Ok((stream, _)) => {
        *state
          .last_activity
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        let state = Arc::clone(state);
        std::thread::spawn(move || {
          if let Err(e) = handle_connection(&stream, &state) {
            debug!("Connection handler error: {e}");
          }
        });
      },
      Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
        std::thread::sleep(Duration::from_millis(WATCHER_POLL_MS));
      },
      Err(e) => {
        error!("Accept error: {e}");
        std::thread::sleep(Duration::from_millis(100));
      },
    }
  }

  let _ = std::fs::remove_file(socket_path);
  info!("Daemon stopped");
  Ok(())
}

fn handle_connection(
  stream: &std::os::unix::net::UnixStream,
  state: &Arc<ServerState>,
) -> Result<()> {
  stream.set_read_timeout(Some(Duration::from_secs(5)))?;
  stream.set_write_timeout(Some(Duration::from_secs(5)))?;

  let mut reader = BufReader::new(stream);
  let mut writer = BufWriter::new(stream);

  let request: DaemonRequest = read_message(&mut reader)?;
  let response = dispatch(request, state);
  write_message(&mut writer, &response)?;

  Ok(())
}

fn dispatch(
  request: DaemonRequest,
  state: &Arc<ServerState>,
) -> DaemonResponse {
  match request {
    DaemonRequest::Eval(ref req) => {
      DaemonResponse::Eval(handle_eval(req, state))
    },
    DaemonRequest::CachePush(req) => {
      DaemonResponse::CachePush(handle_cache_push(req, state))
    },
    DaemonRequest::Status => DaemonResponse::Status(handle_status(state)),
    DaemonRequest::Prompt(ref req) => {
      DaemonResponse::Prompt(handle_prompt(req, state))
    },
    DaemonRequest::Deregister(ref req) => {
      DaemonResponse::Deregister(handle_deregister(req, state))
    },
    DaemonRequest::Shutdown => {
      info!("Shutdown requested");
      *state
        .last_activity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now()
        .checked_sub(Duration::from_secs(IDLE_TIMEOUT_SECS + 1))
        .unwrap_or(state.start_time);
      DaemonResponse::Shutdown
    },
  }
}

fn handle_cache_push(
  req: CachePushRequest,
  state: &Arc<ServerState>,
) -> CachePushResponse {
  info!("[cache] push requested for {}", req.store_path);

  // Track active push
  {
    let mut pushes = state
      .active_cache_pushes
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    pushes.push(req.store_path.clone());
  }

  let state = Arc::clone(state);
  std::thread::spawn(move || {
    let store_path = std::path::Path::new(&req.store_path);
    let cache = xi_core::args::CacheArgs {
      push_to: if req.cache_url.is_empty() {
        None
      } else {
        Some(req.cache_url)
      },
      push_command: req.push_command,
      sign_key: req.sign_key,
      no_push: false,
      async_push: false,
      config_targets: Vec::new(),
    };

    xi_core::cache::push_to_cache(&cache, store_path);
    info!("[cache] push complete for {}", req.store_path);

    // Remove from active list
    let mut active = state
      .active_cache_pushes
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    active.retain(|p| p != &req.store_path);
  });

  CachePushResponse { accepted: true }
}

/// Periodically drain the persistent cache push queue in the background.
fn maybe_drain_cache_queue(state: &Arc<ServerState>) {
  // Check if drain interval has elapsed
  {
    let last = state
      .last_queue_drain
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if last.is_some_and(|t| {
      t.elapsed() < Duration::from_secs(state.queue_config.drain_interval_secs)
    }) {
      return;
    }
  }

  // Check if drain is already running
  {
    let in_progress = state
      .queue_drain_in_progress
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *in_progress {
      return;
    }
  }

  // Check if queue has entries (cheap check)
  if xi_core::cache_queue::pending_count() == 0 {
    // Update timestamp even when empty to avoid repeated file reads
    *state
      .last_queue_drain
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) =
      Some(Instant::now());
    return;
  }

  // Spawn drain in background thread
  *state
    .queue_drain_in_progress
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = true;

  let state = Arc::clone(state);
  std::thread::spawn(move || {
    debug!("[cache] draining queue in background");

    let config = state.queue_config.clone();
    let result = xi_core::cache_queue::drain(
      &|target, path| xi_core::cache::push_single_target(target, path),
      &config,
    );

    if result.succeeded > 0 || result.expired > 0 || result.missing > 0 {
      info!(
        "[cache] queue drain: {} succeeded, {} failed, {} expired, {} missing",
        result.succeeded, result.failed, result.expired, result.missing
      );
    }

    *state
      .last_queue_drain
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) =
      Some(Instant::now());
    *state
      .queue_drain_in_progress
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
  });
}

fn handle_prompt(req: &PromptRequest, state: &ServerState) -> PromptResponse {
  let fid = dirs::flake_id(&state.flake_root);

  // Register/update consumer in shell registry
  {
    let mut reg = state
      .shell_registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.register(req.consumer_pid, req.parent_pid, &fid, &req.target);
  }

  // Register in notification queue too
  {
    let mut q = state
      .notifications
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    q.register(req.consumer_pid);
  }

  // Update target if changed
  {
    let mut target = state
      .current_target
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let changed = *target != req.target;
    if changed {
      (*target).clone_from(&req.target);
    }
    drop(target);
    if changed {
      state.mark_change_pending();
    }
  }

  // Read generation counters
  let env_gen = std::fs::read_to_string(state.state_dir.join("env-generation"))
    .ok()
    .and_then(|s| s.trim().parse::<u64>().ok())
    .unwrap_or(0);
  let hook_gen =
    std::fs::read_to_string(state.state_dir.join("hook-generation"))
      .ok()
      .and_then(|s| s.trim().parse::<u64>().ok())
      .unwrap_or(0);

  // Check per-PID state
  let (should_source_env, should_source_hook) = {
    let reg = state
      .shell_registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    (
      reg.should_source_env(req.consumer_pid, env_gen),
      reg.should_source_hook(req.consumer_pid, hook_gen),
    )
  };

  // Mark as sourced if we're telling the shell to re-source
  if should_source_env {
    let mut reg = state
      .shell_registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.mark_sourced_env(req.consumer_pid, env_gen);
  }
  if should_source_hook {
    let mut reg = state
      .shell_registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.mark_sourced_hook(req.consumer_pid, hook_gen);
  }

  // Build file paths
  let target = state
    .current_target
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let env_link =
    dirs::current_link(&state.state_dir, &format!("env-{target}"), "sh");
  let hook_link =
    dirs::current_link(&state.state_dir, &format!("hook-{target}"), "sh");

  // Drain notifications
  let notifications = {
    let mut q = state
      .notifications
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    q.drain_for(req.consumer_pid)
  };

  let daemon_state = state
    .eval_state
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();

  let is_trusted = trust::is_trusted(&state.flake_root);

  PromptResponse {
    should_source_env,
    env_file_path: if should_source_env {
      Some(env_link.display().to_string())
    } else {
      None
    },
    should_source_hook,
    hook_file_path: if should_source_hook {
      Some(hook_link.display().to_string())
    } else {
      None
    },
    should_exit: false,
    should_spawn_subshell: false,
    spawn_flake_root: None,
    notifications,
    daemon_state,
    is_trusted,
  }
}

fn handle_deregister(
  req: &DeregisterRequest,
  state: &ServerState,
) -> DeregisterResponse {
  let was_registered = {
    let mut reg = state
      .shell_registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.deregister(req.consumer_pid)
  };

  let remaining = {
    let reg = state
      .shell_registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    u32::try_from(reg.consumer_count()).unwrap_or(u32::MAX)
  };

  // If no consumers left, trigger idle shutdown by backdating last_activity
  if remaining == 0 {
    info!(
      "Last consumer deregistered (PID {}), starting idle shutdown",
      req.consumer_pid
    );
    *state
      .last_activity
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now()
      .checked_sub(Duration::from_secs(IDLE_TIMEOUT_SECS + 1))
      .unwrap_or(state.start_time);
  }

  DeregisterResponse {
    was_registered,
    remaining_consumers: remaining,
  }
}

fn handle_eval(req: &EvalRequest, state: &ServerState) -> EvalResponse {
  let start = Instant::now();
  let result = run_full_eval(state, &req.target);
  let duration = start.elapsed();

  match result {
    Ok(changes) => {
      let link = dirs::current_link(
        &state.state_dir,
        &format!("env-{}", req.target),
        "sh",
      );
      #[allow(clippy::cast_possible_truncation)]
      EvalResponse {
        env_file_path: link.display().to_string(),
        changes,
        eval_duration_ms: duration.as_millis() as u64,
      }
    },
    Err(_e) =>
    {
      #[allow(clippy::cast_possible_truncation)]
      EvalResponse {
        env_file_path: String::new(),
        changes: vec![],
        eval_duration_ms: duration.as_millis() as u64,
      }
    },
  }
}

fn handle_status(state: &ServerState) -> StatusResponse {
  let eval_state = state
    .eval_state
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let target = state
    .current_target
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let pkg_count = state
    .current_env
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .as_ref()
    .map_or(0, |e| u32::try_from(e.packages.len()).unwrap_or(u32::MAX));
  let consumers = state
    .notifications
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .consumer_count();
  let consumers = u32::try_from(consumers).unwrap_or(u32::MAX);

  let active_pushes = state
    .active_cache_pushes
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();

  StatusResponse {
    state: eval_state,
    uptime_secs: state.start_time.elapsed().as_secs(),
    consumer_count: consumers,
    current_target: target,
    package_count: pkg_count,
    version: state.version.clone(),
    active_cache_pushes: active_pushes,
  }
}

/// Spawn an eval + hook in a background thread.
fn spawn_eval(state: Arc<ServerState>) {
  {
    let mut in_progress = state
      .eval_in_progress
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *in_progress {
      return;
    }
    *in_progress = true;
  }

  *state
    .eval_state
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner) =
    DaemonState::Evaluating;
  *state
    .last_eval_time
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());

  // Clear change pending flag
  *state
    .change_pending
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = false;

  std::thread::spawn(move || {
    let target = state
      .current_target
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();

    let result = run_full_eval(&state, &target);

    match result {
      Ok(_changes) => {
        // State already updated inside run_full_eval.
        // Reset error backoff on success.
        *state
          .error_count
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
        *state
          .last_error_time
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
      },
      Err(e) => {
        let msg = format!("{e}");
        let error_summary = extract_nix_error(&msg);

        // Increment backoff counter
        {
          let mut count = state
            .error_count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
          *count = count.saturating_add(1);
        }
        *state
          .last_error_time
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner) =
          Some(Instant::now());

        // Only notify if error changed
        let prev = state
          .last_error
          .read()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .clone();
        if prev.as_deref() != Some(&error_summary) {
          state
            .notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Notification::error(format!(
              "devshell failed:\n{error_summary}"
            )));
        }

        let count = *state
          .error_count
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state
          .eval_state
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner) =
          DaemonState::BuildFailed {
            error: error_summary.clone(),
            retry_count: count,
          };
        *state
          .last_error
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner) =
          Some(error_summary);
      },
    }

    *state
      .eval_in_progress
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
  });
}

/// Compute a hash of eval inputs based on the cache mode.
///
/// - `Lock`: hash(flake.lock + flake.nix)
/// - `Inputs`: hash(flake.lock + all *.nix files in git index)
/// - `None`: returns None (never skip)
fn compute_input_hash(
  flake_root: &Path,
  mode: EvalCacheMode,
) -> Option<String> {
  use sha2::{Digest, Sha256};

  match mode {
    EvalCacheMode::None => None,
    EvalCacheMode::Lock => {
      let mut hasher = Sha256::new();
      // Hash flake.lock
      if let Ok(content) = std::fs::read(flake_root.join("flake.lock")) {
        hasher.update(&content);
      } else {
        return None; // No flake.lock → can't cache
      }
      // Hash flake.nix
      if let Ok(content) = std::fs::read(flake_root.join("flake.nix")) {
        hasher.update(b"\0flake.nix\0");
        hasher.update(&content);
      }
      let hash = hasher.finalize();
      Some(crate::dirs::hex_encode(&hash[..16]))
    },
    EvalCacheMode::Inputs => {
      let mut hasher = Sha256::new();
      // Hash flake.lock
      if let Ok(content) = std::fs::read(flake_root.join("flake.lock")) {
        hasher.update(&content);
      } else {
        return None;
      }
      // Hash all *.nix files via git index
      if let Ok(repo) = git2::Repository::open(flake_root) {
        if let Ok(index) = repo.index() {
          let mut nix_files: Vec<String> = index
            .iter()
            .filter_map(|entry| {
              let path = String::from_utf8_lossy(&entry.path).to_string();
              if Path::new(&path)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("nix"))
              {
                Some(path)
              } else {
                None
              }
            })
            .collect();
          nix_files.sort();
          for nix_file in &nix_files {
            let full_path = flake_root.join(nix_file);
            if let Ok(content) = std::fs::read(&full_path) {
              hasher.update(format!("\0{nix_file}\0").as_bytes());
              hasher.update(&content);
            }
          }
        }
      } else {
        // No git repo → fall back to lock mode (just flake.nix)
        if let Ok(content) = std::fs::read(flake_root.join("flake.nix")) {
          hasher.update(b"\0flake.nix\0");
          hasher.update(&content);
        }
      }
      let hash = hasher.finalize();
      Some(crate::dirs::hex_encode(&hash[..16]))
    },
  }
}

/// Run nix eval + hook execution + write files + switch symlinks.
fn run_full_eval(
  state: &ServerState,
  target: &str,
) -> Result<Vec<PackageChange>> {
  let start = Instant::now();

  // Step 0: Eval cache fast-path — skip nix if inputs unchanged.
  let current_input_hash =
    compute_input_hash(&state.flake_root, state.eval_cache_mode);
  if let Some(ref current) = current_input_hash {
    let cached = state
      .eval_input_hash
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    if cached.as_deref() == Some(current.as_str()) {
      debug!(
        "Eval cache hit (mode={:?}, hash={}), skipping nix call",
        state.eval_cache_mode,
        &current[..16.min(current.len())]
      );
      *state
        .eval_state
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        DaemonState::Ready;
      return Ok(vec![]);
    }
  }

  // Step 1: nix print-dev-env (with --profile for GC root)
  let profile_path = dirs::profile_path(&state.state_dir, target);
  let dev_env = run_nix_eval(&state.flake_root, target, Some(&profile_path))?;
  let duration = start.elapsed();

  // Step 1b: Content-hash dedup — skip file writes if env is identical.
  // Compare the new env_hash with the previous one from state.
  // This avoids unnecessary generation bumps (and shell re-sources)
  // when nix eval produces the same output (e.g., only flake.nix
  // formatting changed, or a non-devshell file was edited).
  let prev_hash = state
    .current_env
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .as_ref()
    .map(|e| e.env_hash.clone());
  let env_unchanged = prev_hash.as_deref() == Some(&dev_env.env_hash)
    && !dev_env.env_hash.is_empty();

  if env_unchanged {
    debug!(
      "Eval completed in {:.1}s — env unchanged (hash: {}), skipping file writes",
      duration.as_secs_f64(),
      &dev_env.env_hash[..16.min(dev_env.env_hash.len())]
    );

    // Still update meta timestamp and state, but skip file writes + generation bump
    let _ = meta::save(
      &state.state_dir,
      &meta::DevShellMeta {
        env_hash: dev_env.env_hash.clone(),
        target: target.to_string(),
        flake_root: state.flake_root.display().to_string(),
        store_path: state
          .current_env
          .read()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .as_ref()
          .and_then(|_| {
            // Preserve existing store path
            meta::load(&state.state_dir).ok().and_then(|m| m.store_path)
          }),
        packages: dev_env.packages,
        timestamp: meta::now_secs(),
        eval_duration_ms: {
          #[allow(clippy::cast_possible_truncation)]
          {
            duration.as_millis() as u64
          }
        },
        lock_hash: None,
        input_hash: current_input_hash.clone(),
      },
    );

    // Update cached input hash
    if current_input_hash.is_some() {
      *state
        .eval_input_hash
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        current_input_hash;
    }

    *state
      .eval_state
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = DaemonState::Ready;
    *state
      .last_error
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

    return Ok(vec![]);
  }

  // Step 2: Get store path for dix diff (read from profile symlink)
  let new_store_path = std::fs::canonicalize(&profile_path)
    .ok()
    .map(|p| p.display().to_string());

  // Step 3: Write env files (A/B slot + symlink switch)
  for shell in [ShellType::Bash, ShellType::Zsh, ShellType::Fish] {
    let prefix = format!("env-{target}");
    let ext = match shell {
      ShellType::Fish => "fish",
      ShellType::Bash | ShellType::Zsh => "sh",
    };

    // Generate content with source line for hook-env
    let hook_env_link =
      dirs::current_link(&state.state_dir, &format!("hook-env-{target}"), ext);
    let env_content =
      generate_env_with_hook_source(&dev_env, shell, &hook_env_link);
    dirs::write_and_switch(&state.state_dir, &prefix, ext, &env_content)?;
  }
  dirs::bump_env_generation(&state.state_dir);

  // Step 4: Write shellHook script file (sourced directly by the user's shell)
  // NOT executed by the daemon — aliases, functions, completions need the real shell.
  // Uses a separate hook-generation counter so the shell can detect hook-only changes
  // and re-source just the hook (without re-sourcing the full env file).
  if let Some(ref hook_script) = dev_env.shell_hook
    && !hook_script.trim().is_empty()
  {
    let hook_content = format!(
      "# Generated by xi develop — shellHook from nix devshell\n\
       # Sourced in the user's shell process.\n\
       # Aliases, functions, and completions work.\n\
       {hook_script}\n"
    );
    dirs::write_and_switch(
      &state.state_dir,
      &format!("hook-{target}"),
      "sh",
      &hook_content,
    )?;
    dirs::bump_hook_generation(&state.state_dir);
  }

  // Step 5: Update derivation symlinks + dix diff
  // GC root is handled by --profile in Step 1.
  let drv_current = state.state_dir.join("drv-current");
  let drv_previous = state.state_dir.join("drv-previous");

  if let Some(ref new_path) = new_store_path {
    // Rotate: current → previous
    let had_previous =
      drv_current.exists() || std::fs::read_link(&drv_current).is_ok();
    if had_previous {
      let _ = std::fs::remove_file(&drv_previous);
      let _ = std::fs::rename(&drv_current, &drv_previous);
    }

    // Point current → new store path
    let _ = std::fs::remove_file(&drv_current);
    #[cfg(unix)]
    {
      let _ = std::os::unix::fs::symlink(new_path, &drv_current);
    }

    // Compute dix diff and capture as string
    let diff_output = if had_previous && drv_previous.exists() {
      capture_dix_diff(&drv_previous, &drv_current)
    } else {
      None
    };

    // Push notification
    if let Some(diff_str) = diff_output {
      if !diff_str.trim().is_empty() {
        state
          .notifications
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .push(Notification::success(format!(
            "devshell updated ({:.1}s):\n{diff_str}",
            duration.as_secs_f64(),
          )));
      }
    } else if !had_previous {
      state
        .notifications
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Notification::success(format!(
          "devshell ready ({:.1}s)",
          duration.as_secs_f64(),
        )));
    }
  }

  // Step 6: Save meta + update eval cache
  let _ = meta::save(
    &state.state_dir,
    &meta::DevShellMeta {
      env_hash: dev_env.env_hash.clone(),
      target: target.to_string(),
      flake_root: state.flake_root.display().to_string(),
      store_path: new_store_path,
      packages: dev_env.packages.clone(),
      timestamp: meta::now_secs(),
      eval_duration_ms: {
        #[allow(clippy::cast_possible_truncation)]
        {
          duration.as_millis() as u64
        }
      },
      lock_hash: None,
      input_hash: current_input_hash.clone(),
    },
  );

  // Update cached input hash for future fast-path checks
  if current_input_hash.is_some() {
    *state
      .eval_input_hash
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = current_input_hash;
  }

  // Step 7: Update state
  *state
    .current_env
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(dev_env);
  *state
    .eval_state
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = DaemonState::Ready;
  *state
    .last_error
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

  Ok(vec![])
}

/// Generate env file content with a source line for hook-env.
///
/// Uses depth-scoped registries (`__XI_INJECTED_VARS_<depth>`) so nested
/// devshells only clean up their own level, preserving parent vars.
/// Depth is read from `$__XI_DEPTH` at runtime (set by spawn command).
fn generate_env_with_hook_source(
  dev_env: &env_file::DevEnv,
  shell: ShellType,
  hook_env_link: &Path,
) -> String {
  let mut lines = vec!["# Generated by xi develop — pure exports".to_string()];

  // Cleanup preamble: depth-scoped unset of previously-injected vars
  lines.extend(env_file::generate_cleanup_preamble(shell));

  // PATH: prepend nix paths to current $PATH (not __XI_ORIG_PATH)
  if !dev_env.nix_paths.is_empty() {
    match shell {
      ShellType::Fish => {
        lines.push(format!(
          "set -gx PATH {} $PATH",
          dev_env.nix_paths.join(" ")
        ));
      },
      ShellType::Bash | ShellType::Zsh => {
        let nix_path_str = dev_env.nix_paths.join(":");
        lines.push(format!("export PATH=\"{nix_path_str}:$PATH\""));
      },
    }
  }

  // Env vars (sorted)
  let mut sorted: Vec<_> = dev_env.env_vars.iter().collect();
  sorted.sort_by_key(|(k, _)| *k);
  let mut injected_keys: Vec<&str> = Vec::new();
  for (key, value) in &sorted {
    lines.push(shell.export(key, value));
    injected_keys.push(key);
  }

  lines.push(shell.export("IN_NIX_SHELL", "impure"));
  injected_keys.push("IN_NIX_SHELL");

  // Source line for hook-env (the daemon writes this file separately)
  let hook_path = hook_env_link.display();
  lines.push(String::new());
  match shell {
    ShellType::Fish => {
      lines.push(format!(
        "if test -f '{hook_path}'\n  source '{hook_path}'\nend"
      ));
    },
    ShellType::Bash | ShellType::Zsh => {
      lines.push(format!(
        "if [[ -f '{hook_path}' ]]; then\n  source '{hook_path}'\nfi"
      ));
    },
  }

  // Registry: depth-scoped record of injected vars
  lines.push(env_file::generate_registry(&injected_keys, shell));

  // Cleanup temp vars from preamble
  match shell {
    ShellType::Fish => {
      lines.push("set -e __xi_d __xi_reg __xi_var __xi_keys".to_string());
    },
    ShellType::Bash | ShellType::Zsh => {
      lines.push("unset __xi_d __xi_keys 2>/dev/null".to_string());
    },
  }

  lines.join("\n") + "\n"
}

fn run_nix_eval(
  flake_root: &Path,
  target: &str,
  profile_path: Option<&Path>,
) -> Result<env_file::DevEnv> {
  let installable = if target == "default" {
    ".#".to_string()
  } else {
    format!(".#{target}")
  };

  let mut cmd = std::process::Command::new(find_real_nix_binary());
  cmd.args([
    "print-dev-env",
    &installable,
    "--json",
    "--option",
    "connect-timeout",
    "5",
    "--option",
    "download-attempts",
    "2",
  ]);

  if let Some(profile) = profile_path {
    cmd.args(["--profile", &profile.display().to_string()]);
  }

  let output = cmd
    .current_dir(flake_root)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .output()?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    color_eyre::eyre::bail!("{stderr}");
  }

  env_file::DevEnv::from_nix_json(&output.stdout)
}

/// Extract a meaningful error summary from nix stderr output.
///
/// Nix stderr is verbose — the actual error is usually after `error:`.
/// We find the first `error:` line and include a few lines of context,
/// Capture dix diff output as a string (instead of printing to stdout).
fn capture_dix_diff(old: &Path, new: &Path) -> Option<String> {
  let old_snap = dix::query_store_snapshot(old, true).ok()?;
  let new_snap = dix::query_store_snapshot(new, true).ok()?;
  let report = dix::diff_store_snapshots(&old_snap, &new_snap);

  let mut buf = Vec::new();
  let mut writer = std::io::Cursor::new(&mut buf);
  let wrote =
    dix::write_diff_report(&mut WriteFmtAdapter(&mut writer), &report).ok()?;

  if wrote == 0 {
    return None;
  }

  String::from_utf8(buf).ok()
}

/// Adapter to convert `io::Write` to `fmt::Write` for dix.
struct WriteFmtAdapter<'a, W: std::io::Write>(&'a mut W);

impl<W: std::io::Write> std::fmt::Write for WriteFmtAdapter<'_, W> {
  fn write_str(&mut self, s: &str) -> std::fmt::Result {
    self.0.write_all(s.as_bytes()).map_err(|_| std::fmt::Error)
  }
}

/// Extract nix error from stderr. Shows everything from the first `error:` line
/// to the end, skipping noise (fetching, warning, stack traces, trace notes).
fn extract_nix_error(stderr: &str) -> String {
  let lines: Vec<&str> = stderr.lines().collect();

  // Find first "error:" line
  let error_start = lines.iter().position(|l| l.trim().starts_with("error:"));

  if let Some(start) = error_start {
    let mut summary: Vec<&str> = Vec::new();
    for line in &lines[start..] {
      let trimmed = line.trim();
      // Skip noise
      if trimmed.starts_with("(stack trace")
        || trimmed.starts_with("note: trace")
        || trimmed.starts_with("For full logs")
      {
        continue;
      }
      if trimmed.is_empty() && summary.len() > 1 {
        continue; // skip blank lines in the middle
      }
      summary.push(line); // keep original indentation
    }
    if !summary.is_empty() {
      return summary.join("\n");
    }
  }

  // Fallback: return full stderr minus noise
  lines
    .iter()
    .filter(|l| {
      let t = l.trim();
      !t.is_empty()
        && !t.starts_with("warning:")
        && !t.starts_with("fetching")
        && !t.starts_with("evaluating")
    })
    .copied()
    .collect::<Vec<_>>()
    .join("\n")
}
