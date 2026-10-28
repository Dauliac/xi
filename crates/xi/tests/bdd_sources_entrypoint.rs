//! BDD: the switch entrypoint and CLI topology from
//! `openspec/changes/xi-sources-config/specs/xi-sources-config/spec.md`.
//!
//! These drive the built `xi` binary rather than the library, because
//! the thing under test *is* the command surface: which verbs exist,
//! which flags are rejected, and what the parent verb implies. A
//! library-level test could not catch a group mounted under the wrong
//! command.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{path::PathBuf, process::Command};

/// Path to the binary under test, as cargo built it for this run.
fn xi() -> PathBuf {
  let mut p = std::env::current_exe().expect("test exe path");
  p.pop(); // deps/
  p.pop(); // debug/
  p.push("xi");
  p
}

fn run(args: &[&str]) -> (String, bool) {
  let out = Command::new(xi())
    .args(args)
    // Keep the probe off the real machine's state.
    .env("XI_NO_CHECKS", "1")
    .output()
    .expect("spawn xi");
  let merged = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  (merged, out.status.success())
}

// -- Requirement: `sources` is scoped by its parent verb ------------------

/// Scenario: No top-level sources command.
#[test]
fn there_is_no_top_level_sources_command() {
  let (out, ok) = run(&["sources", "status"]);
  assert!(!ok, "a top-level `xi sources` must not exist");
  assert!(
    out.contains("unrecognized subcommand"),
    "expected clap's unrecognised-subcommand error, got: {out}"
  );
}

/// Every scope verb mounts the group.
#[test]
fn every_scope_verb_mounts_sources() {
  for verb in ["home", "system", "nixos", "darwin"] {
    let (out, ok) = run(&[verb, "sources", "--help"]);
    assert!(ok, "`xi {verb} sources --help` failed: {out}");
    assert!(
      out.contains("status"),
      "`xi {verb} sources` should expose `status`, got: {out}"
    );
  }
}

/// Scenario: Scope cannot be stated twice.
#[test]
fn the_scope_flag_is_rejected() {
  for verb in ["home", "system", "nixos", "darwin"] {
    let (out, ok) = run(&[verb, "sources", "status", "--scope", "user"]);
    assert!(!ok, "`xi {verb} sources status --scope` must be rejected");
    assert!(
      out.contains("unexpected argument"),
      "expected an unknown-flag error for {verb}, got: {out}"
    );
  }
}

// -- Requirement: Switch is the sources entrypoint ------------------------

/// The adoption flags are on every scope's `switch`, since the
/// entrypoint has to behave the same across all four verbs.
#[test]
fn every_switch_carries_the_adoption_flags() {
  for verb in ["home", "system", "nixos", "darwin"] {
    let (out, ok) = run(&[verb, "switch", "--help"]);
    assert!(ok, "`xi {verb} switch --help` failed: {out}");
    for flag in ["--adopt", "--no-adopt", "--force"] {
      assert!(
        out.contains(flag),
        "`xi {verb} switch` should carry {flag}, got: {out}"
      );
    }
  }
}

/// `--adopt` and `--no-adopt` are contradictory and must not both be
/// accepted: silently picking one would decide, on the user's behalf,
/// whether their machine gets repointed.
#[test]
fn adopt_and_no_adopt_conflict() {
  let (out, ok) = run(&["home", "switch", "--adopt", "--no-adopt"]);
  assert!(!ok, "--adopt with --no-adopt must be rejected");
  assert!(
    out.contains("cannot be used with"),
    "expected a clap conflict error, got: {out}"
  );
}

// -- Requirement: the remediation hint names the real entrypoint ----------

/// A `Missing` status must point at the command that actually exists.
/// This regressed once already: the hint still named `xi sources init`
/// after that command was removed.
#[test]
fn missing_status_names_the_switch_entrypoint() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let home = tmp.path();
  let out = Command::new(xi())
    .args(["home", "sources", "status"])
    .env("XI_NO_CHECKS", "1")
    .env("HOME", home)
    .env("XDG_DATA_HOME", home.join(".local/share"))
    .env("XDG_STATE_HOME", home.join(".local/state"))
    .env("XDG_CONFIG_HOME", home.join(".config"))
    .env_remove("XI_SOURCES_REMOTE")
    .current_dir(home)
    .output()
    .expect("spawn xi");
  let stdout = String::from_utf8_lossy(&out.stdout);

  assert!(stdout.contains("\"status\": \"missing\""), "got: {stdout}");
  assert!(
    stdout.contains("switch <remote-url>"),
    "the hint must name the switch entrypoint, got: {stdout}"
  );
  assert!(
    !stdout.contains("sources init"),
    "the hint must not name the removed `xi sources init`, got: {stdout}"
  );
}

// -- Requirement: Switch is the sources entrypoint, adoption half --------

/// A throwaway git repo to adopt.
fn fixture_repo(at: &std::path::Path, label: &str) -> String {
  std::fs::create_dir_all(at).expect("mkdir repo");
  std::fs::write(at.join("flake.nix"), "{ outputs = _: { }; }\n")
    .expect("write flake");
  for args in [vec!["init", "-q", "-b", "main"], vec!["add", "-A"], vec![
    "-c",
    "user.email=t@e",
    "-c",
    "user.name=t",
    "commit",
    "-q",
    "-m",
    label,
  ]] {
    let ok = Command::new("git")
      .arg("-C")
      .arg(at)
      .args(&args)
      .status()
      .expect("run git")
      .success();
    assert!(ok, "git {args:?} failed in {}", at.display());
  }
  at.display().to_string()
}

/// A user-scope machine rooted entirely inside a temp dir.
struct Machine {
  tmp:  tempfile::TempDir,
  home: PathBuf,
}

impl Machine {
  fn new() -> Self {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("proj")).expect("mkdir home");
    Self { tmp, home }
  }

  /// `--adopt` is passed because the fixture remote is a local path,
  /// which the discrimination rule correctly reads as an installable.
  /// The adoption behaviour under test is the same either way.
  fn switch(&self, extra: &[&str]) -> (String, bool) {
    let out = Command::new(xi())
      .args(["home", "switch", "--adopt"])
      .args(extra)
      .env("XI_NO_CHECKS", "1")
      .env("HOME", &self.home)
      .env("XDG_DATA_HOME", self.home.join(".local/share"))
      .env("XDG_STATE_HOME", self.home.join(".local/state"))
      .env("XDG_CONFIG_HOME", self.home.join(".config"))
      .env_remove("XI_SOURCES_REMOTE")
      .current_dir(self.home.join("proj"))
      .output()
      .expect("spawn xi");
    let merged = format!(
      "{}{}",
      String::from_utf8_lossy(&out.stdout),
      String::from_utf8_lossy(&out.stderr)
    );
    (merged, out.status.success())
  }

  fn sources_dir(&self) -> PathBuf {
    self.home.join(".local/share/xi/sources")
  }

  fn recorded_remote(&self) -> Option<String> {
    std::fs::read_to_string(self.home.join(".local/state/xi/remote-url")).ok()
  }

  fn origin(&self) -> String {
    let out = Command::new("git")
      .arg("-C")
      .arg(self.sources_dir())
      .args(["remote", "get-url", "origin"])
      .output()
      .expect("git remote");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
  }

  fn head_subject(&self) -> String {
    let out = Command::new("git")
      .arg("-C")
      .arg(self.sources_dir())
      .args(["log", "-1", "--format=%s"])
      .output()
      .expect("git log");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
  }
}

/// Scenario: Adopt on a fresh machine.
///
/// The build is expected to fail: the fixture flake has no
/// homeConfigurations. What is under test is that adoption happened
/// and that the switch then evaluated the *sources dir*, not the CWD.
#[test]
fn adopt_on_a_fresh_machine_clones_and_records() {
  let m = Machine::new();
  let remote = fixture_repo(&m.tmp.path().join("upstream"), "upstream one");

  let (out, _) = m.switch(&[&remote]);

  assert!(m.sources_dir().join(".git").is_dir(), "no clone: {out}");
  assert_eq!(m.recorded_remote().as_deref(), Some(remote.as_str()));
  assert_eq!(m.head_subject(), "upstream one");
  assert!(
    out.contains("xi/sources"),
    "the switch should evaluate the sources dir, got: {out}"
  );
}

/// Scenario: The same command works on a managed machine.
#[test]
fn re_adopting_the_same_remote_is_idempotent() {
  let m = Machine::new();
  let upstream = m.tmp.path().join("upstream");
  let remote = fixture_repo(&upstream, "first");
  m.switch(&[&remote]);

  // Move the upstream forward, then re-run the identical command.
  std::fs::write(upstream.join("extra"), "x").expect("write");
  for args in [vec!["add", "-A"], vec![
    "-c",
    "user.email=t@e",
    "-c",
    "user.name=t",
    "commit",
    "-q",
    "-m",
    "second",
  ]] {
    Command::new("git")
      .arg("-C")
      .arg(&upstream)
      .args(&args)
      .status()
      .expect("git");
  }

  let (out, _) = m.switch(&[&remote]);

  assert!(
    !out.contains("refusing"),
    "re-adopting the same remote must not be refused: {out}"
  );
  assert_eq!(m.head_subject(), "second", "clone should fast-forward");
  assert_eq!(m.recorded_remote().as_deref(), Some(remote.as_str()));
}

/// Scenario: Switching remotes needs --force.
#[test]
fn a_different_remote_is_refused_without_force() {
  let m = Machine::new();
  let first = fixture_repo(&m.tmp.path().join("one"), "repo one");
  let second = fixture_repo(&m.tmp.path().join("two"), "repo two");
  m.switch(&[&first]);

  let (out, ok) = m.switch(&[&second]);

  assert!(!ok, "re-pointing without --force must fail");
  assert!(out.contains("refusing"), "got: {out}");
  assert!(
    out.contains(&first),
    "the error should name both remotes: {out}"
  );
  assert!(
    out.contains(&second),
    "the error should name both remotes: {out}"
  );
  assert_eq!(
    m.recorded_remote().as_deref(),
    Some(first.as_str()),
    "the refusal must not have changed the recorded remote"
  );
  assert_eq!(m.head_subject(), "repo one", "the clone must be untouched");
}

/// With `--force`, the recorded remote, the clone's origin and HEAD all
/// move together. An earlier version moved only the first of the three,
/// leaving later fetches silently pulling the replaced remote.
#[test]
fn force_repoints_record_origin_and_head_together() {
  let m = Machine::new();
  let first = fixture_repo(&m.tmp.path().join("one"), "repo one");
  let second = fixture_repo(&m.tmp.path().join("two"), "repo two");
  m.switch(&[&first]);

  m.switch(&["--force", &second]);

  assert_eq!(m.recorded_remote().as_deref(), Some(second.as_str()));
  assert_eq!(m.origin(), second, "origin must follow the recorded remote");
  assert_eq!(m.head_subject(), "repo two", "HEAD must follow too");
}

/// After adopting, `status` must name the remote the machine is
/// actually deployed from. Adoption records it in the state dir while
/// `status` reads the config ladder, so without an explicit fallback
/// the two halves silently disagree and the identity comes back empty.
#[test]
fn status_reports_the_adopted_remote_without_any_config_file() {
  let m = Machine::new();
  let remote = fixture_repo(&m.tmp.path().join("upstream"), "adopted");
  m.switch(&[&remote]);

  let out = Command::new(xi())
    .args(["home", "sources", "status"])
    .env("XI_NO_CHECKS", "1")
    .env("HOME", &m.home)
    .env("XDG_DATA_HOME", m.home.join(".local/share"))
    .env("XDG_STATE_HOME", m.home.join(".local/state"))
    .env("XDG_CONFIG_HOME", m.home.join(".config"))
    .env_remove("XI_SOURCES_REMOTE")
    .current_dir(m.home.join("proj"))
    .output()
    .expect("spawn xi");
  let stdout = String::from_utf8_lossy(&out.stdout);

  assert!(
    stdout.contains(&remote),
    "status should name the adopted remote, got: {stdout}"
  );
  assert!(
    !stdout.contains("\"canonical_url\": \"\""),
    "identity must not be empty after adoption: {stdout}"
  );
}
