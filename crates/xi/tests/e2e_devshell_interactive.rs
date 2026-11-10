//! BDD: openspec/changes/devshell-e2e-harness
//!   specs/xi-devshell-test-harness/spec.md#Interactive end-to-end layer
//!
//! The full devshell lifecycle through a real interactive shell on a real
//! pseudo-terminal: activation, entry on cd, nesting, cd-out, explicit exit
//! with suppression, re-entry, and re-eval on flake change.
//!
//! Gated behind `XI_E2E_PTY=1`. These tests need a pseudo-terminal, which is
//! not available in every sandbox, and they are slower than the rest of the
//! suite. The fast layers must stay runnable everywhere, so this layer opts
//! in rather than out:
//!
//! ```text
//! XI_E2E_PTY=1 cargo test -p xi --test e2e_devshell_interactive
//! ```
//!
//! Per the layer-placement rule, a behaviour belongs here only if it needs a
//! prompt hook to fire, a process exit status to cross a parent/child shell
//! boundary, or environment isolation between two live shells. Everything
//! else is tested in the cheaper layers.

mod harness;

use harness::{FlakeFixture, NixResponse, Shell, TestWorld, pty::PtyShell};

/// Skip unless explicitly enabled, reporting why.
fn pty_enabled() -> bool {
  if std::env::var("XI_E2E_PTY").as_deref() == Ok("1") {
    return true;
  }
  eprintln!(
    "SKIP: set XI_E2E_PTY=1 to run the interactive pseudo-terminal layer"
  );
  false
}

/// The activation hook exactly as a user installs it, resolved to the binary
/// under test rather than whatever `xi` is on PATH.
fn activation_for(world: &TestWorld, shell: Shell) -> String {
  let out = world.xi(&["activate", shell.name()], world.flake_root());
  assert!(
    out.status.success(),
    "xi develop activate {} failed: {}",
    shell.name(),
    String::from_utf8_lossy(&out.stderr)
  );
  let script = String::from_utf8_lossy(&out.stdout).into_owned();
  assert!(
    script.contains("develop prompt"),
    "activation script must install the prompt hook, got:\n{script}"
  );
  // The generated script resolves __XI_BIN itself via `command -v xi`, which
  // finds whatever xi is installed on this machine rather than the build under
  // test. The pin MUST come AFTER the script: putting it first lets the
  // script's own assignment overwrite it, which silently runs the installed
  // xi. That failure mode is not subtle in its effects (the installed build
  // has no XI_RUNTIME_DIR support, so it escapes the world entirely) but it is
  // completely silent in its cause, so the ordering here is load-bearing.
  format!(
    "{}\nexport __XI_BIN='{}'\n",
    script,
    env!("CARGO_BIN_EXE_xi")
  )
}

/// BDD: xi-devshell-test-harness#Activation to entry
///
/// The baseline: with the real hook installed in an rc file, cd'ing into a
/// trusted flake must enter a devshell with the staged environment applied.
#[test]
fn activation_enters_devshell_on_cd() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder()
    .flake(FlakeFixture::WithDevShell)
    .nix(&[NixResponse::new().var("XI_STAGE", "one")])
    .trusted(true)
    .build();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };

  sh.run(&format!("cd '{}'", world.flake_root().display()));

  assert!(
    sh.in_devshell(),
    "cd into a trusted flake must enter a devshell.\n--- transcript ---\n{}",
    sh.transcript()
  );
  assert_eq!(
    sh.env_var("__XI_FLAKE_ROOT").map(|p| {
      std::fs::canonicalize(p)
        .unwrap_or_default()
        .display()
        .to_string()
    }),
    Some(
      std::fs::canonicalize(world.flake_root())
        .unwrap()
        .display()
        .to_string()
    ),
    "__XI_FLAKE_ROOT must point at the flake root"
  );
  assert_eq!(sh.depth(), 1, "first level must be depth 1");
}

/// BDD: xi-devshell-test-harness#Activation to entry
///
/// The staged nix environment must actually reach the live shell. Entering a
/// devshell that applies nothing is the failure this catches.
#[test]
fn staged_environment_reaches_the_live_shell() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder()
    .nix(&[NixResponse::new().var("XI_STAGE", "alpha")])
    .trusted(true)
    .build();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };
  sh.run(&format!("cd '{}'", world.flake_root().display()));

  assert_eq!(
    sh.env_var("XI_STAGE").as_deref(),
    Some("alpha"),
    "the staged devshell variable must be exported in the subshell.\n--- \
     transcript ---\n{}",
    sh.transcript()
  );
}

#[test]
/// BDD: xi-devshell-test-harness#Explicit exit cascades to the parent
///
/// The exit-code convention in `subshell.rs` is explicit: status 200 means
/// "left the flake directory" and the parent survives; ANY other status means
/// the user explicitly exited, and the parent re-exits with that same status so
/// the cascade reaches the top-level terminal.
///
/// This test first asserted the opposite, that the parent survives and records
/// `__XI_SUPPRESS`. That was the older contract, which the implementation
/// deliberately replaced: async activation is transparent on the way in, so an
/// explicit exit means "close this terminal", not "pop one invisible layer".
/// Nothing writes `__XI_SUPPRESS` any more; `prompt.rs` only reads and clears
/// it.
fn explicit_exit_cascades_and_closes_the_terminal() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder().trusted(true).build();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };
  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert!(sh.in_devshell(), "precondition: must be inside a devshell");

  // No marker and no prompt can follow: the shell is going away.
  sh.send_raw("exit");

  let code = sh.wait_for_exit(std::time::Duration::from_secs(20));
  assert!(
    code.is_some(),
    "an explicit exit must cascade and terminate the top-level shell, but it \
     is still running.\n--- transcript ---\n{}",
    sh.transcript()
  );
}

#[test]
/// BDD: xi-devshell-test-harness#Re-entry after leaving and returning
///
/// Uses the cd-out path rather than `exit`: cd-out is the only exit that
/// leaves the parent alive, so it is the only one that can be followed by a
/// re-entry in the same shell.
fn reentry_after_leaving_and_returning() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder().trusted(true).build();
  let outside = world.root().join("outside");
  std::fs::create_dir_all(&outside).unwrap();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };

  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert!(sh.in_devshell(), "precondition: entered on the first cd");

  // Leaving via cd-out keeps the parent alive.
  sh.run(&format!("cd '{}'", outside.display()));
  assert!(
    !sh.in_devshell(),
    "leaving the flake root must leave the devshell.\n--- transcript ---\n{}",
    sh.transcript()
  );

  // Returning enters again, in the same still-living shell.
  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert!(
    sh.in_devshell(),
    "returning to the flake root must enter again.\n--- transcript ---\n{}",
    sh.transcript()
  );
}

/// BDD: xi-devshell-test-harness#Nesting isolates and restores
///
/// Entering a nested flake must reach depth 2; leaving it must restore the
/// depth-1 environment exactly and leave the depth-1 shell alive. This is
/// the behaviour that only exists between two live processes.
#[test]
fn nesting_isolates_then_restores_parent_environment() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder()
    .flake(FlakeFixture::Nested)
    .nix(&[NixResponse::new().var("XI_LEVEL", "outer")])
    .trusted(true)
    .build();

  // The nested flake needs its own trust marker, keyed by its own path.
  let nested = world.nested_flake_root();
  let canonical = std::fs::canonicalize(&nested).unwrap();
  let trusted_dir = world.root().join("c/xi/develop/trusted");
  std::fs::create_dir_all(&trusted_dir).unwrap();
  std::fs::write(trusted_dir.join(xi_develop::dirs::flake_id(&canonical)), "")
    .unwrap();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };

  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert_eq!(sh.depth(), 1, "outer level must be depth 1");
  let outer_level = sh.env_var("XI_LEVEL");

  sh.run(&format!("cd '{}'", nested.display()));
  assert_eq!(
    sh.depth(),
    2,
    "entering a nested flake must increment depth.\n--- transcript ---\n{}",
    sh.transcript()
  );

  // Pop exactly one level. `exit` would cascade all the way out, so cd-out is
  // the only way to observe the parent level being restored.
  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert_eq!(
    sh.depth(),
    1,
    "leaving the nested level via cd-out must restore depth 1.\n--- \
     transcript ---\n{}",
    sh.transcript()
  );
  assert_eq!(
    sh.env_var("XI_LEVEL"),
    outer_level,
    "the depth-1 environment must be restored exactly"
  );
  assert!(sh.in_devshell(), "the depth-1 shell must still be alive");
}

/// BDD: xi-devshell-test-harness#cd-out follows the user
///
/// Leaving the flake root from inside the devshell must exit with the
/// sentinel and leave the parent in the directory the user cd'd to, not the
/// one the parent occupied before spawning.
#[test]
fn cd_out_leaves_parent_where_the_user_went() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder().trusted(true).build();
  let elsewhere = world.root().join("elsewhere");
  std::fs::create_dir_all(&elsewhere).unwrap();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };

  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert!(sh.in_devshell(), "precondition: inside the devshell");

  sh.run(&format!("cd '{}'", elsewhere.display()));

  assert!(
    !sh.in_devshell(),
    "leaving the flake root must leave the devshell.\n--- transcript ---\n{}",
    sh.transcript()
  );

  let landed = std::fs::canonicalize(sh.pwd()).unwrap_or_default();
  assert_eq!(
    landed,
    std::fs::canonicalize(&elsewhere).unwrap(),
    "the parent must follow the user out, not snap back to the pre-entry \
     directory. This is the cd-out path that the '>|' force-write \
     protects.\n--- transcript ---\n{}",
    sh.transcript()
  );
}

/// BDD: xi-devshell-test-harness#Flake change triggers re-eval in a live shell
///
/// With a second staged response differing from the first, touching
/// `flake.nix` must cause exactly one re-source, observed by polling the env
/// generation counter rather than by sleeping.
#[test]
fn flake_change_triggers_single_resource_in_live_shell() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder()
    .nix(&[
      NixResponse::new().var("XI_GEN", "first"),
      NixResponse::new()
        .var("XI_GEN", "second")
        .paths(&[harness::store_path("ripgrep", "14.1.0")]),
    ])
    .trusted(true)
    .build();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };
  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert!(sh.in_devshell(), "precondition: inside the devshell");

  let before = world.env_generation();

  // Touch the watched file, then wait on an observable, never on a clock.
  std::fs::write(
    world.flake_root().join("flake.nix"),
    "{ outputs = { self, ... }: { devShells.x86_64-linux.default = { }; }; }\n",
  )
  .unwrap();

  world.wait_for(
    "env generation to advance after a flake change",
    harness::DEFAULT_DEADLINE,
    |w| w.env_generation() > before,
  );

  let after = world.env_generation();
  assert_eq!(
    after,
    before + 1,
    "a single flake change must produce exactly one re-source, not {}",
    after - before
  );
}

/// BDD: xi-devshell-test-harness#Staged failure drives the degraded path
///
/// A failing eval must not destroy a working environment: the previously
/// generated env and hook files must still be present and sourceable.
#[test]
fn eval_failure_preserves_previous_environment() {
  if !pty_enabled() {
    return;
  }
  let world = TestWorld::builder()
    .nix(&[
      NixResponse::new().var("XI_GEN", "good"),
      NixResponse::new().failing("error: attribute 'devShells' missing\n"),
    ])
    .trusted(true)
    .build();

  let rc = activation_for(&world, Shell::Bash);
  let Some(mut sh) = PtyShell::spawn(&world, Shell::Bash, &rc) else {
    return;
  };
  sh.run(&format!("cd '{}'", world.flake_root().display()));
  assert!(sh.in_devshell(), "precondition: inside the devshell");

  let state_dir = world.state_dir();
  let before: Vec<_> = std::fs::read_dir(&state_dir)
    .map(|rd| rd.filter_map(Result::ok).map(|e| e.file_name()).collect())
    .unwrap_or_default();
  assert!(
    !before.is_empty(),
    "precondition: the first eval must have produced state in {}",
    state_dir.display()
  );

  std::fs::write(
    world.flake_root().join("flake.nix"),
    "{ outputs = { self, ... }: { devShells.x86_64-linux.default = { }; }; }\n",
  )
  .unwrap();

  world.wait_for(
    "the failing eval to be attempted",
    harness::DEFAULT_DEADLINE,
    |w| w.nix_eval_count() >= 2,
  );

  for name in &before {
    assert!(
      state_dir.join(name).exists(),
      "a failed eval must not remove previously good state: {:?} vanished",
      name
    );
  }
  assert!(
    sh.in_devshell(),
    "a failed eval must not eject the user from a working devshell"
  );
}

/// BDD: xi-devshell-test-harness#Concurrent worlds do not interact
///
/// Two worlds, two fixtures, driven at the same time. Each must drive its own
/// daemon and neither may observe the other's state. This is what the
/// env-as-data rule buys.
#[test]
fn concurrent_worlds_do_not_interact() {
  if !pty_enabled() {
    return;
  }

  let a = TestWorld::builder()
    .nix(&[NixResponse::new().var("XI_WORLD", "a")])
    .trusted(true)
    .build();
  let b = TestWorld::builder()
    .nix(&[NixResponse::new().var("XI_WORLD", "b")])
    .trusted(true)
    .build();

  assert_ne!(a.flake_id(), b.flake_id(), "worlds must differ by flake id");
  assert_ne!(a.socket_path(), b.socket_path(), "sockets must differ");

  let rc_a = activation_for(&a, Shell::Bash);
  let rc_b = activation_for(&b, Shell::Bash);

  let Some(mut sh_a) = PtyShell::spawn(&a, Shell::Bash, &rc_a) else {
    return;
  };
  let Some(mut sh_b) = PtyShell::spawn(&b, Shell::Bash, &rc_b) else {
    return;
  };

  sh_a.run(&format!("cd '{}'", a.flake_root().display()));
  sh_b.run(&format!("cd '{}'", b.flake_root().display()));

  assert_eq!(
    sh_a.env_var("XI_WORLD").as_deref(),
    Some("a"),
    "world A must see its own staged environment"
  );
  assert_eq!(
    sh_b.env_var("XI_WORLD").as_deref(),
    Some("b"),
    "world B must see its own staged environment"
  );

  a.assert_no_leaks();
  b.assert_no_leaks();
}
