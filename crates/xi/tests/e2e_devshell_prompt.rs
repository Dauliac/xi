//! BDD: openspec/changes/devshell-e2e-harness
//!   specs/xi-devshell-test-harness/spec.md#Script-contract layer
//!
//! The script-contract layer: invoke `xi develop prompt` as a child process
//! inside a hermetic world and assert on the shell text it emits, without
//! executing that text. No shell is involved, nothing is evaluated, and no
//! real nix runs.
//!
//! This is the cheapest layer that can observe trust gating, flake detection,
//! the no-devshell skip, suppression, and parent-versus-subshell dispatch, so
//! per the layer-placement rule those behaviours are tested here and nowhere
//! slower.

mod harness;

use harness::{FlakeFixture, NixResponse, Shell, TestWorld};

/// BDD: xi-devshell-test-harness#Untrusted flake emits no spawn command
#[test]
fn untrusted_flake_emits_no_spawn_command() {
  let world = TestWorld::builder()
    .flake(FlakeFixture::WithDevShell)
    .trusted(false)
    .build();

  let out = world.prompt(Shell::Bash, world.flake_root());

  assert!(
    !out.spawns_subshell(),
    "an untrusted flake must not spawn a subshell, got stdout:\n{}",
    out.stdout
  );
  assert!(
    out.stderr.contains("trust"),
    "the trust hint must go to stderr, got:\n{}",
    out.stderr
  );
  world.assert_no_leaks();
}

/// BDD: xi-devshell-test-harness#Untrusted flake emits no spawn command
///
/// An untrusted flake must not reach nix at all: evaluating before the trust
/// decision would execute flake code the user has not approved.
#[test]
fn untrusted_flake_does_not_invoke_nix() {
  let world = TestWorld::builder()
    .flake(FlakeFixture::WithDevShell)
    .trusted(false)
    .build();

  world.prompt(Shell::Bash, world.flake_root());

  assert_eq!(
    world.nix_eval_count(),
    0,
    "untrusted flake must not evaluate: {:?}",
    world.nix_evals()
  );
}

/// BDD: xi-devshell-test-harness#Flake without a devshell is skipped silently
#[test]
fn flake_without_devshell_is_skipped_silently() {
  let world = TestWorld::builder()
    .flake(FlakeFixture::NoDevShell)
    .trusted(true)
    .build();

  let out = world.prompt(Shell::Bash, world.flake_root());

  assert!(
    out.is_silent(),
    "a flake with no devshell must emit nothing, got:\n{}",
    out.stdout
  );
  assert!(
    !out.stderr.to_lowercase().contains("trust"),
    "must not nag about trust for a flake with no devshell, got:\n{}",
    out.stderr
  );
}

/// BDD: xi-devshell-test-harness#Flake without a devshell is skipped silently
///
/// Regression shape: a dendritic flake whose own `flake.nix` text has no
/// devshell keyword, with the devshell defined in an imported module. This
/// false-negatived before and silently never activated.
#[test]
fn devshell_in_imported_module_is_detected() {
  let world = TestWorld::builder()
    .flake(FlakeFixture::DevShellInModule)
    .trusted(true)
    .build();

  let out = world.prompt(Shell::Bash, world.flake_root());

  assert!(
    out.spawns_subshell(),
    "a devshell in an imported module must still activate, got \
     stdout:\n{}\nstderr:\n{}",
    out.stdout,
    out.stderr
  );
}

/// BDD: xi-devshell-test-harness#Suppress is honoured then cleared
#[test]
fn suppress_is_honoured_beneath_the_root() {
  let world = TestWorld::builder().trusted(true).build();

  let mut cmd = world.command(std::path::Path::new(env!("CARGO_BIN_EXE_xi")));
  cmd
    .arg("develop")
    .arg("prompt")
    .arg("-s")
    .arg("bash")
    .arg("--pid")
    .arg("424242")
    .env("__XI_SUPPRESS", world.flake_root())
    .current_dir(world.flake_root());

  let out = cmd.output().expect("spawn xi");
  let stdout = String::from_utf8_lossy(&out.stdout);

  assert!(
    stdout.trim().is_empty(),
    "suppression must silence the hook while cwd is beneath the root, \
     got:\n{stdout}"
  );
}

/// BDD: xi-devshell-test-harness#Suppress is honoured then cleared
#[test]
fn suppress_is_cleared_once_cwd_moves_away() {
  let world = TestWorld::builder().trusted(true).build();
  let outside = world.root().join("outside");
  std::fs::create_dir_all(&outside).unwrap();

  let mut cmd = world.command(std::path::Path::new(env!("CARGO_BIN_EXE_xi")));
  cmd
    .arg("develop")
    .arg("prompt")
    .arg("-s")
    .arg("bash")
    .arg("--pid")
    .arg("424242")
    .env("__XI_SUPPRESS", world.flake_root())
    .current_dir(&outside);

  let out = cmd.output().expect("spawn xi");
  let stdout = String::from_utf8_lossy(&out.stdout);

  assert!(
    stdout.contains("__XI_SUPPRESS"),
    "leaving the suppressed root must emit the unset, got:\n{stdout}"
  );
  assert!(
    stdout.contains("unset"),
    "the clear must be an unset for bash, got:\n{stdout}"
  );
}

/// BDD: xi-devshell-test-harness#Subshell dispatch is keyed on the in-devshell
/// marker
#[test]
fn in_devshell_marker_suppresses_nested_spawn() {
  let world = TestWorld::builder().trusted(true).build();

  let mut cmd = world.command(std::path::Path::new(env!("CARGO_BIN_EXE_xi")));
  cmd
    .arg("develop")
    .arg("prompt")
    .arg("-s")
    .arg("bash")
    .arg("--pid")
    .arg("424242")
    .env("__XI_IN_DEVSHELL", "1")
    .env("__XI_FLAKE_ROOT", world.flake_root())
    .env("__XI_DEPTH", "1")
    .current_dir(world.flake_root());

  let out = cmd.output().expect("spawn xi");
  let stdout = String::from_utf8_lossy(&out.stdout);

  assert!(
    !stdout.contains("__XI_IN_DEVSHELL=1"),
    "already inside a devshell must not spawn another one (recursion), \
     got:\n{stdout}"
  );
}

/// BDD: xi-devshell-test-harness#Suite runs with no nix present
///
/// The whole layer must be reachable without nix. If this test can run, the
/// fake-nix seam is doing its job.
#[test]
fn no_flake_found_is_silent() {
  let world = TestWorld::builder().trusted(true).build();
  let bare = world.root().join("bare");
  std::fs::create_dir_all(&bare).unwrap();

  let out = world.prompt(Shell::Bash, &bare);

  assert!(
    out.is_silent(),
    "no flake anywhere must emit nothing, got:\n{}",
    out.stdout
  );
  assert_eq!(world.nix_eval_count(), 0, "no flake must not evaluate");
}

/// BDD: xi-devshell-test-harness#Eval arguments are pinned
///
/// Pins the eval contract that nothing else asserts: the installable, the
/// --json flag, both --option pairs, a --profile path, and the working
/// directory. A refactor that drops `connect-timeout` would otherwise make
/// every devshell hang on a slow substituter with no test noticing.
#[test]
fn eval_arguments_are_pinned() {
  let world = TestWorld::builder()
    .flake(FlakeFixture::WithDevShell)
    .nix(&[NixResponse::new()])
    .trusted(true)
    .build();

  // `exec` takes the synchronous eval path, so one call is enough to pin the
  // argument vector without starting a daemon.
  let out = world.xi(&["exec", "--", "true"], world.flake_root());

  let calls = world.nix_evals();
  assert!(
    !calls.is_empty(),
    "expected at least one nix invocation; xi exec said:\nstdout: {}\nstderr: \
     {}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );

  let call = &calls[0];
  assert!(call.has_arg("print-dev-env"), "args: {:?}", call.args);
  assert!(call.has_arg("--json"), "args: {:?}", call.args);
  assert!(
    call.has_option("connect-timeout", "5"),
    "connect-timeout must be pinned, args: {:?}",
    call.args
  );
  assert!(
    call.has_option("download-attempts", "2"),
    "download-attempts must be pinned, args: {:?}",
    call.args
  );
  assert!(
    call.arg_after("--profile").is_some(),
    "a --profile path must be passed so the closure is GC-rooted, args: {:?}",
    call.args
  );

  let canonical = std::fs::canonicalize(world.flake_root()).unwrap();
  assert_eq!(
    std::fs::canonicalize(&call.cwd).unwrap(),
    canonical,
    "nix must be invoked with the flake root as cwd"
  );
}

/// BDD: xi-devshell-test-harness#Profile symlink is created
#[test]
fn profile_symlink_is_created_by_the_fake() {
  let world = TestWorld::builder().trusted(true).build();

  world.xi(&["exec", "--", "true"], world.flake_root());

  let calls = world.nix_evals();
  let profile = calls
    .iter()
    .find_map(|c| c.arg_after("--profile"))
    .expect("a --profile path should have been passed");

  let p = std::path::Path::new(profile);
  assert!(
    p.symlink_metadata().is_ok(),
    "fake nix must create the profile symlink so GC-root and dix paths run \
     against real input: {profile}"
  );
}
