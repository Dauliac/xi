//! BDD: `xi deploy --reattach` dispatches straight to
//! `push::reattach::run`, bypassing fact-gathering and the decision
//! tree entirely, from
//! `openspec/changes/xi-deploy-expose-push-flags/specs/
//! xi-deploy-expose-push-flags/spec.md`'s requirement that the
//! local-vs-fleet decision tree must not run at all for a reattach
//! invocation (xi-6mf6.2.2).
//!
//! Drives the built `xi` binary against a user-scope fixture host
//! adopted the same way `bdd_deprecated_verbs.rs`'s
//! `home_switch_still_adopts_and_builds_and_prints_the_notice` adopts
//! one: cloned from an upstream flake with no `homeConfigurations`, so
//! the fixture is adopted, not dirty, with no inventory and no
//! flake-deploy outputs - exactly the facts `xi_deploy::decision::resolve`
//! maps to `Decision::ActivateLocal(LocalReason::NothingToFanOutTo)`.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::{path::PathBuf, process::Command};

/// Path to the binary under test, as cargo built it for this run.
fn xi() -> PathBuf {
  let mut p = std::env::current_exe().expect("test exe path");
  p.pop(); // deps/
  p.pop(); // debug/
  p.push("xi");
  p
}

/// Run `xi` with `args` from `cwd`, against `home`'s sandboxed
/// `HOME`/`XDG_*`, merging stdout and stderr like
/// `bdd_deprecated_verbs.rs`'s own `run_in` helper.
fn run_in(home: &std::path::Path, args: &[&str]) -> (String, bool) {
  let out = Command::new(xi())
    .args(args)
    .env("XI_NO_CHECKS", "1")
    .env("HOME", home)
    .env("XDG_DATA_HOME", home.join(".local/share"))
    .env("XDG_STATE_HOME", home.join(".local/state"))
    .env("XDG_CONFIG_HOME", home.join(".config"))
    .current_dir(home)
    .output()
    .expect("spawn xi");
  let merged = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  (merged, out.status.success())
}

/// Scenario: `xi deploy --reattach` lists receipts without ever
/// letting the decision tree run, even against a fixture host it
/// would otherwise resolve to a local activation for.
#[test]
fn deploy_reattach_lists_receipts_without_running_the_decision_tree() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let home = tmp.path().join("home");
  std::fs::create_dir_all(&home).expect("mkdir home");
  let upstream = tmp.path().join("upstream");
  std::fs::create_dir_all(&upstream).expect("mkdir upstream");
  std::fs::write(upstream.join("flake.nix"), "{ outputs = _: { }; }\n")
    .expect("write flake");
  let git = |args: &[&str]| {
    Command::new("git")
      .arg("-C")
      .arg(&upstream)
      .args(args)
      .env("GIT_AUTHOR_NAME", "xi-reattach-dispatch-test")
      .env(
        "GIT_AUTHOR_EMAIL",
        "xi-reattach-dispatch-test@example.invalid",
      )
      .env("GIT_COMMITTER_NAME", "xi-reattach-dispatch-test")
      .env(
        "GIT_COMMITTER_EMAIL",
        "xi-reattach-dispatch-test@example.invalid",
      )
      .status()
      .expect("git invocation")
  };
  assert!(git(&["init", "-q", "-b", "main"]).success());
  assert!(git(&["add", "-A"]).success());
  assert!(git(&["commit", "-q", "-m", "upstream one"]).success());

  // Adopt the fixture the same way `xi deploy --scope user --adopt`
  // does: a
  // real clone, no inventory, no flake-deploy outputs - the decision
  // tree's own `NothingToFanOutTo` case. The build against the
  // fixture's empty flake fails (no `homeConfigurations`), proving
  // this invocation's dispatch really did reach the local-activation
  // build step rather than being rejected by clap.
  let (adopt_out, adopt_ok) = run_in(&home, &[
    "deploy",
    "--scope",
    "user",
    "--adopt",
    upstream.to_str().expect("utf8"),
  ]);
  assert!(
    !adopt_ok,
    "adopting against a flake with no homeConfigurations must fail to build: \
     {adopt_out}"
  );
  assert!(
    home.join(".local/share/xi/sources/.git").is_dir(),
    "`xi deploy --adopt` must still have cloned the adopted remote before the \
     build failed: {adopt_out}"
  );

  // Same adopted fixture, no inventory, no flake-deploy outputs - if
  // `--reattach` funneled through `gather_facts`/`resolve_plan` like
  // any other invocation, it would resolve to the exact same
  // `ActivateLocal` local build that just failed above, and fail the
  // same way. Instead it must dispatch straight to
  // `push::reattach::run` and list receipts.
  let (reattach_out, reattach_ok) =
    run_in(&home, &["deploy", "--scope", "user", "--reattach"]);

  assert!(
    reattach_ok,
    "`xi deploy --reattach` against an adopted-but-otherwise-local fixture \
     must succeed, not fall through to the local build: {reattach_out}"
  );
  assert!(
    reattach_out.contains("no in-flight push receipts"),
    "`xi deploy --reattach` with no receipts recorded must list none, got: \
     {reattach_out}"
  );
}
