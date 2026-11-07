//! BDD: `xi deploy --retry 0` disables retrying entirely, against a
//! fixture fleet, matching `xi push --retry 0`'s own existing coverage
//! (`crates/xi-deploy/tests/bdd_push_retry.rs`'s
//! `retry_0_makes_exactly_one_attempt_and_the_failure_surfaces_with_no_retry_log_lines`).
//!
//! `push_args()` (`crates/xi/src/deploy.rs`, xi-6mf6.2.1) already proves
//! `DeployArgs::retry` carries unchanged into `PushArgs::retry`; `xi
//! push --retry 0`'s own coverage already proves that value disables
//! `SshTransport::run`'s retry loop. What is untested is the deploy
//! verb's own flag reaching that same transport unchanged: this file
//! parses `DeployArgs` the way `xi deploy --retry 0` itself would, then
//! resolves and exercises the real `SshTransport` against a scripted
//! `ssh` stand-in exactly as the push-level test does, across a fixture
//! fleet of hosts rather than a single one, proving the flag disables
//! retrying for every target a fleet rollout would touch.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{
  io,
  os::unix::fs::PermissionsExt,
  path::Path,
  sync::{Arc, Mutex},
};

use clap::Parser;
use serial_test::serial;
use xi_deploy::{
  push::{
    config::{resolve_retry_attempts, PushConfig},
    engine::{RemoteCommand, SshTransport, TargetAddr, Transport},
  },
  unified_args::DeployArgs,
};

/// Write an executable at `dir/ssh` that fails (exit `255`, `ssh`'s own
/// "could not establish or maintain the connection" code) for its first
/// `fail_count` invocations, tracked in a sibling counter file, then
/// succeeds on every call after - identical to
/// `bdd_push_retry.rs::write_scripted_ssh`.
fn write_scripted_ssh(dir: &Path, fail_count: u32) {
  let count_file = dir.join("attempts");
  let script = format!(
    "#!/bin/sh\n\
     count=$(cat '{count}' 2>/dev/null || echo 0)\n\
     count=$((count + 1))\n\
     echo \"$count\" > '{count}'\n\
     if [ \"$count\" -le {fail_count} ]; then\n\
     \texit 255\n\
     fi\n\
     exit 0\n",
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
  let original_path = std::env::var("PATH").unwrap_or_default();
  // SAFETY: serialized via #[serial] - no other thread observes PATH
  // while this closure runs.
  unsafe {
    std::env::set_var(
      "PATH",
      format!("{}:{original_path}", dir.display()),
    );
  }
  let result = f();
  unsafe {
    std::env::set_var("PATH", original_path);
  }
  result
}

/// A `tracing` writer that appends every formatted event into a shared
/// buffer, so a test can assert on exactly which log lines fired -
/// identical to `bdd_push_retry.rs::CaptureWriter`.
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

fn target(name: &str) -> TargetAddr {
  TargetAddr {
    name:     name.to_owned(),
    ssh_host: name.to_owned(),
    ssh_opts: Vec::new(),
  }
}

/// Flattens `DeployArgs` behind the real `clap` parser, the same shape
/// `xi deploy` itself parses through, so `--retry 0` resolves the way
/// an operator would actually see it.
#[derive(Debug, Parser)]
struct DeployArgsHarness {
  #[command(flatten)]
  args: DeployArgs,
}

/// A fixture fleet: every host `xi deploy --retry 0` would roll out to,
/// each proven independently to receive exactly one attempt.
const FIXTURE_FLEET: [&str; 2] = ["web1", "web2"];

#[test]
#[serial]
fn deploy_retry_0_makes_exactly_one_attempt_per_fleet_host_with_no_retry_log_lines()
 {
  let parsed = DeployArgsHarness::try_parse_from(["xi", "--retry", "0"])
    .expect("`xi deploy --retry 0` parses")
    .args;
  assert_eq!(
    parsed.retry,
    Some(0),
    "the deploy verb's own --retry flag parses to 0"
  );

  for host in FIXTURE_FLEET {
    let dir = tempfile::tempdir().expect("tempdir");
    // Would eventually succeed if retried - this proves `--retry 0`
    // really disables retrying rather than happening to run out of
    // attempts at the same number a default policy would.
    write_scripted_ssh(dir.path(), 10);
    let (logs, _guard) = capture_logs();

    let resolved = resolve_retry_attempts(parsed.retry, host, &PushConfig::default());
    assert_eq!(
      resolved, 0,
      "{host}: the deploy verb's CLI flag ladder resolves --retry 0 to 0"
    );

    let outcome = with_scripted_ssh_on_path(dir.path(), || {
      let transport = SshTransport;
      transport.run(&target(host), &RemoteCommand::elevated("true", false, resolved))
    })
    .expect("run does not error on a remote exit, only on a local spawn failure");

    assert!(
      !outcome.success,
      "{host}: today's single-attempt behavior: the first failure is final"
    );
    assert_eq!(
      attempt_count(dir.path()),
      1,
      "{host}: exactly one attempt is made"
    );
    assert_eq!(
      logs.retry_log_line_count(),
      0,
      "{host}: no retry was scheduled, so no retry log line fires: got {:?}",
      logs.contents()
    );
  }
}
