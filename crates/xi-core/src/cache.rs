use std::{path::Path, process::Stdio};

use color_eyre::{Result, eyre::Context};
use nix_command::find_real_nix_binary;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::args::{CacheArgs, CacheTarget};

/// Notification written by background cache push processes.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheNotification {
  pub success: bool,
  pub label: String,
  #[serde(default)]
  pub stderr: String,
  #[serde(default)]
  pub store_path: String,
  #[serde(default)]
  pub push_url: Option<String>,
  #[serde(default)]
  pub signing_key: Option<String>,
  #[serde(default)]
  pub push_command: Vec<String>,
}

/// Push a built store path to all configured binary caches.
///
/// Push is best-effort: failures emit a warning but do not block
/// the calling operation. Failed pushes are enqueued for later retry.
pub fn push_to_cache(cache: &CacheArgs, out_path: &Path) {
  let targets = cache.resolve_targets();
  if targets.is_empty() {
    return;
  }

  if cache.async_push {
    push_to_cache_async(&targets, out_path);
    return;
  }

  let out_path_str = out_path.to_str().unwrap_or_default();
  let queue_config = crate::cache_queue::QueueConfig::default();
  for target in &targets {
    if let Err(e) = push_single_target(target, out_path) {
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
  }
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
  } else if let Some(ref url) = target.push_url {
    push_via_nix_copy(url, target.signing_key.as_deref(), out_path)
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
            "[cache] daemon not available ({e}), falling back to detached process"
          );
        },
      }
      // Fallback for this target
      match spawn_detached_push(target, out_path) {
        Ok(pid) => info!(
          "[cache] push '{}' running in background (pid {pid})",
          target.name
        ),
        Err(e) => warn!("[cache] push '{}' failed to spawn: {e}", target.name),
      }
    }
    return;
  }

  // Fallback: detached process for each target
  for target in targets {
    match spawn_detached_push(target, out_path) {
      Ok(pid) => info!(
        "[cache] push '{}' running in background (pid {pid})",
        target.name
      ),
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
  use std::io::{Read, Write};
  use std::os::unix::net::UnixStream;
  use std::time::Duration;

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
      "{sign_part}AWS_REQUEST_CHECKSUM_CALCULATION=WHEN_REQUIRED \
       nix copy --to {} --quiet --quiet {}",
      shell_escape(&optimized_url),
      shell_escape(out_path_str),
    )
  } else {
    return Err(color_eyre::eyre::eyre!("No push target configured"));
  };

  // Pre-build the full JSON from Rust so the shell only needs to splice in
  // stderr. Include target info so drain_notification() can enqueue failures.
  let success_notif = CacheNotification {
    success: true,
    label: target.name.clone(),
    stderr: String::new(),
    store_path: out_path_str.to_string(),
    push_url: None,
    signing_key: None,
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

fn push_via_nix_copy(
  url: &str,
  signing_key: Option<&str>,
  out_path: &Path,
) -> Result<()> {
  let optimized_url = optimize_store_uri(url);
  info!("[cache] pushing to {optimized_url}");

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

  let output = std::process::Command::new(find_real_nix_binary())
    .args(["copy", "--to", &optimized_url])
    .arg("--quiet")
    .arg("--quiet")
    .arg(out_path)
    .env("AWS_REQUEST_CHECKSUM_CALCULATION", "WHEN_REQUIRED")
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
