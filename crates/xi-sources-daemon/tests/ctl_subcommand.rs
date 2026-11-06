//! The `ctl` subcommand ([[xi-push-target-pull]] task 2.1): spawns the real
//! `xi-sources-daemon` binary, feeds it one request frame on `stdin`, and
//! checks the one reply frame it writes to `stdout` round-trips through a
//! real [`ControlServer`] - not just `relay_stdio` called in-process.
//!
//! `XI_SOURCES_STATE_DIR` points the child at the test's own temp dir, so
//! it resolves the same `control.sock` the test's `ControlServer` is
//! listening on, without needing root for the `system` scope's real paths.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{
  io::{Read, Write},
  path::PathBuf,
  process::{Command, Stdio},
  sync::Arc,
  time::Duration,
};

use tempfile::TempDir;
use xi_core::sources::{
  Scope,
  SourcesId,
  config::SyncMode,
  pause::{PauseReply, PauseRequest},
};
use xi_sources_daemon::{
  control::{self, AdvanceContext, AdvanceNow, AdvanceReply, ControlServer},
  exec::recorder::RecordingExec,
  singleton::Singleton,
};

/// An [`AdvanceContext`] for a test that never sends `AdvanceNow`: the
/// paths are never touched, so they do not need to exist.
fn dummy_context() -> AdvanceContext {
  AdvanceContext {
    sources_id:      SourcesId::new(Scope::System, "https://example.invalid/unused.git"),
    sources_dir:     PathBuf::from("/nonexistent-ctl-subcommand-test/sources"),
    state_dir:       PathBuf::from("/nonexistent-ctl-subcommand-test/state"),
    remote_url:      "https://example.invalid/unused.git".to_owned(),
    branch:          "main".to_owned(),
    require_signed:  false,
    allowed_signers: None,
    gpg_public_keys: None,
    on_advance:      None,
    sync_mode:       SyncMode::Prompt,
  }
}

fn write_frame(writer: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
  #[allow(clippy::cast_possible_truncation, reason = "frames are tiny")]
  let len = (payload.len() as u32).to_le_bytes();
  writer.write_all(&len)?;
  writer.write_all(payload)?;
  writer.flush()
}

fn read_frame(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
  let mut len_buf = [0_u8; 4];
  reader.read_exact(&mut len_buf)?;
  let len = u32::from_le_bytes(len_buf) as usize;
  let mut buf = vec![0_u8; len];
  reader.read_exact(&mut buf)?;
  Ok(buf)
}

#[test]
fn ctl_subcommand_relays_one_request_and_one_reply() {
  let tmp = TempDir::new().expect("tempdir");
  let socket_path = control::socket_path(tmp.path());
  let server = ControlServer::spawn(
    socket_path,
    Duration::from_secs(30),
    Arc::new(Singleton::new()),
    Box::new(RecordingExec::always_ok()),
    dummy_context(),
  )
  .expect("spawn control server");

  let exe = env!("CARGO_BIN_EXE_xi-sources-daemon");
  let mut child = Command::new(exe)
    .args(["--scope", "user", "ctl"])
    .env("XI_SOURCES_STATE_DIR", tmp.path())
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn ctl subcommand");

  let request = PauseRequest::Resume {
    requester_pid: std::process::id(),
  };
  let payload = serde_json::to_vec(&request).expect("encode request");
  {
    let mut stdin = child.stdin.take().expect("child stdin");
    write_frame(&mut stdin, &payload).expect("write request frame");
  }

  let reply_bytes = {
    let mut stdout = child.stdout.take().expect("child stdout");
    read_frame(&mut stdout).expect("read reply frame")
  };

  let output = child.wait_with_output().expect("ctl subcommand exits");
  assert!(
    output.status.success(),
    "ctl subcommand exited with {:?}; stderr: {}",
    output.status,
    String::from_utf8_lossy(&output.stderr)
  );

  let reply: PauseReply =
    serde_json::from_slice(&reply_bytes).expect("decode reply frame");
  assert_eq!(reply, PauseReply::NotPaused);

  server.shutdown();
}

/// Same relay, but with an [`AdvanceNow`] request ([[xi-push-target-pull]]
/// task 2.2) instead of a [`PauseRequest`] - the `ctl` subcommand and the
/// untagged `ControlRequest` decode on the server side must tell the two
/// request shapes apart over the real stdio-to-socket relay, not just
/// in-process.
///
/// Holds the shared [`Singleton`] gate before sending the request so the
/// server answers `Busy` without needing a real git remote behind
/// `dummy_context`'s nonexistent paths.
#[test]
fn ctl_subcommand_relays_an_advance_now_request_and_reply() {
  let tmp = TempDir::new().expect("tempdir");
  let socket_path = control::socket_path(tmp.path());
  let gate = Arc::new(Singleton::new());
  let server = ControlServer::spawn(
    socket_path,
    Duration::from_secs(30),
    Arc::clone(&gate),
    Box::new(RecordingExec::always_ok()),
    dummy_context(),
  )
  .expect("spawn control server");

  let permit = gate.enter();
  server.begin_tick();

  let exe = env!("CARGO_BIN_EXE_xi-sources-daemon");
  let mut child = Command::new(exe)
    .args(["--scope", "user", "ctl"])
    .env("XI_SOURCES_STATE_DIR", tmp.path())
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn ctl subcommand");

  let request = AdvanceNow {
    to_sha:        "deadbeef".to_owned(),
    requester_pid: std::process::id(),
  };
  let payload = serde_json::to_vec(&request).expect("encode request");
  {
    let mut stdin = child.stdin.take().expect("child stdin");
    write_frame(&mut stdin, &payload).expect("write request frame");
  }

  let reply_bytes = {
    let mut stdout = child.stdout.take().expect("child stdout");
    read_frame(&mut stdout).expect("read reply frame")
  };

  let output = child.wait_with_output().expect("ctl subcommand exits");
  assert!(
    output.status.success(),
    "ctl subcommand exited with {:?}; stderr: {}",
    output.status,
    String::from_utf8_lossy(&output.stderr)
  );

  let reply: AdvanceReply =
    serde_json::from_slice(&reply_bytes).expect("decode reply frame");
  assert_eq!(reply, AdvanceReply::Busy);

  drop(permit);
  server.end_tick();
  server.shutdown();
}
