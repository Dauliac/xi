//! BDD: the five superseded verbs (`os switch`, `home switch`, `darwin
//! switch`, `system switch`, `push`) and the legacy `deploy` still work
//! during the deprecation window, from
//! `openspec/changes/xi-deploy-unified-verb/specs/xi-deploy-unified-verb/spec.
//! md`'s "The five superseded verbs deprecate before removal" requirement
//! (xi-fxgx.5.3).
//!
//! These drive the built `xi` binary, because what is under test spans
//! two layers only the binary ties together: `--help`'s rendered
//! command list (clap's own `#[command(hide = true)]`), and the
//! deprecation notice `XiCommand::run` prints before dispatching
//! (`crates/xi/src/interface.rs::deprecated_verb_notice`).
//!
//! Every scenario below avoids ever touching a real system path:
//! `os`/`darwin`/`system switch` all resolve to `Scope::System`
//! (`/etc/xi`, `/var/lib/xi`, fixed - no env override), so each is
//! driven with no positional and no `--adopt`, which the sources model
//! resolves without ever recording or cloning anything
//! (`xi_core::sources::switch::prepare`'s case 3). `push`'s default
//! scope is the same `Scope::System`, so it is driven with `--scope
//! user` instead, which *is* sandboxable via `$HOME`/`$XDG_*`. Only
//! `home switch` (`Scope::User`) exercises a real adopt-and-clone, the
//! same fixture shape `bdd_sources_entrypoint.rs` already uses.

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

/// A fixture dir with a flake declaring no outputs at all, so any
/// mechanism's build step fails deterministically on a missing
/// attribute rather than succeeding or hanging.
fn empty_flake_dir() -> tempfile::TempDir {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  std::fs::write(tmp.path().join("flake.nix"), "{ outputs = _: { }; }\n")
    .expect("write flake.nix");
  tmp
}

/// Run `xi` with `args` from `cwd`, merging stdout and stderr like
/// `bdd_sources_entrypoint.rs`'s `run` helper - the notice goes to
/// stderr, the build/eval error may land on either depending on the
/// mechanism.
fn run_in(
  cwd: &std::path::Path,
  args: &[&str],
  extra_env: &[(&str, &str)],
) -> (String, bool) {
  let mut cmd = Command::new(xi());
  cmd.args(args).env("XI_NO_CHECKS", "1").current_dir(cwd);
  for (key, value) in extra_env {
    cmd.env(key, value);
  }
  let out = cmd.output().expect("spawn xi");
  let merged = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  (merged, out.status.success())
}

/// The exact wording `deprecated_verb_notice` prints for `old`.
fn notice(old: &str) -> String {
  format!(
    "xi: `{old}` is deprecated and will be removed; use `xi deploy` instead"
  )
}

// -- Requirement: the five superseded verbs deprecate before removal -----

/// Scenario: A deprecated verb still works, with a notice - the four
/// scope switches.
///
/// Each of `os`/`darwin`/`system` resolves to `Scope::System`, whose
/// `sources_dir`/`config_file` are fixed real paths
/// (`xi_core::sources::config::ScopePaths::resolve`), not overridable
/// via env - so this drives each with no positional and no `--adopt`.
/// With nothing recorded under the (overridden) state dir,
/// `switch::prepare`'s case 3 hands the installable back untouched and
/// takes no lock and clones nothing; the mechanism then tries to build
/// its own attribute from the fixture's empty flake and fails on a
/// missing-attribute error, which is reached only if dispatch actually
/// ran the real `switch::prepare` + build pipeline rather than being
/// rejected by clap.
#[test]
fn scope_switches_still_dispatch_and_print_the_notice() {
  for verb in ["os", "darwin", "system"] {
    let fixture = empty_flake_dir();
    let state_dir = fixture.path().join("state");
    let (out, ok) = run_in(fixture.path(), &[verb, "switch"], &[(
      "XI_SOURCES_STATE_DIR",
      state_dir.to_str().expect("utf8"),
    )]);

    assert!(
      !ok,
      "`xi {verb} switch` against an empty flake must fail: {out}"
    );
    assert!(
      out.contains(&notice(&format!("{verb} switch"))),
      "`xi {verb} switch` must print the deprecation notice, got: {out}"
    );
    assert!(
      !out.contains("unrecognized subcommand")
        && !out.contains("unexpected argument"),
      "`xi {verb} switch` must still be a real subcommand, not CLI-rejected: \
       {out}"
    );
    assert!(
      !state_dir.join("remote-url").is_file(),
      "`xi {verb} switch` with no positional and nothing recorded must not \
       have adopted anything"
    );
  }
}

/// Scenario: A deprecated verb still works, with a notice - `home
/// switch`, the one scope sandboxable enough to drive a real adopt.
///
/// Mirrors `bdd_sources_entrypoint.rs`'s
/// `adopt_on_a_fresh_machine_clones_and_records`: the fixture flake has
/// no `homeConfigurations`, so the build is expected to fail, but
/// adoption (the clone, the recorded remote) must have happened first -
/// proof the deprecated verb still activates, not just prints a notice
/// and stops.
#[test]
fn home_switch_still_adopts_and_builds_and_prints_the_notice() {
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
      .env("GIT_AUTHOR_NAME", "xi-deprecation-test")
      .env("GIT_AUTHOR_EMAIL", "xi-deprecation-test@example.invalid")
      .env("GIT_COMMITTER_NAME", "xi-deprecation-test")
      .env("GIT_COMMITTER_EMAIL", "xi-deprecation-test@example.invalid")
      .status()
      .expect("git invocation")
  };
  assert!(git(&["init", "-q", "-b", "main"]).success());
  assert!(git(&["add", "-A"]).success());
  assert!(git(&["commit", "-q", "-m", "upstream one"]).success());

  let (out, _ok) = run_in(
    &home,
    &[
      "home",
      "switch",
      "--adopt",
      upstream.to_str().expect("utf8"),
    ],
    &[
      ("HOME", home.to_str().expect("utf8")),
      (
        "XDG_DATA_HOME",
        home.join(".local/share").to_str().expect("utf8"),
      ),
      (
        "XDG_STATE_HOME",
        home.join(".local/state").to_str().expect("utf8"),
      ),
      (
        "XDG_CONFIG_HOME",
        home.join(".config").to_str().expect("utf8"),
      ),
    ],
  );

  assert!(
    home.join(".local/share/xi/sources/.git").is_dir(),
    "`xi home switch` must still have cloned the adopted remote: {out}"
  );
  assert!(
    out.contains(&notice("home switch")),
    "`xi home switch` must print the deprecation notice, got: {out}"
  );
}

/// Scenario: A deprecated verb still works, with a notice - `push`.
///
/// `push`'s default scope is also `Scope::System`; `--scope user`
/// switches it to the one sandboxable scope. With nothing adopted
/// there, `push::orchestrate::run` reaches `context::resolve_context`
/// and refuses naming the user scope by name - proof dispatch reached
/// the real orchestrator, not a CLI-level rejection.
#[test]
fn push_still_dispatches_and_prints_the_notice() {
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let home = tmp.path().join("home");
  std::fs::create_dir_all(&home).expect("mkdir home");

  let (out, ok) = run_in(tmp.path(), &["push", "--scope", "user"], &[
    ("HOME", home.to_str().expect("utf8")),
    (
      "XDG_DATA_HOME",
      home.join(".local/share").to_str().expect("utf8"),
    ),
    (
      "XDG_STATE_HOME",
      home.join(".local/state").to_str().expect("utf8"),
    ),
    (
      "XDG_CONFIG_HOME",
      home.join(".config").to_str().expect("utf8"),
    ),
  ]);

  assert!(!ok, "`xi push` with nothing adopted must fail: {out}");
  assert!(
    out.contains(&notice("push")),
    "`xi push` must print the deprecation notice, got: {out}"
  );
  assert!(
    out.contains("no sources configured for the user scope"),
    "the refusal must come from the real orchestrator, not a CLI rejection: \
     {out}"
  );
}

/// Scenario: A deprecated verb still works, with a notice - the legacy
/// `deploy-legacy`.
///
/// Against the fixture's empty flake, the builtin backend's own
/// discovery fails cleanly on "no nixosConfigurations" - proof
/// dispatch reached `detect_and_deploy`, not a CLI-level rejection.
#[test]
fn deploy_legacy_still_dispatches_and_prints_the_notice() {
  let fixture = empty_flake_dir();

  let (out, ok) = run_in(fixture.path(), &["deploy-legacy"], &[]);

  assert!(
    !ok,
    "`xi deploy-legacy` against an empty flake must fail: {out}"
  );
  assert!(
    out.contains(&notice("deploy-legacy")),
    "`xi deploy-legacy` must print the deprecation notice, got: {out}"
  );
  assert!(
    out.contains("No nixosConfigurations found"),
    "the failure must come from the real backend, not a CLI rejection: {out}"
  );
}

// -- Requirement: the deprecated verbs disappear from `--help` -----------

/// Scenario: `push` and the legacy `deploy` are gone from the top-level
/// command list.
///
/// `os`/`home`/`darwin`/`system` stay listed - only their nested
/// `switch` subcommand is deprecated, not the whole verb.
#[test]
fn push_and_deploy_legacy_are_hidden_from_top_level_help() {
  let fixture = empty_flake_dir();
  let (out, ok) = run_in(fixture.path(), &["--help"], &[]);
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
  for hidden in ["push", "deploy-legacy"] {
    assert!(
      !commands_section
        .lines()
        .any(|l| l.trim_start().starts_with(hidden)),
      "`{hidden}` must not be listed in `--help` any more, \
       got:\n{commands_section}"
    );
  }
}

/// Scenario: `switch` disappears from each scope verb's own help, while
/// its siblings stay.
#[test]
fn switch_is_hidden_from_every_scope_verbs_help() {
  let fixture = empty_flake_dir();
  for verb in ["os", "home", "darwin", "system"] {
    let (out, ok) = run_in(fixture.path(), &[verb, "--help"], &[]);
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
