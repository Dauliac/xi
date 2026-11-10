//! BDD: the six superseded verbs (`os switch`, `home switch`, `darwin
//! switch`, `system switch`, `push`, `deploy-legacy`) are gone from the
//! CLI past the deprecation window, from
//! `openspec/changes/xi-deploy-unified-verb/specs/xi-deploy-unified-verb/spec.
//! md`'s "The five superseded verbs deprecate before removal" requirement
//! (xi-fxgx.5.4): each now exits non-zero as an unrecognized subcommand
//! instead of dispatching with a deprecation notice.
//!
//! `os`/`home`/`darwin`/`system` stay as scope verbs (`build`, `status`,
//! `sources`, `uninstall` are untouched) - only their `switch` child is
//! gone. `xi deploy` itself keeps working: it is the one verb all six
//! are superseded by, and internally still constructs and runs the
//! exact same `Switch` variant in Rust (bypassing CLI parsing entirely),
//! which these tests must not break.
//!
//! These drive the built `xi` binary, because what is under test is
//! clap's own parsing behavior (`--help`'s rendered command list, and
//! whether a removed token still parses at all).

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

/// Run `xi` with `args` from `cwd`, merging stdout and stderr.
fn run_in(cwd: &std::path::Path, args: &[&str]) -> (String, bool) {
  let mut cmd = Command::new(xi());
  cmd.args(args).env("XI_NO_CHECKS", "1").current_dir(cwd);
  let out = cmd.output().expect("spawn xi");
  let merged = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  (merged, out.status.success())
}

// -- Requirement: the six superseded verbs are gone, not just hidden -----

/// Scenario: each scope's `switch` child no longer parses at all.
#[test]
fn scope_switches_are_unrecognized_subcommands() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  for verb in ["os", "home", "darwin", "system"] {
    let (out, ok) = run_in(tmp.path(), &[verb, "switch"]);

    assert!(!ok, "`xi {verb} switch` must fail: {out}");
    assert!(
      out.contains("unrecognized subcommand"),
      "`xi {verb} switch` must be rejected by clap as unrecognized, not \
       dispatch anywhere, got: {out}"
    );
  }
}

/// Scenario: the standalone `push` verb no longer parses at all.
#[test]
fn push_is_an_unrecognized_subcommand() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let (out, ok) = run_in(tmp.path(), &["push"]);

  assert!(!ok, "`xi push` must fail: {out}");
  assert!(
    out.contains("unrecognized subcommand"),
    "`xi push` must be rejected by clap as unrecognized, got: {out}"
  );
}

/// Scenario: the legacy `deploy-legacy` verb no longer parses at all.
#[test]
fn deploy_legacy_is_an_unrecognized_subcommand() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let (out, ok) = run_in(tmp.path(), &["deploy-legacy"]);

  assert!(!ok, "`xi deploy-legacy` must fail: {out}");
  assert!(
    out.contains("unrecognized subcommand"),
    "`xi deploy-legacy` must be rejected by clap as unrecognized, got: {out}"
  );
}

/// Scenario: `xi deploy` itself is unaffected by any of the above -
/// still the one surviving verb, still fully functional.
#[test]
fn deploy_itself_still_works() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let (out, ok) = run_in(tmp.path(), &["deploy", "--help"]);
  assert!(ok, "`xi deploy --help` must still succeed: {out}");
  assert!(
    out.contains("deploy"),
    "expected deploy's own help text, got: {out}"
  );
}

// -- Requirement: the removed verbs disappear from `--help` --------------

/// Scenario: `push` and `deploy-legacy` are gone from the top-level
/// command list; `deploy` is the only verb left under "Deployment".
#[test]
fn push_and_deploy_legacy_are_gone_from_top_level_help() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let (out, ok) = run_in(tmp.path(), &["--help"]);
  assert!(ok, "`xi --help` must succeed: {out}");

  let commands_start = out.find("Commands:").expect("a Commands: section");
  let options_start = out[commands_start..]
    .find("Options:")
    .map(|rel| commands_start + rel)
    .expect("an Options: section");
  let commands_section = &out[commands_start..options_start];

  for still_listed in ["os", "home", "darwin", "system", "deploy"] {
    assert!(
      commands_section
        .lines()
        .any(|l| l.trim_start().starts_with(still_listed)),
      "`{still_listed}` must still be listed in `--help`, \
       got:\n{commands_section}"
    );
  }
  for gone in ["push", "deploy-legacy"] {
    assert!(
      !commands_section
        .lines()
        .any(|l| l.trim_start().starts_with(gone)),
      "`{gone}` must not be listed in `--help` any more, \
       got:\n{commands_section}"
    );
  }
}

/// Scenario: `switch` disappears from each scope verb's own help, while
/// its siblings (`build`, `status`, `sources`) stay.
#[test]
fn switch_is_gone_from_every_scope_verbs_help() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  for verb in ["os", "home", "darwin", "system"] {
    let (out, ok) = run_in(tmp.path(), &[verb, "--help"]);
    assert!(ok, "`xi {verb} --help` must succeed: {out}");

    let commands_start = out.find("Commands:").expect("a Commands: section");
    let commands_section = &out[commands_start..];

    assert!(
      !commands_section
        .lines()
        .any(|l| l.trim_start().starts_with("switch")),
      "`xi {verb} --help` must not list `switch` any more, \
       got:\n{commands_section}"
    );
    assert!(
      commands_section
        .lines()
        .any(|l| l.trim_start().starts_with("build")),
      "`xi {verb} --help` must still list its other subcommands, \
       got:\n{commands_section}"
    );
  }
}
