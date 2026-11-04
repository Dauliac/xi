//! BDD: contract for the `xi-sources-gitops` capability
//! (openspec/changes/xi-sources-gitops/specs/xi-sources-gitops/spec.md).
//!
//! One test per SC-G-* criterion this crate currently implements, named
//! after it, so a failure here names exactly one broken requirement.
//! Drives a real `tick::run` against real git fixtures in a temp dir -
//! the criteria are about what lands on disk (the sources dir's tree,
//! the `sources-head` state file), which a fake git cannot show.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{fs, path::Path, process::Command};

use tempfile::TempDir;
use xi_core::sources::{
  Scope,
  SourcesId,
  config::SyncMode,
  conflict::PostAdvance,
};
use xi_sources_daemon::{
  exec::recorder::RecordingExec,
  health::TickOutcome,
  sentinel::Sentinel,
  tick::{self, TickInputs},
};

fn git(at: &Path, args: &[&str]) {
  let status = Command::new("git")
    .env("GIT_TERMINAL_PROMPT", "0")
    .arg("-C")
    .arg(at)
    .args(args)
    .status()
    .expect("git should run");
  assert!(status.success(), "git {args:?} failed");
}

fn head(at: &Path) -> String {
  let output = Command::new("git")
    .arg("-C")
    .arg(at)
    .args(["rev-parse", "HEAD"])
    .output()
    .expect("rev-parse");
  String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(repo: &Path, name: &str, body: &str) {
  fs::write(repo.join(name), body).expect("write");
  git(repo, &["add", "-A"]);
  git(repo, &["commit", "-q", "-m", body]);
}

/// Two fresh clones - a bare-looking "remote" and the daemon-owned
/// "sources dir" - plus the state dir a tick writes to.
struct World {
  _tmp:        TempDir,
  remote:      std::path::PathBuf,
  sources_dir: std::path::PathBuf,
  state_dir:   std::path::PathBuf,
  sources_id:  SourcesId,
}

impl World {
  fn new() -> Self {
    let tmp = TempDir::new().expect("tempdir");
    let remote = tmp.path().join("remote");
    let sources_dir = tmp.path().join("sources");
    let state_dir = tmp.path().join("state");

    fs::create_dir_all(&remote).expect("mkdir");
    git(&remote, &["init", "-q", "-b", "main"]);
    git(&remote, &["config", "user.email", "test@example.com"]);
    git(&remote, &["config", "user.name", "Test"]);
    commit(&remote, "flake.nix", "base");

    fs::create_dir_all(&sources_dir).expect("mkdir");
    git(&sources_dir, &["init", "-q", "-b", "main"]);

    let sources_id =
      SourcesId::new(Scope::System, remote.to_str().expect("utf8 path"));
    Sentinel::new(sources_id.clone())
      .write(&sources_dir)
      .expect("write sentinel");

    Self {
      _tmp: tmp,
      remote,
      sources_dir,
      state_dir,
      sources_id,
    }
  }

  fn inputs(&self) -> TickInputs<'_> {
    TickInputs {
      sources_id:      &self.sources_id,
      sources_dir:     &self.sources_dir,
      state_dir:       &self.state_dir,
      remote_url:      self.remote.to_str().expect("utf8 path"),
      branch:          "main",
      require_signed:  false,
      allowed_signers: None,
      gpg_public_keys: None,
      on_advance:      None,
      sync_mode:       SyncMode::Prompt,
    }
  }
}

/// SC-G-002: "Daemon fetches a remote that has advanced by N commits, all
/// descendants of `sources_head`; `xi sources status` returns `Ready` and
/// the sources dir advances to the new tip."
///
/// This test covers the daemon-side half directly observable from this
/// crate: the sources dir's `HEAD` and working tree land on the new tip,
/// and `sources-head` in the state dir agrees. The `xi sources status`
/// half is `xi-core`'s [`xi_core::sources::SourcesContext::status`],
/// already covered by that crate's own BDD suite reading the same state
/// file this test writes.
#[test]
fn sc_g_002_remote_advance_by_n_commits_fast_forwards_the_sources_dir() {
  let world = World::new();
  let exec = RecordingExec::always_ok();

  // First tick: adopt the base commit, as a freshly bootstrapped daemon
  // would.
  let first = tick::run(&exec, &world.inputs());
  assert!(first.advanced(), "first tick should adopt the base commit");

  // The remote advances by three commits in a row.
  commit(&world.remote, "flake.nix", "advance-1");
  commit(&world.remote, "flake.nix", "advance-2");
  commit(&world.remote, "flake.nix", "advance-3");
  let remote_tip = head(&world.remote);

  let outcome = tick::run(&exec, &world.inputs());

  match &outcome {
    TickOutcome::Advanced {
      from,
      to,
      hook,
      post_advance,
    } => {
      assert_eq!(*to, remote_tip, "sources dir should land on the new tip");
      assert!(
        from.is_some(),
        "an advance from a known head carries `from`"
      );
      assert!(hook.is_none(), "no on_advance was configured");
      assert_eq!(
        *post_advance,
        PostAdvance::Announce,
        "the default sync_mode (prompt) reports Announce"
      );
    },
    other => panic!("expected TickOutcome::Advanced, got {other:?}"),
  }

  assert_eq!(
    head(&world.sources_dir),
    remote_tip,
    "the sources dir's own HEAD must have moved"
  );
  assert_eq!(
    fs::read_to_string(world.sources_dir.join("flake.nix")).expect("read"),
    "advance-3",
    "the working tree must reflect the new tip, not just the ref"
  );

  let recorded = fs::read_to_string(world.state_dir.join("sources-head"))
    .expect("sources-head should be recorded");
  assert_eq!(
    recorded.trim(),
    remote_tip,
    "the recorded sources-head must agree with the sources dir's HEAD"
  );
}

// ---------------------------------------------------------------------------
// xi-1ae1: the daemon binary runs `xi sources repair` before anything else
// ---------------------------------------------------------------------------

/// xi-1ae1: `xi-sources-daemon`'s `main` calls `repair::auto_repair`
/// before anything else, including lock acquisition. Driven against the
/// real compiled binary rather than `tick::run` directly, because the
/// thing under test is specifically what `main` does before the tick
/// loop starts - a dangling shadow fetch left beside the sources dir by
/// a killed predecessor must be gone by the time this process has done
/// anything else.
#[test]
fn the_daemon_binary_sweeps_a_dangling_shadow_fetch_on_startup() {
  // Deliberately not `World`: that fixture is built around `tick::run`
  // taking `sources_dir` as an explicit argument, so its `sources_dir`
  // lives wherever the tempdir happens to put it and need not agree
  // with what `ScopePaths::resolve` would compute. `repair::auto_repair`
  // (what this test exercises) has no such override - it always
  // resolves `ScopePaths::resolve(scope)` itself - so this fixture
  // drives the daemon entirely through the same `XDG_*` /
  // `XI_SOURCES_*` environment a real deployment would set, with no
  // `--sources-dir` flag, so both halves of the binary agree on one
  // path.
  let tmp = TempDir::new().expect("tempdir");
  let root = tmp.path();
  let xdg_data = root.join("data");
  let xdg_config = root.join("config");
  let state_dir = root.join("state");
  let remote = root.join("remote");

  fs::create_dir_all(&remote).expect("mkdir remote");
  git(&remote, &["init", "-q", "-b", "main"]);
  git(&remote, &["config", "user.email", "test@example.com"]);
  git(&remote, &["config", "user.name", "Test"]);
  commit(&remote, "flake.nix", "base");

  // `ScopePaths::resolve_with(Scope::User, ...)` puts the sources dir at
  // `$XDG_DATA_HOME/xi/sources`; cloning it there up front is what
  // satisfies `main`'s "not a clone yet" refusal and gives the tick a
  // real commit to adopt.
  let sources_dir = xdg_data.join("xi").join("sources");
  fs::create_dir_all(&sources_dir).expect("mkdir sources dir");
  let cloned = Command::new("git")
    .args(["clone", "-q"])
    .arg(&remote)
    .arg(&sources_dir)
    .status()
    .expect("run git clone")
    .success();
  assert!(cloned, "seed clone into the sources dir failed");

  // What a kill mid-fetch leaves behind: a populated `.incoming` sibling
  // of the sources dir, from a previous (interrupted) tick.
  let incoming = xi_core::sources::shadow::incoming_path(&sources_dir);
  fs::create_dir_all(incoming.join(".git")).expect("plant dangling shadow");
  fs::write(incoming.join("marker"), b"half-fetched").expect("plant marker");

  let exe = env!("CARGO_BIN_EXE_xi-sources-daemon");
  let output = Command::new(exe)
    .args(["--scope", "user", "--once"])
    .env("XDG_DATA_HOME", &xdg_data)
    .env("XDG_CONFIG_HOME", &xdg_config)
    .env("XI_SOURCES_STATE_DIR", &state_dir)
    .env("XI_SOURCES_REMOTE", remote.to_str().expect("utf8"))
    .env("XI_SOURCES_BRANCH", "main")
    .output()
    .expect("run xi-sources-daemon");

  assert!(
    output.status.success(),
    "daemon --once should exit cleanly: stdout={}\nstderr={}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    !incoming.exists(),
    "startup repair must have swept the dangling shadow fetch before \
     the tick loop ran: stderr={}",
    String::from_utf8_lossy(&output.stderr)
  );
}

/// A second tick after a successful advance, with nothing new on the
/// remote, must be idempotent: no further writes, and the outcome says
/// nothing changed rather than claiming a fresh advance.
#[test]
fn a_tick_with_no_remote_movement_is_idempotent() {
  let world = World::new();
  let exec = RecordingExec::always_ok();

  assert!(tick::run(&exec, &world.inputs()).advanced());
  let settled_head = head(&world.sources_dir);

  let outcome = tick::run(&exec, &world.inputs());
  assert!(
    matches!(outcome, TickOutcome::NoChange { .. }),
    "expected NoChange, got {outcome:?}"
  );
  assert_eq!(
    head(&world.sources_dir),
    settled_head,
    "a no-op tick must not move the sources dir"
  );
}

/// SC-G-003: "Daemon fetches a remote that is NOT a descendant of
/// `sources_head` (force push); the sources dir does NOT advance,
/// `xi sources status` returns `Degraded, reason: remote-force-push`, and
/// the daemon logs the rejected sha."
///
/// This is `tick.rs`'s own
/// `a_force_pushed_remote_is_rejected_and_sources_head_stays_put` unit test
/// re-run at the BDD/integration layer this crate's other SC-G-* tests live
/// at, against the same `World` fixture `sc_g_002` uses, so the two
/// criteria that define "advance only via fast-forward" (sections 3.1/3.2)
/// are exercised side by side from the same harness.
#[test]
fn sc_g_003_a_force_pushed_remote_is_rejected_and_sources_head_stays_put() {
  let world = World::new();
  let exec = RecordingExec::always_ok();

  assert!(tick::run(&exec, &world.inputs()).advanced());
  let held_head = head(&world.sources_dir);
  let held_file =
    fs::read_to_string(world.sources_dir.join("flake.nix")).expect("read");

  // Force-push: rewrite history instead of extending it, so the new
  // remote tip is not a descendant of what the sources dir already has.
  git(&world.remote, &["checkout", "-q", "--orphan", "rewritten"]);
  commit(&world.remote, "flake.nix", "rewritten-history");
  git(&world.remote, &["branch", "-M", "rewritten", "main"]);
  let rejected_tip = head(&world.remote);

  let outcome = tick::run(&exec, &world.inputs());

  match &outcome {
    TickOutcome::Degraded(reason) => {
      assert_eq!(
        reason.token(),
        "remote-force-push",
        "status must read Degraded, reason: remote-force-push"
      );
      let text = reason.to_string();
      assert!(
        text.contains(&rejected_tip),
        "the daemon must log the rejected sha, got: {text}"
      );
    },
    other => panic!("expected Degraded(remote-force-push), got {other:?}"),
  }

  assert_eq!(
    head(&world.sources_dir),
    held_head,
    "sources_head must not advance on a rejected force push"
  );
  assert_eq!(
    fs::read_to_string(world.sources_dir.join("flake.nix")).expect("read"),
    held_file,
    "the working tree must not change on a rejected force push"
  );
}
