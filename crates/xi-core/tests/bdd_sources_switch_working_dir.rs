//! BDD: contract for the working-dir mismatch guard from
//! `openspec/changes/xi-deploy-unify-activation/specs/
//! xi-deploy-unify-activation/spec.md` ("An explicit local path is checked
//! against the configured `working_dir`").
//!
//! `switch.rs`'s own unit tests already cover `working_dir_mismatch` and
//! `confirm_working_dir_mismatch` in isolation (xi-jhh4.4.1, xi-jhh4.4.2).
//! What they do not cover is the two functions wired together through the
//! real [`prepare`] entrypoint, resolving a real `ScopePaths` against a
//! real environment the way `xi <scope> switch` actually would. That is
//! this file's job: one test per scenario named in the spec's
//! `--force-working-dir` and no-TTY acceptance criteria.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::path::Path;

use serial_test::serial;
use tempfile::TempDir;
use xi_core::{
  installable::{Installable, InstallableArgs},
  sources::{
    Scope,
    switch::{SwitchSourcesArgs, prepare},
  },
};

/// Saves, clears, and restores the env vars `prepare` resolves
/// `ScopePaths` and the `working_dir` config key against, so the two
/// scenarios below do not race each other or leak into other tests in
/// the same process.
struct EnvGuard {
  saved: [(&'static str, Option<String>); 6],
}

impl EnvGuard {
  fn rooted_at(home: &Path, working_dir: &Path) -> Self {
    let names = [
      "HOME",
      "XDG_DATA_HOME",
      "XDG_STATE_HOME",
      "XDG_CONFIG_HOME",
      "XI_SOURCES_WORKING_DIR",
      "XI_SOURCES_REMOTE",
    ];
    let saved = [
      (names[0], std::env::var(names[0]).ok()),
      (names[1], std::env::var(names[1]).ok()),
      (names[2], std::env::var(names[2]).ok()),
      (names[3], std::env::var(names[3]).ok()),
      (names[4], std::env::var(names[4]).ok()),
      (names[5], std::env::var(names[5]).ok()),
    ];
    unsafe {
      std::env::set_var(names[0], home);
      std::env::set_var(names[1], home.join(".local/share"));
      std::env::set_var(names[2], home.join(".local/state"));
      std::env::set_var(names[3], home.join(".config"));
      std::env::set_var(names[4], working_dir);
      std::env::remove_var(names[5]);
    }
    Self { saved }
  }
}

impl Drop for EnvGuard {
  fn drop(&mut self) {
    unsafe {
      for (name, value) in &self.saved {
        match value {
          Some(v) => std::env::set_var(name, v),
          None => std::env::remove_var(name),
        }
      }
    }
  }
}

/// A fresh user-scope machine with a configured `working_dir` that
/// differs from `positional`, and no recorded remote - so `prepare`'s
/// case 3 is the one that runs.
struct Mismatch {
  _tmp:        TempDir,
  _env:        EnvGuard,
  positional:  std::path::PathBuf,
  working_dir: std::path::PathBuf,
}

impl Mismatch {
  fn new() -> Self {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    let positional = tmp.path().join("experiments-fork");
    let working_dir = tmp.path().join("dotfiles");
    for dir in [&home, &positional, &working_dir] {
      std::fs::create_dir_all(dir).expect("mkdir fixture dir");
    }
    let env = EnvGuard::rooted_at(&home, &working_dir);
    Self {
      _tmp: tmp,
      _env: env,
      positional,
      working_dir,
    }
  }

  fn args(&self, force_working_dir: bool) -> SwitchSourcesArgs {
    SwitchSourcesArgs {
      branch: None,
      adopt: false,
      no_adopt: false,
      force: false,
      force_dirty: false,
      force_working_dir,
    }
  }

  fn positional_installable(&self) -> InstallableArgs {
    InstallableArgs::Specified(Installable::Flake {
      reference: self.positional.display().to_string(),
      attribute: Vec::new(),
    })
  }
}

/// Scenario: `--force-working-dir` skips the prompt.
///
/// GIVEN scope `user`'s configured `working_dir` differs from the
/// positional
/// WHEN `xi user switch <positional> --force-working-dir` runs
/// THEN it proceeds (returns `Ok`) without prompting - a prompt would
/// error out here, since `cargo test` attaches no TTY to stdin
#[test]
#[serial]
fn force_working_dir_proceeds_without_prompting() {
  let fixture = Mismatch::new();

  let result = prepare(
    Scope::User,
    fixture.positional_installable(),
    &fixture.args(true),
  );

  assert!(
    result.is_ok(),
    "--force-working-dir must build without prompting"
  );
}

/// Scenario: a mismatch with no TTY and no `--force-working-dir` refuses.
///
/// GIVEN the same mismatch as above
/// WHEN `xi user switch <positional>` runs with no TTY attached and
/// without `--force-working-dir`
/// THEN it exits non-zero (`prepare` returns `Err`), naming the
/// mismatch and `--force-working-dir`, without building
#[test]
#[serial]
fn no_tty_and_no_force_refuses_naming_the_mismatch_and_the_flag() {
  let fixture = Mismatch::new();

  let result = prepare(
    Scope::User,
    fixture.positional_installable(),
    &fixture.args(false),
  );
  let Err(err) = result else {
    panic!("no TTY and no --force-working-dir must refuse");
  };

  let message = err.to_string();
  let positional = fixture.positional.canonicalize().expect("canon");
  let working_dir = fixture.working_dir.canonicalize().expect("canon");
  assert!(
    message.contains(&positional.display().to_string()),
    "got: {message}"
  );
  assert!(
    message.contains(&working_dir.display().to_string()),
    "got: {message}"
  );
  assert!(message.contains("--force-working-dir"), "got: {message}");
}
