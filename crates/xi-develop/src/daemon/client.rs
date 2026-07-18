//! Daemon client — connects to Unix socket, sends requests.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use color_eyre::Result;
use color_eyre::eyre::bail;

use super::protocol::{
  CachePushRequest, DaemonRequest, DaemonResponse, DeregisterRequest,
  DeregisterResponse, PromptRequest, PromptResponse, StatusResponse,
  read_message, write_message,
};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Connect to the daemon socket.
///
/// # Errors
/// Returns an error if connection fails or times out.
pub fn connect(socket_path: &Path) -> Result<UnixStream> {
  let stream = UnixStream::connect(socket_path)
    .map_err(|e| color_eyre::eyre::eyre!("Cannot connect to daemon: {e}"))?;
  stream.set_read_timeout(Some(IO_TIMEOUT))?;
  stream.set_write_timeout(Some(IO_TIMEOUT))?;
  Ok(stream)
}

/// Send a request and receive a response.
///
/// # Errors
/// Returns an error if I/O or deserialization fails.
pub fn call(
  stream: &UnixStream,
  request: &DaemonRequest,
) -> Result<DaemonResponse> {
  let mut writer = BufWriter::new(stream);
  let mut reader = BufReader::new(stream);

  write_message(&mut writer, request)?;
  let response: DaemonResponse = read_message(&mut reader)?;
  Ok(response)
}

/// Send a status request.
///
/// # Errors
/// Returns an error if connection or call fails.
pub fn status(socket_path: &Path) -> Result<StatusResponse> {
  let stream = connect(socket_path)?;
  match call(&stream, &DaemonRequest::Status)? {
    DaemonResponse::Status(resp) => Ok(resp),
    DaemonResponse::Error { message } => bail!("Daemon error: {message}"),
    _ => bail!("Unexpected response type"),
  }
}

/// Send a shutdown request.
///
/// # Errors
/// Returns an error if connection or call fails.
pub fn shutdown(socket_path: &Path) -> Result<()> {
  let stream = connect(socket_path)?;
  let _ = call(&stream, &DaemonRequest::Shutdown);
  Ok(())
}

/// Send a cache push request to the daemon.
///
/// The daemon runs the push in a background thread and sends a desktop
/// notification when complete. Returns immediately after the daemon
/// acknowledges the request.
///
/// # Errors
/// Returns an error if connection or call fails.
pub fn cache_push(
  socket_path: &Path,
  store_path: &str,
  cache_url: &str,
  push_command: &[String],
  sign_key: Option<&str>,
) -> Result<bool> {
  let stream = connect(socket_path)?;
  let request = DaemonRequest::CachePush(CachePushRequest {
    store_path: store_path.to_string(),
    cache_url: cache_url.to_string(),
    push_command: push_command.to_vec(),
    sign_key: sign_key.map(String::from),
  });
  match call(&stream, &request)? {
    DaemonResponse::CachePush(resp) => Ok(resp.accepted),
    DaemonResponse::Error { message } => bail!("Daemon error: {message}"),
    _ => bail!("Unexpected response type"),
  }
}

/// Send a prompt request and return the response.
///
/// # Errors
/// Returns an error if connection or call fails.
pub fn prompt(
  socket_path: &Path,
  consumer_pid: u32,
  target: &str,
  cwd: &str,
  is_subshell: bool,
  parent_pid: Option<u32>,
) -> Result<PromptResponse> {
  let stream = connect(socket_path)?;
  let request = DaemonRequest::Prompt(PromptRequest {
    consumer_pid,
    target: target.to_string(),
    cwd: cwd.to_string(),
    is_subshell,
    parent_pid,
  });
  match call(&stream, &request)? {
    DaemonResponse::Prompt(resp) => Ok(resp),
    DaemonResponse::Error { message } => bail!("Daemon error: {message}"),
    _ => bail!("Unexpected response type"),
  }
}

/// Send a deregister request (shell exiting).
///
/// # Errors
/// Returns an error if connection or call fails.
pub fn deregister(
  socket_path: &Path,
  consumer_pid: u32,
) -> Result<DeregisterResponse> {
  let stream = connect(socket_path)?;
  let request = DaemonRequest::Deregister(DeregisterRequest { consumer_pid });
  match call(&stream, &request)? {
    DaemonResponse::Deregister(resp) => Ok(resp),
    DaemonResponse::Error { message } => bail!("Daemon error: {message}"),
    _ => bail!("Unexpected response type"),
  }
}

/// Check if daemon is alive (socket connectable).
#[must_use]
pub fn is_alive(socket_path: &Path) -> bool {
  connect(socket_path).is_ok()
}
