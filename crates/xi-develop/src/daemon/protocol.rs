//! Daemon protocol types — JSON over Unix socket.
//!
//! Wire format: 4-byte little-endian length prefix + JSON payload.

use serde::{Deserialize, Serialize};

/// Blocking eval request (for `xi develop` sync entry).
#[derive(Debug, Serialize, Deserialize)]
pub struct EvalRequest {
  pub target: String,
}

/// Eval response with package changes.
#[derive(Debug, Serialize, Deserialize)]
pub struct EvalResponse {
  pub env_file_path: String,
  pub changes: Vec<PackageChange>,
  pub eval_duration_ms: u64,
}

/// Status request/response.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
  pub state: DaemonState,
  pub uptime_secs: u64,
  pub consumer_count: u32,
  pub current_target: String,
  pub package_count: u32,
  pub version: String,
  /// Store paths currently being pushed to cache.
  pub active_cache_pushes: Vec<String>,
}

/// Shutdown request (empty).
#[derive(Debug, Serialize, Deserialize)]
pub struct ShutdownRequest {}

/// Notification severity/type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotifKind {
  /// Devshell is evaluating.
  Loading,
  /// Devshell ready / updated (with optional package diff).
  Success,
  /// Informational (hook stdout, status messages).
  Info,
  /// Non-fatal warning (offline, stale, degraded).
  Warn,
  /// Eval or hook failure.
  Error,
}

/// A notification message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
  pub message: String,
  pub kind: NotifKind,
}

impl Notification {
  /// Create a loading notification.
  pub fn loading(msg: impl Into<String>) -> Self {
    Self {
      message: msg.into(),
      kind: NotifKind::Loading,
    }
  }

  /// Create a success notification.
  pub fn success(msg: impl Into<String>) -> Self {
    Self {
      message: msg.into(),
      kind: NotifKind::Success,
    }
  }

  /// Create an info notification.
  pub fn info(msg: impl Into<String>) -> Self {
    Self {
      message: msg.into(),
      kind: NotifKind::Info,
    }
  }

  /// Create a warning notification.
  pub fn warn(msg: impl Into<String>) -> Self {
    Self {
      message: msg.into(),
      kind: NotifKind::Warn,
    }
  }

  /// Create an error notification.
  pub fn error(msg: impl Into<String>) -> Self {
    Self {
      message: msg.into(),
      kind: NotifKind::Error,
    }
  }

  /// Format with ANSI colors for terminal display.
  #[must_use]
  pub fn render(&self) -> String {
    use xi_core::style::Icon;
    let icon = match self.kind {
      NotifKind::Loading => Icon::Loading,
      NotifKind::Success => Icon::Success,
      NotifKind::Info => Icon::Info,
      NotifKind::Warn => Icon::Warn,
      NotifKind::Error => Icon::Error,
    };
    format!("{} {}", icon.render_with_label("xi"), self.message)
  }
}

/// Package change entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageChange {
  pub kind: ChangeKind,
  pub name: String,
  pub old_version: String,
  pub new_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
  Added,
  Removed,
  Updated,
}

/// Request to push a store path to a binary cache in the background.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachePushRequest {
  /// Nix store path to push.
  pub store_path: String,
  /// Cache URL (s3://, ssh://, etc.) — if empty, uses `push_command`.
  pub cache_url: String,
  /// External push command (e.g. `["cachix", "push", "mycache"]`).
  pub push_command: Vec<String>,
  /// Signing key path (optional).
  pub sign_key: Option<String>,
}

/// Response to a cache push request.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachePushResponse {
  /// Whether the push was accepted (queued).
  pub accepted: bool,
}

/// Daemon state — covers all lifecycle phases and degraded modes.
///
/// Adding a variant here requires extending [`crate::daemon::state_meta`] as
/// well. That module contains a compile-time exhaustive `match` that fails
/// the build if a variant is added without a catalog entry (SC-016).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonState {
  Starting,
  Evaluating,
  Ready,
  BuildFailed { error: String, retry_count: u32 },
  WatcherDegraded,
  ConfigError { error: String },
  ShuttingDown,
  /// v3: A read-only query has an in-flight job producing the value.
  ///
  /// Emitted by `Query::*` reads when the value is not yet materialised
  /// but a job is running. Carries the running job id.
  Pending { job_id: String },
  /// v3: The requested value has no producer and no job is running.
  ///
  /// Emitted by `Query::*` reads for values that never existed and are
  /// not being computed (e.g. a query for a non-existent target).
  Missing,
  /// v3: The daemon is serving from a degraded source (offline cache,
  /// stale watcher, partial results).
  Degraded { reason: String },
  /// v3: A job has made no progress for longer than the stuck threshold.
  Stuck { job_id: String, stuck_ms: u64 },
  /// v3: The daemon is aborting a stuck job and restarting itself.
  SelfHealing,
}

/// Request sent by `xi develop prompt` to the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct PromptRequest {
  pub consumer_pid: u32,
  pub target: String,
  pub cwd: String,
  pub is_subshell: bool,
  pub parent_pid: Option<u32>,
}

/// Structured response from daemon to prompt command.
/// The Rust binary generates shell code from this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResponse {
  pub should_source_env: bool,
  pub env_file_path: Option<String>,
  pub should_source_hook: bool,
  pub hook_file_path: Option<String>,
  pub should_exit: bool,
  pub should_spawn_subshell: bool,
  pub spawn_flake_root: Option<String>,
  pub notifications: Vec<Notification>,
  pub daemon_state: DaemonState,
  pub is_trusted: bool,
}

/// Request sent on subshell EXIT to deregister a consumer.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeregisterRequest {
  pub consumer_pid: u32,
}

/// Response to a deregister request.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeregisterResponse {
  pub was_registered: bool,
  pub remaining_consumers: u32,
}

/// Envelope for all daemon messages (tagged union for multiplexing).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonRequest {
  Eval(EvalRequest),
  CachePush(CachePushRequest),
  Prompt(PromptRequest),
  Deregister(DeregisterRequest),
  Status,
  Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonResponse {
  Eval(EvalResponse),
  CachePush(CachePushResponse),
  Prompt(PromptResponse),
  Deregister(DeregisterResponse),
  Status(StatusResponse),
  Shutdown,
  Error { message: String },
}

/// Write a length-prefixed JSON message to a writer.
///
/// # Errors
/// Returns an error if serialization or I/O fails.
pub fn write_message<W: std::io::Write, T: Serialize>(
  writer: &mut W,
  msg: &T,
) -> std::io::Result<()> {
  let json = serde_json::to_vec(msg)
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
  #[allow(clippy::cast_possible_truncation)] // messages won't exceed 4GB
  let len = (json.len() as u32).to_le_bytes();
  writer.write_all(&len)?;
  writer.write_all(&json)?;
  writer.flush()
}

/// Read a length-prefixed JSON message from a reader.
///
/// # Errors
/// Returns an error if deserialization or I/O fails.
pub fn read_message<R: std::io::Read, T: for<'de> Deserialize<'de>>(
  reader: &mut R,
) -> std::io::Result<T> {
  let mut len_buf = [0u8; 4];
  reader.read_exact(&mut len_buf)?;
  let len = u32::from_le_bytes(len_buf) as usize;

  // Sanity check: max 16MB
  if len > 16 * 1024 * 1024 {
    return Err(std::io::Error::new(
      std::io::ErrorKind::InvalidData,
      format!("Message too large: {len} bytes"),
    ));
  }

  let mut buf = vec![0u8; len];
  reader.read_exact(&mut buf)?;
  serde_json::from_slice(&buf)
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn write_read_message_roundtrip() {
    let req = DaemonRequest::Status;
    let mut buf = Vec::new();
    write_message(&mut buf, &req).expect("write");
    let mut cursor = std::io::Cursor::new(buf);
    let decoded: DaemonRequest = read_message(&mut cursor).expect("read");
    assert!(matches!(decoded, DaemonRequest::Status));
  }

  #[test]
  fn daemon_state_serde() {
    let state = DaemonState::Evaluating;
    let json = serde_json::to_string(&state).expect("serialize");
    let decoded: DaemonState =
      serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, DaemonState::Evaluating);
  }
}
