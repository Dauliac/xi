use std::{
  io::Write as _,
  path::{Path, PathBuf},
  process::Stdio,
  sync::mpsc,
  time::UNIX_EPOCH,
};

use color_eyre::{Result, eyre::Context};
use nix_command::find_real_nix_binary;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::{
  args::{CacheArgs, CacheCredentials, CacheTarget},
  sources::credentials::Secret,
};

/// Default bound on concurrent `nix copy` child processes a single push
/// cycle (current build + queue drain) may run at once.
///
/// `[cache] max_parallel_jobs`, `--max-parallel-jobs`, and
/// `XI_CACHE_MAX_PARALLEL_JOBS` all override this; CLI > env > toml >
/// this default, same ladder as the rest of xi.
#[must_use]
pub fn default_max_parallel_jobs() -> usize {
  std::thread::available_parallelism()
    .map_or(8, std::num::NonZero::get)
    .min(8)
}

/// Resolve `max_parallel_jobs` from CLI > env > toml > default.
#[must_use]
pub fn resolve_max_parallel_jobs(
  cli: Option<usize>,
  toml: Option<usize>,
) -> usize {
  cli
    .or_else(|| {
      std::env::var("XI_CACHE_MAX_PARALLEL_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
    })
    .or(toml)
    .unwrap_or_else(default_max_parallel_jobs)
    .max(1)
}

/// Notification written by background cache push processes.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheNotification {
  pub success:      bool,
  pub label:        String,
  #[serde(default)]
  pub stderr:       String,
  #[serde(default)]
  pub store_path:   String,
  #[serde(default)]
  pub push_url:     Option<String>,
  #[serde(default)]
  pub signing_key:  Option<String>,
  #[serde(default)]
  pub push_command: Vec<String>,
}

/// Outcome of pushing to one target, as part of a [`PushResult`].
#[derive(Debug)]
pub struct TargetOutcome {
  pub name:     String,
  pub required: bool,
  pub error:    Option<color_eyre::eyre::Report>,
}

/// Aggregate outcome of a [`push_to_cache`] call, across every resolved
/// target.
#[must_use]
#[derive(Debug, Default)]
pub struct PushResult {
  pub outcomes: Vec<TargetOutcome>,
}

impl PushResult {
  /// Whether any `required = true` target failed  -  the signal
  /// `xi ... switch` uses to decide its own exit code.
  #[must_use]
  pub fn required_failed(&self) -> bool {
    self
      .outcomes
      .iter()
      .any(|o| o.required && o.error.is_some())
  }

  /// Names of every `required = true` target that failed, for the error
  /// message [`enforce_required`] raises.
  fn failed_required_names(&self) -> Vec<&str> {
    self
      .outcomes
      .iter()
      .filter(|o| o.required && o.error.is_some())
      .map(|o| o.name.as_str())
      .collect()
  }
}

/// Turn a [`push_to_cache`] result into an error when a `required = true`
/// target failed, so the calling `xi ... switch` exits non-zero
/// (SC-BC-011). A non-required target's failure never reaches here  -
/// it only queues for retry.
///
/// # Errors
/// Returns an error naming every failed required target.
pub fn enforce_required(result: &PushResult) -> Result<()> {
  let failed = result.failed_required_names();
  if failed.is_empty() {
    Ok(())
  } else {
    color_eyre::eyre::bail!(
      "required cache target(s) failed to push: {}",
      failed.join(", ")
    )
  }
}

/// Push a built store path to all configured binary caches.
///
/// Drains the persistent retry queue first (previously-failed pushes),
/// then pushes the current build's output, both over one bounded
/// thread pool (`max_parallel_jobs`). Push is best-effort: failures emit
/// a warning but do not block the calling operation unless the failing
/// target is `required = true` -- see [`PushResult::required_failed`].
pub fn push_to_cache(cache: &CacheArgs, out_path: &Path) -> PushResult {
  let targets = cache.resolve_targets();
  if targets.is_empty() {
    return PushResult::default();
  }

  if cache.async_push {
    // A `push_url_file` target's decrypted URL must stay inside a
    // single scoped child-process env var (see that field's own doc
    // comment) - the daemon-socket and detached-shell dispatch paths
    // `push_to_cache_async` uses below would otherwise have to carry
    // it as a socket message or a spawned command line instead.
    // Route exactly those targets through the ordinary synchronous
    // path regardless of `async_push`; everything else still goes
    // async.
    let (sync_only, async_eligible): (Vec<_>, Vec<_>) = targets
      .into_iter()
      .partition(|t| t.push_url.is_none() && t.push_url_file.is_some());

    if !async_eligible.is_empty() {
      push_to_cache_async(&async_eligible, out_path);
    }
    return if sync_only.is_empty() {
      PushResult::default()
    } else {
      push_to_cache_sync(&sync_only, cache, out_path)
    };
  }

  push_to_cache_sync(&targets, cache, out_path)
}

/// The synchronous half of [`push_to_cache`]: drain the retry queue,
/// then push `targets` over one bounded thread pool.
fn push_to_cache_sync(
  targets: &[CacheTarget],
  cache: &CacheArgs,
  out_path: &Path,
) -> PushResult {
  let max_parallel_jobs =
    resolve_max_parallel_jobs(cache.max_parallel_jobs, None);
  let queue_config = crate::cache_queue::QueueConfig::default();

  let drained = crate::cache_queue::drain_bounded(
    &push_single_target,
    &queue_config,
    max_parallel_jobs,
  );
  if drained.succeeded > 0 {
    info!(
      "[cache] drained {} previously-queued push(es)",
      drained.succeeded
    );
  }

  let out_path_str = out_path.to_str().unwrap_or_default();
  let outcomes = run_bounded(targets, max_parallel_jobs, |target| {
    let result = push_single_target(target, out_path);
    if let Err(ref e) = result {
      warn!("[cache] push to '{}' failed (non-fatal): {e}", target.name);
      crate::cache_queue::enqueue(
        out_path_str,
        target,
        &format!("{e}"),
        &queue_config,
      );
      warn!(
        "[cache] queued for retry — run {} to flush",
        crate::style::bold("xi cache retry")
      );
    }
    TargetOutcome {
      name:     target.name.clone(),
      required: target.required,
      error:    result.err(),
    }
  });

  PushResult { outcomes }
}

/// Run `f` over every item in `items` using a bounded thread pool of at
/// most `max_parallel_jobs` concurrent workers, returning results in
/// the input order.
///
/// Each push is an independent `nix copy` subprocess; one target
/// blocking/timing out must not delay the others (SC-BC-007).
pub(crate) fn run_bounded<T, F, R>(
  items: &[T],
  max_parallel_jobs: usize,
  f: F,
) -> Vec<R>
where
  T: Sync,
  R: Send,
  F: Fn(&T) -> R + Sync,
{
  if items.is_empty() {
    return Vec::new();
  }
  let workers = max_parallel_jobs.min(items.len()).max(1);
  if workers <= 1 {
    return items.iter().map(f).collect();
  }

  let (tx, rx) = mpsc::channel();
  let next = std::sync::atomic::AtomicUsize::new(0);
  std::thread::scope(|scope| {
    for _ in 0..workers {
      let tx = tx.clone();
      let next = &next;
      let f = &f;
      scope.spawn(move || {
        loop {
          let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
          if i >= items.len() {
            break;
          }
          let result = f(&items[i]);
          if tx.send((i, result)).is_err() {
            break;
          }
        }
      });
    }
    drop(tx);

    let mut indexed: Vec<(usize, R)> = rx.into_iter().collect();
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, r)| r).collect()
  })
}

/// Push a store path to a single cache target.
///
/// This is the shared push logic used by both sync push and queue drain.
///
/// # Errors
/// Returns an error if the push fails.
pub fn push_single_target(target: &CacheTarget, out_path: &Path) -> Result<()> {
  if !target.push_command.is_empty() {
    push_via_command(&target.push_command, out_path)
  } else if let Some(url) = target.effective_push_url() {
    push_via_nix_copy(
      &url,
      target.signing_key.as_deref(),
      target.credentials.as_ref(),
      out_path,
    )
  } else if let Some(path) = &target.push_url_file {
    let url = Secret::from_file(Path::new(path))?;
    url.with_secret(|url| {
      push_via_nix_copy(
        url,
        target.signing_key.as_deref(),
        target.credentials.as_ref(),
        out_path,
      )
    })
  } else {
    Ok(())
  }
}

/// Check whether cache push is configured (via args or env).
#[must_use]
pub fn is_push_configured(cache: &CacheArgs) -> bool {
  !cache.resolve_targets().is_empty()
}

/// Push to all caches asynchronously via the xi daemon (if running) or a
/// detached background process as fallback.
fn push_to_cache_async(targets: &[CacheTarget], out_path: &Path) {
  let Some(out_path_str) = out_path.to_str() else {
    warn!("[cache] push skipped: path contains invalid UTF-8");
    return;
  };

  // Try daemon first (fire-and-forget via Unix socket)
  let socket_path = daemon_socket_path();
  if socket_path.exists() {
    for target in targets {
      let cache_url = target.push_url.clone().unwrap_or_default();
      match send_daemon_cache_push(
        &socket_path,
        out_path_str,
        &cache_url,
        &target.push_command,
        target.signing_key.as_deref(),
      ) {
        Ok(true) => {
          info!("[cache] push '{}' delegated to daemon", target.name);
          continue;
        },
        Ok(false) => {
          warn!(
            "[cache] daemon rejected push for '{}', falling back",
            target.name
          );
        },
        Err(e) => {
          debug!(
            "[cache] daemon not available ({e}), falling back to detached \
             process"
          );
        },
      }
      // Fallback for this target
      match spawn_detached_push(target, out_path) {
        Ok(pid) => {
          info!(
            "[cache] push '{}' running in background (pid {pid})",
            target.name
          );
        },
        Err(e) => warn!("[cache] push '{}' failed to spawn: {e}", target.name),
      }
    }
    return;
  }

  // Fallback: detached process for each target
  for target in targets {
    match spawn_detached_push(target, out_path) {
      Ok(pid) => {
        info!(
          "[cache] push '{}' running in background (pid {pid})",
          target.name
        );
      },
      Err(e) => warn!("[cache] push '{}' failed to spawn: {e}", target.name),
    }
  }
}

/// Resolve the daemon socket path (best-effort).
fn daemon_socket_path() -> std::path::PathBuf {
  let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
    .ok()
    .unwrap_or_else(|| format!("/run/user/{}", nix::unistd::getuid()));
  std::path::PathBuf::from(runtime_dir)
    .join("xi-develop")
    .join("daemon.sock")
}

/// Send a cache push request to the xi daemon over Unix socket.
fn send_daemon_cache_push(
  socket_path: &Path,
  store_path: &str,
  cache_url: &str,
  push_command: &[String],
  sign_key: Option<&str>,
) -> Result<bool> {
  use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
  };

  let mut stream = UnixStream::connect(socket_path)?;
  stream.set_read_timeout(Some(Duration::from_secs(5)))?;
  stream.set_write_timeout(Some(Duration::from_secs(5)))?;

  let request = serde_json::json!({
    "type": "CachePush",
    "store_path": store_path,
    "cache_url": cache_url,
    "push_command": push_command,
    "sign_key": sign_key,
  });

  let payload = serde_json::to_vec(&request)?;
  #[allow(clippy::cast_possible_truncation)]
  let len = (payload.len() as u32).to_le_bytes();
  stream.write_all(&len)?;
  stream.write_all(&payload)?;
  stream.flush()?;

  let mut len_buf = [0u8; 4];
  stream.read_exact(&mut len_buf)?;
  let resp_len = u32::from_le_bytes(len_buf) as usize;

  let mut resp_buf = vec![0u8; resp_len];
  stream.read_exact(&mut resp_buf)?;

  let resp: serde_json::Value = serde_json::from_slice(&resp_buf)?;
  Ok(
    resp
      .get("accepted")
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false),
  )
}

/// Resolve the cache push notification directory.
fn notification_dir() -> std::path::PathBuf {
  let state_home = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
    std::env::var("HOME")
      .map_or_else(|_| "/tmp".to_string(), |h| format!("{h}/.local/state"))
  });
  std::path::PathBuf::from(state_home).join("xi/cache")
}

/// Read and consume the cache push notification (if any).
///
/// Failed async pushes are automatically enqueued for retry.
///
/// Returns a rendered, colored string ready for display on stderr.
#[must_use]
pub fn drain_notification() -> Option<String> {
  let notif_dir = notification_dir();

  // Collect all notification files (one per target)
  let entries: Vec<_> = std::fs::read_dir(&notif_dir)
    .ok()?
    .filter_map(std::result::Result::ok)
    .filter(|e| {
      e.file_name()
        .to_str()
        .is_some_and(|n| n.starts_with("notification"))
    })
    .collect();

  if entries.is_empty() {
    return None;
  }

  let queue_config = crate::cache_queue::QueueConfig::default();
  let mut lines = Vec::new();
  for entry in &entries {
    let content = match std::fs::read_to_string(entry.path()) {
      Ok(c) if !c.is_empty() => c,
      _ => continue,
    };
    let _ = std::fs::remove_file(entry.path());

    #[allow(clippy::option_if_let_else)]
    let rendered =
      if let Ok(notif) = serde_json::from_str::<CacheNotification>(&content) {
        if notif.success {
          crate::style::labeled_status(
            crate::style::Icon::Success,
            "cache",
            &format!("push complete ({})", notif.label),
          )
        } else {
          if !notif.stderr.is_empty() {
            debug!("[cache] push stderr for {}: {}", notif.label, notif.stderr);
          }

          // Enqueue for retry so async failures aren't silently lost
          if !notif.store_path.is_empty() {
            let target = CacheTarget {
              name: notif.label.clone(),
              push_url: notif.push_url.clone(),
              signing_key: notif.signing_key.clone(),
              push_command: notif.push_command.clone(),
              ..Default::default()
            };
            crate::cache_queue::enqueue(
              &notif.store_path,
              &target,
              &notif.stderr,
              &queue_config,
            );
          }

          crate::style::labeled_status(
            crate::style::Icon::Error,
            "cache",
            &format!(
              "push failed ({}), queued for retry — run {} to flush",
              notif.label,
              crate::style::bold("xi cache retry")
            ),
          )
        }
      } else {
        // Legacy: raw text from old background processes
        content.trim().to_string()
      };

    lines.push(rendered);
  }

  if lines.is_empty() {
    return None;
  }

  Some(format!(
    "{}\n{}",
    crate::style::dim("── previous session ──"),
    lines.join("\n")
  ))
}

/// Spawn a detached push command that writes a notification on completion.
fn spawn_detached_push(target: &CacheTarget, out_path: &Path) -> Result<u32> {
  use std::process::Command;

  let out_path_str = out_path
    .to_str()
    .ok_or_else(|| color_eyre::eyre::eyre!("Path contains invalid UTF-8"))?;

  let notif_dir = notification_dir();
  // Use a target-specific notification file to avoid clobbering
  let safe_name: String = target
    .name
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect();
  let notif_path = notif_dir.join(format!("notification-{safe_name}"));

  let push_cmd_str = if !target.push_command.is_empty() {
    let escaped: Vec<String> = target
      .push_command
      .iter()
      .chain(std::iter::once(&out_path_str.to_string()))
      .map(|a| shell_escape(a))
      .collect();
    escaped.join(" ")
  } else if let Some(ref url) = target.push_url {
    let optimized_url = optimize_store_uri(url);

    let sign_part =
      target.signing_key.as_ref().map_or_else(String::new, |key| {
        format!(
          "nix store sign --recursive --key-file {} {} 2>/dev/null; ",
          shell_escape(key),
          shell_escape(out_path_str),
        )
      });

    format!(
      "{sign_part}AWS_REQUEST_CHECKSUM_CALCULATION=WHEN_REQUIRED nix copy \
       --to {} --quiet --quiet {}",
      shell_escape(&optimized_url),
      shell_escape(out_path_str),
    )
  } else {
    return Err(color_eyre::eyre::eyre!("No push target configured"));
  };

  // Pre-build the full JSON from Rust so the shell only needs to splice in
  // stderr. Include target info so drain_notification() can enqueue failures.
  let success_notif = CacheNotification {
    success:      true,
    label:        target.name.clone(),
    stderr:       String::new(),
    store_path:   out_path_str.to_string(),
    push_url:     None,
    signing_key:  None,
    push_command: vec![],
  };
  let success_json =
    serde_json::to_string(&success_notif).unwrap_or_else(|_| {
      r#"{"success":true,"label":"cache","stderr":"","store_path":""}"#
        .to_string()
    });
  let failure_prefix = serde_json::json!({
    "success": false,
    "label": target.name,
    "store_path": out_path_str,
    "push_url": target.push_url,
    "signing_key": target.signing_key,
    "push_command": target.push_command,
  });
  // Remove trailing `}` so we can append `,"stderr":"..."}` from the shell.
  let failure_prefix_str = {
    let s = serde_json::to_string(&failure_prefix)
      .unwrap_or_else(|_| r#"{"success":false,"label":"cache"}"#.to_string());
    // Strip trailing `}`
    s.strip_suffix('}').unwrap_or(&s).to_string()
  };

  let script = format!(
    r#"mkdir -p {notif_dir}
_xi_push_err=$({push_cmd} 2>&1)
_xi_push_rc=$?
if [ "$_xi_push_rc" -eq 0 ]; then
  cat > {notif_path} <<'__XI_EOF__'
{success_json}
__XI_EOF__
else
  _xi_stderr=$(printf '%s' "$_xi_push_err" | head -c 1024 | sed 's/\\/\\\\/g;s/"/\\"/g' | tr '\n' ' ')
  printf '%s,"stderr":"%s"}}' {failure_prefix} "$_xi_stderr" > {notif_path}
fi"#,
    notif_dir = shell_escape(notif_dir.to_str().unwrap_or("/tmp")),
    push_cmd = push_cmd_str,
    success_json = success_json,
    failure_prefix = shell_escape(&failure_prefix_str),
    notif_path = shell_escape(notif_path.to_str().unwrap_or("/dev/null")),
  );

  let child = Command::new("sh")
    .args(["-c", &script])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .context("Failed to spawn background push process")?;

  Ok(child.id())
}

/// Escape a string for safe use in shell commands.
fn shell_escape(s: &str) -> String {
  if s.chars().all(|c| {
    c.is_ascii_alphanumeric()
      || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '+' | ',')
  }) {
    s.to_string()
  } else {
    format!("'{}'", s.replace('\'', "'\\''"))
  }
}

/// Build the `nix copy --to <url>` [`std::process::Command`], injecting
/// native file-backed credentials as env vars scoped to this child
/// process only  -  never exported to xi's own process environment.
///
/// # Errors
/// Returns [`crate::sources::credentials::SecretError`] (via
/// `color_eyre`) when a configured credential file cannot be read.
/// `NotMounted`'s message distinguishes "not mounted yet" from other
/// I/O errors, per SC-BC-003.
fn build_copy_command(
  url: &str,
  credentials: Option<&CacheCredentials>,
  out_path: &Path,
) -> Result<std::process::Command> {
  let optimized_url = optimize_store_uri(url);
  let mut cmd = std::process::Command::new(find_real_nix_binary());
  cmd
    .args(["copy", "--to", &optimized_url])
    .arg("--quiet")
    .arg("--quiet")
    .arg(out_path)
    .env("AWS_REQUEST_CHECKSUM_CALCULATION", "WHEN_REQUIRED");

  if let Some(creds) = credentials {
    if let Some(path) = &creds.access_key_id_file {
      let secret = Secret::from_file(Path::new(path))?;
      secret.with_secret(|v| cmd.env("AWS_ACCESS_KEY_ID", v));
    }
    if let Some(path) = &creds.secret_access_key_file {
      let secret = Secret::from_file(Path::new(path))?;
      secret.with_secret(|v| cmd.env("AWS_SECRET_ACCESS_KEY", v));
    }
  }

  Ok(cmd)
}

fn push_via_nix_copy(
  url: &str,
  signing_key: Option<&str>,
  credentials: Option<&CacheCredentials>,
  out_path: &Path,
) -> Result<()> {
  info!("[cache] pushing to {}", optimize_store_uri(url));

  if let Some(key) = signing_key {
    debug!("[cache] signing store paths with key: {key}");
    let status = std::process::Command::new(find_real_nix_binary())
      .args(["store", "sign", "--recursive", "--key-file", key])
      .arg(out_path)
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .status()
      .wrap_err("Failed to run nix store sign")?;

    if !status.success() {
      warn!(
        "[cache] nix store sign exited with status {status}, continuing \
         without signing"
      );
    }
  }

  let output = build_copy_command(url, credentials, out_path)?
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .output()
    .wrap_err("Failed to run nix copy")?;

  if output.status.success() {
    info!("[cache] push complete");
    Ok(())
  } else {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let msg = if stderr.is_empty() {
      format!("nix copy exited with status {}", output.status)
    } else {
      format!("nix copy exited with status {}:\n{stderr}", output.status)
    };
    warn!("[cache] {msg}");
    color_eyre::eyre::bail!("{msg}")
  }
}

fn push_via_command(push_command: &[String], out_path: &Path) -> Result<()> {
  let Some((cmd, args)) = push_command.split_first() else {
    return Err(color_eyre::eyre::eyre!("push_command is empty"));
  };

  info!("[cache] pushing via {cmd}");

  let out_path_str = out_path.to_str().ok_or_else(|| {
    color_eyre::eyre::eyre!("Output path contains invalid UTF-8")
  })?;

  let output = std::process::Command::new(cmd)
    .args(args)
    .arg(out_path_str)
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .output()
    .wrap_err_with(|| format!("Failed to run push command: {cmd}"))?;

  if output.status.success() {
    info!("[cache] push complete");
    Ok(())
  } else {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let msg = if stderr.is_empty() {
      format!("{cmd} exited with status {}", output.status)
    } else {
      format!("{cmd} exited with status {}:\n{stderr}", output.status)
    };
    warn!("[cache] {msg}");
    color_eyre::eyre::bail!("{msg}")
  }
}

/// Resolve the effective trusted public key for a target's pull
/// configuration.
///
/// An explicit `pull_public_key` always wins; otherwise, when
/// `signing_key` is set, the public half is derived via `nix key
/// convert-secret-to-public` and cached next to the key file.
#[must_use]
pub fn resolve_pull_public_key(target: &CacheTarget) -> Option<String> {
  if let Some(key) = &target.pull_public_key {
    return Some(key.clone());
  }
  let signing_key = target.signing_key.as_ref()?;
  match derive_public_key(signing_key) {
    Ok(key) => Some(key),
    Err(e) => {
      warn!(
        "[cache] failed to derive public key for '{}' from signing key: {e}",
        target.name
      );
      None
    },
  }
}

/// Short timeout for the `xi cache doctor` endpoint-reachability probe,
/// same posture as `cache_prestage.rs`'s `KEY_FETCH_TIMEOUT`  -  this is
/// a diagnostic, not a build; it must fail fast.
const DOCTOR_ENDPOINT_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(10);

/// Check that an `http(s)://` substituter URL actually answers, for
/// `xi cache doctor`. Not meaningful for an `s3://`/`ssh://` push URL,
/// which the caller is expected to skip.
///
/// # Errors
/// Returns an error naming why the endpoint is considered unreachable
/// (connection failure or a non-2xx/3xx status).
pub fn check_endpoint_reachable(url: &str) -> Result<()> {
  let client = reqwest::blocking::Client::builder()
    .timeout(DOCTOR_ENDPOINT_TIMEOUT)
    .build()
    .map_err(|e| {
      color_eyre::eyre::eyre!("could not build an HTTP client: {e}")
    })?;

  let trimmed = url.trim_end_matches('/');
  let resp = client
    .get(trimmed)
    .send()
    .map_err(|e| color_eyre::eyre::eyre!("unreachable: {e}"))?;
  if resp.status().is_success() || resp.status().is_redirection() {
    Ok(())
  } else {
    color_eyre::eyre::bail!("answered HTTP {}", resp.status().as_u16())
  }
}

/// Check that `target`'s signing key (if any) is readable and actually
/// produces a public key via `nix key convert-secret-to-public`.
///
/// Used by `xi cache doctor`, which wants this as an independent check
/// (not folded into [`resolve_pull_public_key`]'s silent fallback) so a
/// broken signing key is reported by name rather than quietly resolving
/// to "no pull key".
///
/// Returns `None` when no signing key is configured at all.
#[must_use]
pub fn check_signing_key(target: &CacheTarget) -> Option<Result<String>> {
  target.signing_key.as_deref().map(derive_public_key)
}

/// Derive the public half of `signing_key_path` via `nix key
/// convert-secret-to-public`, caching the result next to the key file
/// (`<signing_key>.pub.cache`), invalidated on the key file's mtime.
fn derive_public_key(signing_key_path: &str) -> Result<String> {
  let key_path = Path::new(signing_key_path);
  let mtime_secs = mtime_secs(key_path)?;
  let cache_path = PathBuf::from(format!("{}.pub.cache", key_path.display()));

  if let Some(cached) = read_cached_pub_key(&cache_path, mtime_secs) {
    return Ok(cached);
  }

  let key_contents = std::fs::read(key_path).wrap_err_with(|| {
    format!("failed to read signing key {signing_key_path}")
  })?;

  let mut child = std::process::Command::new(find_real_nix_binary())
    .args(["key", "convert-secret-to-public"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .wrap_err("failed to spawn nix key convert-secret-to-public")?;

  child
    .stdin
    .take()
    .ok_or_else(|| color_eyre::eyre::eyre!("failed to open child stdin"))?
    .write_all(&key_contents)
    .wrap_err(
      "failed to write signing key to nix key convert-secret-to-public",
    )?;

  let output = child
    .wait_with_output()
    .wrap_err("failed to wait for nix key convert-secret-to-public")?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    color_eyre::eyre::bail!(
      "nix key convert-secret-to-public exited with status {}: {}",
      output.status,
      stderr.trim()
    );
  }

  let public_key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
  write_cached_pub_key(&cache_path, mtime_secs, &public_key);
  Ok(public_key)
}

fn mtime_secs(path: &Path) -> Result<u64> {
  let modified = std::fs::metadata(path)
    .wrap_err_with(|| format!("failed to stat {}", path.display()))?
    .modified()
    .wrap_err_with(|| format!("failed to read mtime of {}", path.display()))?;
  Ok(
    modified
      .duration_since(UNIX_EPOCH)
      .map_or(0, |d| d.as_secs()),
  )
}

/// Format: first line is the source key's mtime (seconds), second line
/// is the derived public key.
fn read_cached_pub_key(cache_path: &Path, mtime_secs: u64) -> Option<String> {
  let content = std::fs::read_to_string(cache_path).ok()?;
  let (cached_mtime, pub_key) = content.split_once('\n')?;
  if cached_mtime.trim().parse::<u64>().ok()? == mtime_secs {
    Some(pub_key.trim().to_owned())
  } else {
    None
  }
}

fn write_cached_pub_key(cache_path: &Path, mtime_secs: u64, pub_key: &str) {
  if let Err(e) =
    std::fs::write(cache_path, format!("{mtime_secs}\n{pub_key}\n"))
  {
    debug!(
      "[cache] failed to cache derived public key at {}: {e}",
      cache_path.display()
    );
  }
}

/// Auto-tune a Nix store URI for better push performance.
fn optimize_store_uri(url: &str) -> String {
  let mut uri = url.to_string();

  if !uri.contains("compression=") {
    let sep = if uri.contains('?') { "&" } else { "?" };
    uri = format!("{uri}{sep}compression=zstd");
  }

  if uri.starts_with("s3://") && !uri.contains("multipart-upload=") {
    uri = format!("{uri}&multipart-upload=true");
  }

  uri
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod push_url_file_tests {
  use super::push_single_target;
  use crate::args::CacheTarget;

  #[test]
  fn missing_push_url_file_is_pending_not_a_generic_error() {
    let target = CacheTarget {
      push_url_file: Some("/does/not/exist/cache-url".to_string()),
      ..CacheTarget::default()
    };

    let err =
      push_single_target(&target, std::path::Path::new("/nix/store/abc-pkg"))
        .unwrap_err();

    assert!(
      err.to_string().contains("not mounted yet"),
      "expected a Pending-style message, got: {err}"
    );
  }

}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod credentials_tests {
  use std::ffi::OsStr;

  use super::build_copy_command;
  use crate::args::CacheCredentials;

  #[test]
  fn credentials_are_scoped_to_the_child_command_only() {
    let dir = tempfile::tempdir().unwrap();
    let akid_path = dir.path().join("akid");
    let secret_path = dir.path().join("secret");
    std::fs::write(&akid_path, "AKIAFAKE123\n").unwrap();
    std::fs::write(&secret_path, "shh-its-a-secret\n").unwrap();

    let credentials = CacheCredentials {
      access_key_id_file:     Some(akid_path.display().to_string()),
      secret_access_key_file: Some(secret_path.display().to_string()),
    };

    let cmd = build_copy_command(
      "s3://my-cache",
      Some(&credentials),
      std::path::Path::new("/nix/store/abc-pkg"),
    )
    .unwrap();

    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.iter().any(|(k, v)| {
      *k == OsStr::new("AWS_ACCESS_KEY_ID")
        && *v == Some(OsStr::new("AKIAFAKE123"))
    }));
    assert!(envs.iter().any(|(k, v)| {
      *k == OsStr::new("AWS_SECRET_ACCESS_KEY")
        && *v == Some(OsStr::new("shh-its-a-secret"))
    }));

    // SC-BC-002: never exported to xi's own process environment.
    assert!(std::env::var("AWS_ACCESS_KEY_ID").is_err());
    assert!(std::env::var("AWS_SECRET_ACCESS_KEY").is_err());
  }

  #[test]
  fn missing_credential_file_is_pending_not_a_generic_error() {
    let credentials = CacheCredentials {
      access_key_id_file:     Some("/does/not/exist/akid".to_string()),
      secret_access_key_file: None,
    };

    let err = build_copy_command(
      "s3://my-cache",
      Some(&credentials),
      std::path::Path::new("/nix/store/abc-pkg"),
    )
    .unwrap_err();

    assert!(
      err.to_string().contains("not mounted yet"),
      "expected a Pending-style message, got: {err}"
    );
  }

  #[test]
  fn no_credentials_configured_sets_no_aws_env_vars() {
    let cmd = build_copy_command(
      "s3://my-cache",
      None,
      std::path::Path::new("/nix/store/abc-pkg"),
    )
    .unwrap();
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(
      !envs
        .iter()
        .any(|(k, _)| *k == OsStr::new("AWS_ACCESS_KEY_ID"))
    );
  }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, reason = "Fine in tests")]
mod doctor_tests {
  use super::{check_endpoint_reachable, check_signing_key};
  use crate::args::CacheTarget;

  #[test]
  fn unreachable_endpoint_is_an_error() {
    // Nothing listens on port 0 after the OS reclaims it; this just
    // needs a connection that is refused quickly, no real network.
    let err = check_endpoint_reachable("http://127.0.0.1:1").unwrap_err();
    assert!(err.to_string().contains("unreachable"));
  }

  #[test]
  fn no_signing_key_is_none() {
    assert!(check_signing_key(&CacheTarget::default()).is_none());
  }

  #[test]
  fn unreadable_signing_key_names_the_problem() {
    let target = CacheTarget {
      signing_key: Some("/does/not/exist/key".to_string()),
      ..Default::default()
    };
    let result = check_signing_key(&target).expect("signing_key was set");
    assert!(result.is_err());
  }
}

#[cfg(test)]
mod pull_key_tests {
  use super::resolve_pull_public_key;
  use crate::args::CacheTarget;

  #[test]
  fn explicit_pull_public_key_wins_without_shelling_out() {
    let target = CacheTarget {
      pull_public_key: Some("cache.example.com:abc123=".to_string()),
      signing_key: Some("/does/not/exist/signing-key".to_string()),
      ..Default::default()
    };
    // If this fell through to derivation it would try to read the
    // nonexistent signing key and fail; it must not.
    assert_eq!(
      resolve_pull_public_key(&target),
      Some("cache.example.com:abc123=".to_string())
    );
  }

  #[test]
  fn no_signing_key_and_no_explicit_key_resolves_to_none() {
    assert_eq!(resolve_pull_public_key(&CacheTarget::default()), None);
  }
}

#[cfg(test)]
mod required_target_tests {
  use super::{PushResult, TargetOutcome, enforce_required};

  fn outcome(name: &str, required: bool, failed: bool) -> TargetOutcome {
    TargetOutcome {
      name: name.to_string(),
      required,
      error: failed.then(|| color_eyre::eyre::eyre!("push failed")),
    }
  }

  #[test]
  fn required_target_failure_is_an_error() {
    let result = PushResult {
      outcomes: vec![outcome("ci", true, true)],
    };
    assert!(result.required_failed());
    assert!(enforce_required(&result).is_err());
  }

  #[test]
  fn non_required_target_failure_is_not_an_error() {
    let result = PushResult {
      outcomes: vec![outcome("mirror", false, true)],
    };
    assert!(!result.required_failed());
    assert!(enforce_required(&result).is_ok());
  }

  #[test]
  fn required_target_success_is_not_an_error() {
    let result = PushResult {
      outcomes: vec![outcome("ci", true, false)],
    };
    assert!(enforce_required(&result).is_ok());
  }
}

#[cfg(test)]
mod parallel_push_tests {
  use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
  };

  use super::run_bounded;

  #[test]
  fn one_slow_item_does_not_block_the_others() {
    let items = vec![0, 1, 2, 3, 4];
    let started = Instant::now();
    let results = run_bounded(&items, 4, |i| {
      if *i == 0 {
        std::thread::sleep(Duration::from_millis(200));
      }
      *i
    });
    // The 4 fast items plus the one slow item all complete, but the
    // wall-clock is close to the slow item's own delay, not 5x it.
    assert_eq!(results.len(), 5);
    assert!(started.elapsed() < Duration::from_millis(400));
  }

  #[test]
  fn concurrency_respects_the_configured_bound() {
    let items = vec![0; 5];
    let concurrent = AtomicUsize::new(0);
    let max_seen = AtomicUsize::new(0);

    run_bounded(&items, 2, |_| {
      let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
      max_seen.fetch_max(now, Ordering::SeqCst);
      std::thread::sleep(Duration::from_millis(30));
      concurrent.fetch_sub(1, Ordering::SeqCst);
    });

    assert!(max_seen.load(Ordering::SeqCst) <= 2);
  }

  #[test]
  fn results_preserve_input_order() {
    let items = vec![4, 3, 2, 1, 0];
    let results = run_bounded(&items, 3, |i| *i);
    assert_eq!(results, items);
  }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use super::{optimize_store_uri, shell_escape};

  #[test]
  fn adds_zstd_compression_to_bare_s3_url() {
    let result = optimize_store_uri("s3://my-cache");
    assert!(result.contains("compression=zstd"));
    assert!(result.contains("multipart-upload=true"));
  }

  #[test]
  fn preserves_existing_compression() {
    let result = optimize_store_uri("s3://my-cache?compression=xz");
    assert!(result.contains("compression=xz"));
    assert!(!result.contains("compression=zstd"));
  }

  #[test]
  fn handles_ssh_url() {
    let result = optimize_store_uri("ssh://server");
    assert!(result.contains("compression=zstd"));
    assert!(!result.contains("multipart-upload"));
  }

  #[test]
  fn handles_file_url() {
    let result = optimize_store_uri("file:///tmp/cache");
    assert!(result.contains("compression=zstd"));
  }

  #[test]
  fn shell_escape_quotes_urls_with_ampersand() {
    let url = "s3://bucket?region=eu-west-3&compression=zstd";
    let escaped = shell_escape(url);
    assert!(
      escaped.starts_with('\''),
      "URL with & must be single-quoted: {escaped}"
    );
    assert!(escaped.ends_with('\''));
  }

  #[test]
  fn shell_escape_quotes_urls_with_question_mark() {
    let url = "s3://bucket?region=eu-west-3";
    let escaped = shell_escape(url);
    assert!(
      escaped.starts_with('\''),
      "URL with ? must be single-quoted: {escaped}"
    );
  }

  #[test]
  fn shell_escape_leaves_simple_paths_unquoted() {
    assert_eq!(shell_escape("/nix/store/abc-123"), "/nix/store/abc-123");
  }

  #[test]
  fn notification_json_parses_success() {
    let json = r#"{"success":true,"label":"my-s3","stderr":""}"#;
    let v: super::CacheNotification = serde_json::from_str(json).unwrap();
    assert!(v.success);
    assert_eq!(v.label, "my-s3");
  }

  #[test]
  fn notification_json_parses_failure() {
    let json =
      r#"{"success":false,"label":"cachix","stderr":"connection refused"}"#;
    let v: super::CacheNotification = serde_json::from_str(json).unwrap();
    assert!(!v.success);
    assert_eq!(v.stderr, "connection refused");
  }
}
