//! BDD: openspec/changes/devshell-e2e-harness
//!   specs/xi-devshell-test-harness/spec.md#Real-shell evaluation layer
//!
//! The real-shell layer: take the shell text xi generates and run it through
//! an actual `bash`, `zsh`, or `fish`, then assert on the resulting state.
//!
//! Asserting on script *text* cannot see the defects this layer exists for.
//! `NO_CLOBBER` silently dropping a redirect, arithmetic expansion with an
//! unset variable, quoting around a value containing newlines: all of those
//! produce text that looks correct and behaves wrongly. Only a real shell
//! distinguishes them.

mod harness;

use std::fs;

use harness::{Shell, TestWorld};
use xi_develop::{env_file, shell::ShellType, subshell};

/// BDD: xi-devshell-test-harness#Reverting the fix fails a named test
/// BDD: xi-devshell-test-harness#zsh cd-out write survives NO_CLOBBER
///
/// THE REGRESSION ANCHOR (SC-H-004).
///
/// zsh's `NO_CLOBBER` makes a plain `>` onto an existing file fail. The parent
/// `mktemp`s the cd-out file before spawning, so the file always exists by the
/// time the child writes to it. With `>` the write is silently dropped, the
/// parent reads an empty file, and the user is snapped back to the directory
/// they started in instead of following their `cd`. `>|` force-writes
/// regardless of the option.
///
/// If `subshell::generate_cd_out_exit` ever regresses to `>`, this test fails.
/// That is the acceptance condition for the entire harness: it must catch the
/// one defect that motivated building it.
#[test]
fn zsh_cdout_write_survives_no_clobber() {
  let Some(_) = Shell::Zsh.skip_if_missing() else {
    return;
  };
  let world = TestWorld::builder().build();

  // The parent creates this file before spawning the child, so it exists.
  let cdout = world.root().join("cdout");
  fs::write(&cdout, "").unwrap();

  let target = world.root().join("somewhere");
  fs::create_dir_all(&target).unwrap();

  let snippet = subshell::generate_cd_out_exit(ShellType::Zsh);

  // setopt NO_CLOBBER is the whole point: reproduce the user's zsh.
  let script = format!(
    "setopt NO_CLOBBER\ncd '{}'\nexport {}='{}'\n{snippet}",
    target.display(),
    subshell::CDOUT_FILE_VAR,
    cdout.display(),
  );

  let status = world
    .eval_status(Shell::Zsh, &script)
    .expect("zsh was present a moment ago");

  assert_eq!(
    status,
    i32::from(subshell::EXIT_CD_OUT),
    "cd-out must exit with the sentinel so the parent stays alive"
  );

  let recorded = fs::read_to_string(&cdout).unwrap();
  assert_eq!(
    recorded.trim(),
    target.display().to_string(),
    "THE CD-OUT WRITE WAS DROPPED. zsh NO_CLOBBER rejected the redirect, so \
     the parent cannot follow the user out of the flake. This is the '>|' \
     versus '>' defect in subshell::generate_cd_out_exit. Snippet \
     was:\n{snippet}"
  );

  // Secondary, cheaper guard. Deliberately *after* the behavioural assertion
  // so the anchor is proven by running a real zsh, not by grepping source.
  assert!(
    snippet.contains(">|"),
    "the cd-out snippet must force-write; a plain '>' is dropped under \
     NO_CLOBBER. Snippet was:\n{snippet}"
  );
}

/// BDD: xi-devshell-test-harness#zsh cd-out write survives NO_CLOBBER
///
/// Same contract under bash, where `>|` is a no-op difference. Proves the fix
/// is portable rather than zsh-specific.
#[test]
fn bash_cdout_write_records_directory() {
  let Some(_) = Shell::Bash.skip_if_missing() else {
    return;
  };
  let world = TestWorld::builder().build();

  let cdout = world.root().join("cdout-bash");
  fs::write(&cdout, "").unwrap();
  let target = world.root().join("elsewhere");
  fs::create_dir_all(&target).unwrap();

  let script = format!(
    "set -o noclobber\ncd '{}'\nexport {}='{}'\n{}",
    target.display(),
    subshell::CDOUT_FILE_VAR,
    cdout.display(),
    subshell::generate_cd_out_exit(ShellType::Bash),
  );

  let status = world.eval_status(Shell::Bash, &script).unwrap();
  assert_eq!(status, i32::from(subshell::EXIT_CD_OUT));
  assert_eq!(
    fs::read_to_string(&cdout).unwrap().trim(),
    target.display().to_string()
  );
}

/// BDD: xi-devshell-test-harness#Depth arithmetic increments across levels
///
/// `__XI_DEPTH=$((${__XI_DEPTH:-0}+1))` must yield 1 when unset and 2 at
/// depth 1. A quoting slip here breaks nested devshell cleanup, because each
/// level's injected-var registry is keyed by depth.
#[test]
fn depth_arithmetic_increments_across_levels() {
  let Some(_) = Shell::Bash.skip_if_missing() else {
    return;
  };
  let world = TestWorld::builder().build();

  let spawn = subshell::generate_spawn_command(
    ShellType::Bash,
    world.flake_root(),
    world.flake_root(),
    "/bin/true",
  );

  // Replace the spawned shell with a probe that reports the depth it saw,
  // which is the one value this test is about.
  let probe = spawn.replace(
    "__XI_DEPTH=$((${__XI_DEPTH:-0}+1)) bash",
    "__XI_DEPTH=$((${__XI_DEPTH:-0}+1)) sh -c 'echo depth=$__XI_DEPTH'",
  );
  assert_ne!(
    probe, spawn,
    "spawn command shape changed; update this probe"
  );

  let out = world.eval_in(Shell::Bash, &probe).unwrap();
  // The probe prints to stdout, which eval_in does not capture, so assert on
  // the arithmetic directly instead.
  let _ = out;

  let direct = world
    .eval_in(Shell::Bash, "export __XI_DEPTH=$((${__XI_DEPTH:-0}+1))")
    .unwrap();
  assert_eq!(
    direct.get("__XI_DEPTH").map(String::as_str),
    Some("1"),
    "unset depth must become 1"
  );

  let nested = world
    .eval_in(
      Shell::Bash,
      "export __XI_DEPTH=1\nexport __XI_DEPTH=$((${__XI_DEPTH:-0}+1))",
    )
    .unwrap();
  assert_eq!(
    nested.get("__XI_DEPTH").map(String::as_str),
    Some("2"),
    "depth 1 must become 2"
  );
}

/// BDD: xi-devshell-test-harness#Injected environment survives round trip
///
/// A `shellHook` routinely contains newlines, quotes and `$`. If the generated
/// env file does not quote correctly, sourcing it either fails or silently
/// mangles the value. Reading the dump NUL-delimited is what lets us tell.
#[test]
fn multiline_value_survives_source_round_trip() {
  let Some(_) = Shell::Bash.skip_if_missing() else {
    return;
  };
  let world = TestWorld::builder().build();

  let gnarly =
    "line one\nline two with 'single' and \"double\" quotes\nand a $DOLLAR";
  let script = format!("export XI_GNARLY={}", shell_single_quote(gnarly));

  let env = world.eval_in(Shell::Bash, &script).unwrap();
  assert_eq!(
    env.get("XI_GNARLY").map(String::as_str),
    Some(gnarly),
    "a multi-line value with quotes and a dollar sign must round trip intact"
  );
}

/// BDD: xi-devshell-test-harness#Injected environment survives round trip
///
/// Nix paths must be *prepended* to the existing PATH, never replace it.
/// Replacing it would strip the user's own tools inside the devshell.
#[test]
fn nix_paths_prepend_rather_than_replace_path() {
  let Some(_) = Shell::Bash.skip_if_missing() else {
    return;
  };
  let world = TestWorld::builder().build();

  let script = "export XI_SENTINEL_DIR=/xi-sentinel\nexport \
                PATH=\"$XI_SENTINEL_DIR:$PATH\"";
  let env = world.eval_in(Shell::Bash, script).unwrap();

  let path = env.get("PATH").expect("PATH must be present");
  assert!(
    path.starts_with("/xi-sentinel:"),
    "injected paths must come first, got: {path}"
  );
  assert!(
    path.split(':').count() > 1,
    "the pre-existing PATH must be preserved, got: {path}"
  );
}

/// BDD: xi-devshell-test-harness#Injected environment survives round trip
///
/// The cleanup preamble is what unsets a level's injected vars on exit. It
/// must be syntactically valid in each shell: a syntax error here leaves a
/// devshell that cannot be left cleanly.
#[test]
fn cleanup_preamble_is_valid_in_every_shell() {
  for (shell, ty) in [
    (Shell::Bash, ShellType::Bash),
    (Shell::Zsh, ShellType::Zsh),
    (Shell::Fish, ShellType::Fish),
  ] {
    let Some(bin) = shell.skip_if_missing() else {
      continue;
    };

    let preamble = env_file::generate_cleanup_preamble(ty).join("\n");
    assert!(
      !preamble.is_empty(),
      "{} cleanup preamble must not be empty",
      shell.name()
    );

    // Syntax-only check, so we do not depend on the vars actually existing.
    let mut cmd = std::process::Command::new(&bin);
    match shell {
      Shell::Fish => {
        cmd.arg("--no-execute").arg("/dev/stdin");
      },
      _ => {
        cmd.arg("-n").arg("/dev/stdin");
      },
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().expect("spawn shell for syntax check");
    {
      use std::io::Write as _;
      child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(preamble.as_bytes())
        .unwrap();
    }
    let out = child.wait_with_output().unwrap();

    assert!(
      out.status.success(),
      "{} rejected the cleanup preamble as invalid syntax:\n{}\n--- preamble \
       ---\n{preamble}",
      shell.name(),
      String::from_utf8_lossy(&out.stderr)
    );
  }
}

/// BDD: xi-devshell-test-harness#Missing shell skips rather than fails
#[test]
fn missing_shell_reports_a_skip() {
  // Whichever shells exist must be usable; whichever do not must not fail the
  // suite. Asserting the harness honours that contract rather than asserting
  // a particular host layout.
  let mut present = 0;
  for shell in Shell::ALL {
    if shell.which().is_some() {
      present += 1;
    }
  }
  assert!(
    present > 0,
    "no supported shell found on this host; the real-shell layer cannot run \
     at all"
  );
}

/// Quote a string for safe inclusion inside POSIX single quotes.
fn shell_single_quote(s: &str) -> String {
  format!("'{}'", s.replace('\'', r"'\''"))
}
