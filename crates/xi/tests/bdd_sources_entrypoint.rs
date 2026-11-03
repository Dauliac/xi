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

  fn recorded_head(&self) -> Option<String> {
    std::fs::read_to_string(self.home.join(".local/state/xi/sources-head")).ok()
  }

  fn git_head(&self) -> String {
    let out = Command::new("git")
      .arg("-C")
      .arg(self.sources_dir())
      .args(["rev-parse", "HEAD"])
      .output()
      .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
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

  /// The `xi home sources status` envelope, as JSON on stdout.
  fn status(&self) -> String {
    let out = Command::new(xi())
      .args(["home", "sources", "status"])
      .env("XI_NO_CHECKS", "1")
      .env("HOME", &self.home)
      .env("XDG_DATA_HOME", self.home.join(".local/share"))
      .env("XDG_STATE_HOME", self.home.join(".local/state"))
      .env("XDG_CONFIG_HOME", self.home.join(".config"))
      .env_remove("XI_SOURCES_REMOTE")
      .current_dir(self.home.join("proj"))
      .output()
      .expect("spawn xi");
    String::from_utf8_lossy(&out.stdout).into_owned()
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

  let stdout = m.status();

  assert!(
    stdout.contains(&remote),
    "status should name the adopted remote, got: {stdout}"
  );
  assert!(
    !stdout.contains("\"canonical_url\": \"\""),
    "identity must not be empty after adoption: {stdout}"
  );
}

/// The status a freshly adopted source reports, decided on bead xi-4p3h.
///
/// The fixture flake has no `homeConfigurations`, so this switch clones
/// and then fails to build  -  which is precisely the window the bead
/// asks about. xi-sources-model maps "sources dir exists but
/// `active_generation` is unknown (never switched)" to `Pending`, and
/// adoption must not claim a generation it did not produce. The clone's
/// HEAD *is* recorded, because the model says `sources_head` is unknown
/// only "before any clone".
#[test]
fn status_after_a_fresh_adopt_is_pending_not_ready() {
  let m = Machine::new();
  let remote = fixture_repo(&m.tmp.path().join("upstream"), "adopted");
  m.switch(&[&remote]);

  let stdout = m.status();

  assert!(
    stdout.contains("\"status\": \"pending\""),
    "a clone that has not activated is Pending, got: {stdout}"
  );
  assert!(
    stdout.contains("\"active_generation\": null"),
    "adoption must not claim a generation: {stdout}"
  );
  assert!(
    !stdout.contains("\"sources_head\": null"),
    "adoption must record the clone's HEAD: {stdout}"
  );
  assert_eq!(
    m.recorded_head().as_deref(),
    Some(m.git_head().as_str()),
    "the recorded sources-head must be the clone's real HEAD"
  );
}

// -- Requirement: a switch serialises against other sources actors -------

/// A switch takes the sources lock, which is what serialises it against
/// a daemon tick or an inbound push.
///
/// Asserted on the lock's own on-disk footprint rather than on
/// contention: `crates/xi-core/tests/bdd_sources_race.rs` already proves
/// `flock` excludes and that the refusal names the holder, so what is
/// left to check here is that the *command* reaches for it at all. That
/// is also the half that was missing - the primitive had no callers.
///
/// Contention is deliberately not driven through the binary: an
/// interactive switch has no way to bound its lock wait, so a test that
/// held the lock would block for a full 15-minute lease. See xi-aiw7.
#[test]
fn a_switch_takes_the_sources_lock() {
  let m = Machine::new();
  let remote = fixture_repo(&m.tmp.path().join("upstream"), "locked");

  // The command fails after the clone, because the fixture flake has no
  // `homeConfigurations` to build; the adoption side effects are what
  // matter, as in the scenarios above.
  m.switch(&[&remote]);
  assert!(
    m.recorded_remote().is_some(),
    "adoption must have recorded the remote"
  );

  let state_dir = m.home.join(".local/state/xi");
  assert!(
    state_dir.join("lock").is_file(),
    "the switch must have taken the lock, which creates {}",
    state_dir.join("lock").display()
  );
  assert!(
    state_dir.join("lock.waiters").is_dir(),
    "acquisition registers the caller as a waiter first, so the queue \
     directory must exist too"
  );
  assert_eq!(
    std::fs::read_to_string(state_dir.join("lock"))
      .expect("read the lock file")
      .trim(),
    "",
    "a released lock leaves an empty record, so a later reader reports free \
     rather than naming a process that has exited"
  );
}

// -- Requirement: the cache prestage/unstage/status verbs are reachable ---
// (openspec/changes/xi-sources-cache-prestage, bead xi-d1xr)

/// A flake declaring one cache via the `nixCache.settings` fallback
/// attribute, which is the one well-known path that needs neither a
/// NixOS/home-manager module nor a guessed hostname.
fn fixture_cache_flake(at: &std::path::Path) -> String {
  std::fs::create_dir_all(at).expect("mkdir flake");
  std::fs::write(
    at.join("flake.nix"),
    r#"{
  outputs = _: {
    nixCache.settings = {
      substituters = [ "https://cache.example.org" ];
      trusted-public-keys = [ "cache.example.org-1:QmFzZTY0S2V5VmFsdWVIZXJlRm9yVGVzdGluZz0=" ];
    };
  };
}
"#,
  )
  .expect("write flake");
  format!("path:{}#nixCache.settings", at.display())
}

impl Machine {
  /// Run `xi home sources cache <args...>`, merging stdout and stderr
  /// like [`Machine::switch`].
  fn cache(&self, args: &[&str]) -> (String, bool) {
    let out = Command::new(xi())
      .args(["home", "sources", "cache"])
      .args(args)
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

  fn xi_conf(&self) -> Option<String> {
    std::fs::read_to_string(self.home.join(".config/nix/xi.conf")).ok()
  }
}

/// Every scope verb mounts `sources cache` with all three verbs.
#[test]
fn cache_subcommands_are_reachable_from_every_scope_verb() {
  for verb in ["home", "system", "nixos", "darwin"] {
    let (out, ok) = run(&[verb, "sources", "cache", "--help"]);
    assert!(ok, "`xi {verb} sources cache --help` failed: {out}");
    for sub in ["prestage", "unstage", "status"] {
      assert!(
        out.contains(sub),
        "`xi {verb} sources cache` should expose `{sub}`, got: {out}"
      );
    }
  }
}

/// SC-K-004 half: a machine that was never prestaged reports inactive
/// rather than erroring, under `--json`.
#[test]
fn cache_status_on_an_untouched_machine_is_inactive() {
  let m = Machine::new();
  let (out, ok) = m.cache(&["status", "--json"]);
  assert!(ok, "`cache status` on a clean machine must not fail: {out}");
  assert!(out.contains("\"active\": false"), "got: {out}");
  assert!(out.contains("\"receipt_present\": false"), "got: {out}");
}

/// `cache unstage` with no prior prestage has nothing to reverse and
/// says so, rather than silently succeeding.
#[test]
fn cache_unstage_without_a_prior_prestage_fails() {
  let m = Machine::new();
  let (out, ok) = m.cache(&["unstage"]);
  assert!(!ok, "unstage with no receipt must fail: {out}");
  assert!(out.contains("no cache prestage to reverse"), "got: {out}");
}

/// SC-K-001/002/003/006: the full prestage -> status -> unstage round
/// trip through the binary, exercising the fallback attribute path and
/// the `!include` amend together with the CLI wiring.
#[test]
fn cache_prestage_status_unstage_round_trip() {
  let m = Machine::new();
  let target = fixture_cache_flake(&m.tmp.path().join("cache-flake"));

  let (out, ok) =
    m.cache(&["prestage", &target, "--trust-declared-keys", "--json"]);
  assert!(ok, "`cache prestage` failed: {out}");
  assert!(out.contains("\"status\": \"applied\""), "got: {out}");
  assert!(
    out.contains("cache.example.org"),
    "the envelope should name the discovered substituter: {out}"
  );
  let xi_conf = m.xi_conf().expect("xi.conf written");
  assert!(xi_conf.contains("extra-substituters = https://cache.example.org"));

  let (status_out, status_ok) = m.cache(&["status", "--json"]);
  assert!(status_ok, "`cache status` failed: {status_out}");
  assert!(status_out.contains("\"active\": true"), "got: {status_out}");

  let (unstage_out, unstage_ok) = m.cache(&["unstage", "--json"]);
  assert!(unstage_ok, "`cache unstage` failed: {unstage_out}");
  assert!(
    unstage_out.contains("\"include_removed\": true"),
    "got: {unstage_out}"
  );
  assert!(
    unstage_out.contains("\"xi_conf_removed\": true"),
    "got: {unstage_out}"
  );
  assert!(m.xi_conf().is_none(), "xi.conf must be gone after unstage");

  let (after_out, after_ok) = m.cache(&["status", "--json"]);
  assert!(after_ok, "`cache status` after unstage failed: {after_out}");
  assert!(after_out.contains("\"active\": false"), "got: {after_out}");
}

/// A prestage takes the sources lock, like switch and sync do: it
/// writes `nix.conf` and the state dir, both of which other sources
/// actors also write. Asserted on the lock's on-disk footprint, as
/// `a_switch_takes_the_sources_lock` does above, for the same reason -
/// contention cannot be driven through the binary within a bounded test.
#[test]
fn cache_prestage_takes_the_sources_lock() {
  let m = Machine::new();
  let target = fixture_cache_flake(&m.tmp.path().join("cache-flake"));

  let (out, ok) =
    m.cache(&["prestage", &target, "--trust-declared-keys", "--json"]);
  assert!(ok, "`cache prestage` failed: {out}");

  let state_dir = m.home.join(".local/state/xi");
  assert!(
    state_dir.join("lock").is_file(),
    "the prestage must have taken the lock, which creates {}",
    state_dir.join("lock").display()
  );
  assert_eq!(
    std::fs::read_to_string(state_dir.join("lock"))
      .expect("read the lock file")
      .trim(),
    "",
    "a released lock leaves an empty record"
  );
}
