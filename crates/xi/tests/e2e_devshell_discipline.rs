//! BDD: openspec/changes/devshell-e2e-harness
//!   specs/xi-devshell-test-harness/spec.md#No elapsed-time synchronisation
//!   specs/xi-devshell-test-harness/spec.md#Layer placement rule
//!
//! Tests about the test suite. These exist because the two rules they enforce
//! are the ones that rot silently:
//!
//! * a fixed sleep used as a synchronisation primitive turns a real failure
//!   into an intermittent one, and nobody notices until CI is flaky,
//! * a behaviour tested in the slowest layer that a cheaper layer could have
//!   observed makes the fast suite progressively less useful.
//!
//! Enforcing them by grep is crude, but it is checkable and it fails loudly,
//! which beats a convention in a document nobody re-reads.

use std::{
  fs,
  path::{Path, PathBuf},
};

/// The harness plus every devshell e2e test binary.
fn suite_sources() -> Vec<PathBuf> {
  let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
  let mut out =
    vec![tests.join("harness/mod.rs"), tests.join("harness/pty.rs")];
  for entry in fs::read_dir(&tests).expect("tests dir must be readable") {
    let p = entry.expect("readable dir entry").path();
    let name = p
      .file_name()
      .unwrap_or_default()
      .to_string_lossy()
      .to_string();
    if name.starts_with("e2e_devshell_") && name.ends_with(".rs") {
      out.push(p);
    }
  }
  out
}

/// Lines where the harness is legitimately allowed to sleep: the poll
/// intervals inside a bounded wait-for-an-observable loop. Those are not
/// synchronisation primitives, they are the back-off between two checks of a
/// real signal.
fn is_sanctioned_poll(line: &str) -> bool {
  let l = line.trim();
  l.contains("sleep(POLL)")
    || l.contains("sleep(POLL_INTERVAL)")
    || l.starts_with("//")
    || l.starts_with("*")
}

/// BDD: xi-devshell-test-harness#Suite contains no sleep-based waits
///
/// A bare `sleep(Duration::from_*)` in a test body means "wait and hope".
/// Every legitimate wait in this suite goes through `TestWorld::wait_for`,
/// `PtyShell::read_until`, or `PtyShell::wait_for_exit`, each of which polls
/// a named observable against a deadline and reports what it was waiting for
/// when it gives up.
#[test]
fn suite_has_no_sleep_based_synchronisation() {
  let mut offenders: Vec<String> = Vec::new();

  for path in suite_sources() {
    // This file is scanned too: see the runtime-assembled needles below.
    let Ok(text) = fs::read_to_string(&path) else {
      continue;
    };
    for (i, line) in text.lines().enumerate() {
      if is_sanctioned_poll(line) {
        continue;
      }
      // Needles are assembled at runtime rather than written literally, so
      // this detector can scan its own source without matching itself. The
      // alternative, excluding this file, would leave the one file nobody
      // checks.
      let sep = "::";
      let qualified = format!("thread{sep}sleep");
      let call = format!("sleep{}Duration", "(");
      let has_sleep = line.contains(&qualified) || line.contains(&call);
      if has_sleep {
        offenders.push(format!(
          "{}:{}: {}",
          path.file_name().unwrap_or_default().to_string_lossy(),
          i + 1,
          line.trim()
        ));
      }
    }
  }

  assert!(
    offenders.is_empty(),
    "sleep used as a synchronisation primitive. Wait on an observable with a \
     deadline instead (TestWorld::wait_for, PtyShell::read_until, \
     PtyShell::wait_for_exit):\n  {}",
    offenders.join("\n  ")
  );
}

/// BDD: xi-devshell-test-harness#Deliberate delay is staged, not slept
///
/// Where a test genuinely needs slowness, it must come from the fake nix
/// under test instruction, not from the test process sleeping.
#[test]
fn deliberate_delay_is_staged_through_the_fake() {
  let fake = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../xi-develop/tests/fixtures/fake-nix");
  let text = fs::read_to_string(&fake).expect("fake-nix must be readable");

  assert!(
    text.contains(".delay"),
    "the fake must support a staged delay, so timeout and backoff paths are \
     reachable without the test itself sleeping"
  );
}

/// BDD: xi-devshell-test-harness#Interactive layer is gated
///
/// The interactive layer must opt in, so a host or sandbox with no
/// pseudo-terminal can still run everything else.
#[test]
fn interactive_layer_is_gated_behind_an_opt_in() {
  let path = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/e2e_devshell_interactive.rs");
  let text = fs::read_to_string(&path).expect("interactive layer must exist");

  assert!(
    text.contains("XI_E2E_PTY"),
    "the interactive layer must be gated behind the XI_E2E_PTY opt-in"
  );
  assert!(
    text.contains("fn pty_enabled()"),
    "the gate must be a single checkable function, not scattered conditionals"
  );

  // Every test in that file must consult the gate, or an ungated one would
  // fail the whole suite on a host with no pty.
  let test_count = text.matches("#[test]").count();
  let gate_count = text.matches("if !pty_enabled()").count();
  assert_eq!(
    gate_count, test_count,
    "all {test_count} interactive tests must check the gate, only \
     {gate_count} do"
  );
}

/// BDD: xi-devshell-test-harness#Script-content assertion is not promoted
///
/// The cheap layers must stay the default home for anything they can observe.
/// If the interactive layer ever outgrows the script-contract layer, the
/// placement rule has stopped being followed.
#[test]
fn cheap_layers_carry_more_tests_than_the_interactive_layer() {
  let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
  let count = |name: &str| -> usize {
    fs::read_to_string(tests.join(name))
      .map(|t| t.matches("#[test]").count())
      .unwrap_or(0)
  };

  let prompt = count("e2e_devshell_prompt.rs");
  let shell_eval = count("e2e_devshell_shell_eval.rs");
  let interactive = count("e2e_devshell_interactive.rs");

  assert!(
    prompt + shell_eval > interactive,
    "the cheap layers ({prompt} script-contract + {shell_eval} real-shell) \
     must outweigh the interactive layer ({interactive}). A behaviour belongs \
     in the interactive layer only if it needs a prompt hook to fire, an exit \
     status to cross a parent/child boundary, or isolation between two live \
     shells."
  );
}

/// BDD: xi-devshell-test-harness#Reverting the fix fails a named test
///
/// The acceptance condition for the whole harness must stay present and
/// named. If someone deletes it, the suite loses the one defect it was built
/// to catch.
#[test]
fn the_cdout_regression_anchor_still_exists() {
  let path = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/e2e_devshell_shell_eval.rs");
  let text = fs::read_to_string(&path).expect("real-shell layer must exist");

  assert!(
    text.contains("fn zsh_cdout_write_survives_no_clobber"),
    "the named cd-out regression anchor must exist (SC-H-004)"
  );
  assert!(
    text.contains("NO_CLOBBER"),
    "the anchor must actually set NO_CLOBBER, or it proves nothing"
  );

  // The behavioural assertion must come before the cheaper text assertion, or
  // reverting the fix trips the text check and never proves the behavioural
  // one works. This ordering was a real bug during development.
  let behavioural = text
    .find("THE CD-OUT WRITE WAS DROPPED")
    .expect("the behavioural assertion message must be present");
  let textual = text
    .find("the cd-out snippet must force-write")
    .expect("the secondary text assertion must be present");
  assert!(
    behavioural < textual,
    "the behavioural assertion must precede the text assertion, so the anchor \
     is proven by running a real zsh rather than by grepping source"
  );
}
