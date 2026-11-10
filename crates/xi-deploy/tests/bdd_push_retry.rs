//! BDD: `SshTransport::run`'s retry wiring
//! (`openspec/specs/xi-sources-retry/spec.md`, SC-RT-001..004).
//!
//! `SshTransport::run`'s streaming (activation) path wraps its `ssh` spawn
//! in `xi_core::retry::retry` with `classify_ssh_exit` (see
//! `crates/xi-deploy/src/push/engine.rs`). That wrapping sits *below* the
//! `Transport` trait - a fake `Transport` used elsewhere in this crate's
//! BDD suite (`FixtureTransport` et al.) answers a call directly and never
//! exercises the retry loop at all. Testing the real wiring therefore
//! means exercising the real `SshTransport` against a scripted `ssh`
//! stand-in: a tiny shell script, placed first on `PATH`, that counts its
//! own invocations and answers transient failures (exit `255`) for a
//! scripted number of attempts before succeeding - the "scripted
//! transport" this file's scenarios are named after.
//!
//! Every scenario here asserts the *outcome* is unaffected by whether a
//! retry happened - the whole point of this capability is that a push
//! that needed a retry looks exactly like one that didn't, modulo the
//! `debug`-level "transient failure; retrying" log line
//! `xi_core::retry::retry` emits once per scheduled retry.
//!
//! These tests mutate the process-wide `PATH` to install the scripted
//! `ssh`, so they run `#[serial]` against each other (nothing else in
//! this crate's test suite spawns a real `ssh`, so they do not need to
//! serialize against anything else).

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{
  io,
  os::unix::fs::PermissionsExt,
  path::Path,
  sync::{Arc, Mutex},
};

use serial_test::serial;
use xi_deploy::push::{
  config::{PushConfig, resolve_retry_attempts},
  engine::{RemoteCommand, SshTransport, TargetAddr, Transport},
};

/// Write an executable at `dir/ssh` that fails (exit `255`, `ssh`'s own
/// "could not establish or maintain the connection" code) for its first
/// `fail_count` invocations, tracked in a sibling counter file, then
/// succeeds on every call after.
fn write_scripted_ssh(dir: &Path, fail_count: u32) {
  let count_file = dir.join("attempts");
  let script = format!(
    "#!/bin/sh\ncount=$(cat '{count}' 2>/dev/null || echo 0)\ncount=$((count \
     + 1))\necho \"$count\" > '{count}'\nif [ \"$count\" -le {fail_count} ]; \
     then\n\texit 255\nfi\nexit 0\n",
    count = count_file.display(),
  );
  let ssh_path = dir.join("ssh");
  std::fs::write(&ssh_path, script).expect("write scripted ssh");
  std::fs::set_permissions(&ssh_path, std::fs::Permissions::from_mode(0o755))
    .expect("chmod scripted ssh");
}

fn attempt_count(dir: &Path) -> u32 {
  std::fs::read_to_string(dir.join("attempts"))
    .expect("attempts file")
    .trim()
    .parse()
    .expect("attempts file holds a number")
}

/// Prepend `dir` to `PATH` for the duration of `f`, restoring it after -
/// `std::env::set_var` is process-wide, which is why every caller of this
/// helper is `#[serial]`.
fn with_scripted_ssh_on_path<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
  with_path(
    &format!(
      "{}:{}",
      dir.display(),
      std::env::var("PATH").unwrap_or_default()
    ),
    f,
  )
}

/// Replace `PATH` outright (no fallback to the real one) for the
/// duration of `f` - used to simulate `ssh` being entirely absent, since
/// merely prepending an empty directory would still resolve the real
/// `ssh` further down the original `PATH`.
fn with_no_ssh_on_path<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
  with_path(&dir.display().to_string(), f)
}

fn with_path<R>(scoped_path: &str, f: impl FnOnce() -> R) -> R {
  let original_path = std::env::var("PATH").unwrap_or_default();
  // SAFETY: serialized via #[serial] - no other thread observes PATH
  // while this closure runs.
  unsafe {
    std::env::set_var("PATH", scoped_path);
  }
  let result = f();
  unsafe {
    std::env::set_var("PATH", original_path);
  }
  result
}

/// A `tracing` writer that appends every formatted event into a shared
/// buffer, so a test can assert on exactly which log lines fired.
#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.0.lock().expect("capture lock").extend_from_slice(buf);
    Ok(buf.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl CaptureWriter {
  fn contents(&self) -> String {
    String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
  }

  fn retry_log_line_count(&self) -> usize {
    self
      .contents()
      .lines()
      .filter(|line| line.contains("transient failure; retrying"))
      .count()
  }
}

/// Install a capturing `tracing` subscriber as the default for the
/// duration of the guard it returns.
fn capture_logs() -> (CaptureWriter, tracing::subscriber::DefaultGuard) {
  let writer = CaptureWriter::default();
  let make_writer = {
    let writer = writer.clone();
    move || writer.clone()
  };
  let subscriber = tracing_subscriber::fmt()
    .with_max_level(tracing::Level::DEBUG)
    .with_writer(make_writer)
    .with_ansi(false)
    .finish();
  let guard = tracing::subscriber::set_default(subscriber);
  (writer, guard)
}

fn target() -> TargetAddr {
  TargetAddr {
    name:     "web1".to_owned(),
    ssh_host: "web1".to_owned(),
    ssh_opts: Vec::new(),
  }
}

#[test]
#[serial]
fn a_connection_that_fails_twice_then_succeeds_completes_with_only_retry_log_lines_as_the_difference()
 {
  let dir = tempfile::tempdir().expect("tempdir");
  write_scripted_ssh(dir.path(), 2);
  let (logs, _guard) = capture_logs();

  let outcome = with_scripted_ssh_on_path(dir.path(), || {
    let transport = SshTransport;
    transport.run(&target(), &RemoteCommand::elevated("true", false, 3))
  })
  .expect("eventually succeeds");

  assert!(outcome.success, "the push must still complete");
  assert_eq!(
    attempt_count(dir.path()),
    3,
    "two failed attempts, then a third that succeeds"
  );
  assert_eq!(
    logs.retry_log_line_count(),
    2,
    "one retry log line per scheduled retry: got {:?}",
    logs.contents()
  );
}

#[test]
#[serial]
fn retry_0_makes_exactly_one_attempt_and_the_failure_surfaces_with_no_retry_log_lines()
 {
  let dir = tempfile::tempdir().expect("tempdir");
  // Would eventually succeed if retried - this proves `--retry 0` really
  // disables retrying rather than happening to run out of attempts at
  // the same number a default policy would.
  write_scripted_ssh(dir.path(), 10);
  let (logs, _guard) = capture_logs();

  let resolved =
    resolve_retry_attempts(Some(0), "web1", &PushConfig::default());
  assert_eq!(resolved, 0, "the CLI flag ladder resolves --retry 0 to 0");

  let outcome = with_scripted_ssh_on_path(dir.path(), || {
    let transport = SshTransport;
    transport.run(&target(), &RemoteCommand::elevated("true", false, resolved))
  })
  .expect("run does not error on a remote exit, only on a local spawn failure");

  assert!(
    !outcome.success,
    "today's single-attempt behavior: the first failure is final"
  );
  assert_eq!(attempt_count(dir.path()), 1, "exactly one attempt is made");
  assert_eq!(
    logs.retry_log_line_count(),
    0,
    "no retry was scheduled, so no retry log line fires: got {:?}",
    logs.contents()
  );
}

#[test]
#[serial]
fn with_no_retry_flag_configured_up_to_the_compiled_floor_of_3_attempts_are_made()
 {
  let dir = tempfile::tempdir().expect("tempdir");
  write_scripted_ssh(dir.path(), 10); // never succeeds within 3 attempts
  let (logs, _guard) = capture_logs();

  let resolved = resolve_retry_attempts(None, "web1", &PushConfig::default());
  assert_eq!(resolved, 3, "the compiled floor with no rung configured");

  let outcome = with_scripted_ssh_on_path(dir.path(), || {
    let transport = SshTransport;
    transport.run(&target(), &RemoteCommand::elevated("true", false, resolved))
  })
  .expect("a remote-visible failure is an Outcome, not an Err");

  assert!(!outcome.success, "every attempt failed");
  assert_eq!(
    attempt_count(dir.path()),
    3,
    "up to 3 attempts, then giving up"
  );
  assert_eq!(
    logs.retry_log_line_count(),
    2,
    "a retry line after attempt 1 and after attempt 2, none after the last: \
     got {:?}",
    logs.contents()
  );
}

#[test]
#[serial]
fn a_local_spawn_failure_is_retried_the_same_number_of_times_as_a_255_exit() {
  // An empty PATH entry with no `ssh` binary at all: the local spawn
  // itself fails every attempt, which `classify_ssh_exit` treats as
  // transient exactly like a 255 exit.
  let dir = tempfile::tempdir().expect("tempdir");
  let (logs, _guard) = capture_logs();

  let err = with_no_ssh_on_path(dir.path(), || {
    let transport = SshTransport;
    transport.run(&target(), &RemoteCommand::elevated("true", false, 3))
  })
  .expect_err("ssh could not be spawned at all");

  assert!(
    format!("{err}").contains("gave up after 3 attempts"),
    "got: {err}"
  );
  assert_eq!(
    logs.retry_log_line_count(),
    2,
    "retried the same number of times as a 255 exit would be: got {:?}",
    logs.contents()
  );
}

#[test]
#[serial]
fn a_non_retryable_activation_command_is_not_retried_on_a_simulated_255() {
  // xi-3i2g: an activation/switch command's ssh exit 255 can mean the
  // connection dropped *after* activation already started, not only that
  // it never connected - retrying would re-invoke the whole command (and,
  // for `mode_sources::switch`'s systemd-run-wrapped equivalent, spin up a
  // second concurrent activation). `RemoteCommand::non_retryable` must make
  // `SshTransport::run` skip `xi_core::retry::retry` entirely, running
  // exactly once regardless of `retry_attempts`.
  let dir = tempfile::tempdir().expect("tempdir");
  // Would succeed on the second attempt if this were retried at all.
  write_scripted_ssh(dir.path(), 1);
  let (logs, _guard) = capture_logs();

  let outcome = with_scripted_ssh_on_path(dir.path(), || {
    let transport = SshTransport;
    transport.run(
      &target(),
      &RemoteCommand::elevated("true", false, 3).non_retryable(),
    )
  })
  .expect("run does not error on a remote exit, only on a local spawn failure");

  assert!(
    !outcome.success,
    "the first (and only) attempt's failure is final"
  );
  assert_eq!(
    attempt_count(dir.path()),
    1,
    "a non-retryable activation command makes exactly one attempt regardless \
     of retry_attempts"
  );
  assert_eq!(
    logs.retry_log_line_count(),
    0,
    "a non-retryable command never calls into xi_core::retry::retry at all: \
     got {:?}",
    logs.contents()
  );
}

#[test]
#[serial]
fn a_probe_never_retries_even_against_a_transiently_failing_host() {
  let dir = tempfile::tempdir().expect("tempdir");
  // Would succeed on a later attempt if the probe path retried at all.
  write_scripted_ssh(dir.path(), 10);
  let (logs, _guard) = capture_logs();

  let outcome = with_scripted_ssh_on_path(dir.path(), || {
    let transport = SshTransport;
    transport.run(&target(), &RemoteCommand::probe("true"))
  })
  .expect("a probe's own ssh invocation still spawns fine");

  assert!(!outcome.success, "the probe answers honestly, not retried");
  assert_eq!(
    attempt_count(dir.path()),
    1,
    "a probe makes exactly one attempt regardless of retry_attempts"
  );
  assert_eq!(
    logs.retry_log_line_count(),
    0,
    "the probe path never calls into xi_core::retry::retry at all"
  );
}
