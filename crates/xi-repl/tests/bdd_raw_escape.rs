//! BDD coverage for the `:raw` escape hatch (`OpenSpec` task 8.4): proves it
//! really constructs and spawns a `nix_command::NixCommand::new(nix_command
//! ::CommandKind::Repl)` invocation rather than silently no-op'ing.
//!
//! There is no pty in this workspace (see `xi_repl::raw`'s module doc
//! comment), so a synthetic-reedline-event harness would not add anything
//! here: `:raw`'s own code never touches `reedline` at all once dispatched -
//! it hands the whole terminal to a child process. The actual
//! command-dispatch function, [`xi_repl::raw::enter`], is already factored
//! out from the interactive loop (`session::handle_raw` just calls it), so
//! these tests drive it - and its pure command-builder half,
//! [`xi_repl::raw::repl_command`] - directly.
//!
//! `repl_command` did not exist before task 8.4; it was split out of
//! `enter` (see that function's doc comment) purely so the tests below can
//! assert on the exact argv without needing to fork `enter`'s own
//! stdio-inheriting subprocess call.
//!
//! The string-matching half of dispatch (recognizing `:raw`/`:raw <expr>`
//! as the trigger at all) is already covered in-crate by
//! `session::tests::strip_meta_command_*`; it is not re-tested here.

#![expect(
  clippy::panic,
  reason = "Fine in tests - asserting a background thread's outcome"
)]

use std::{path::Path, process::Stdio, sync::mpsc, thread, time::Duration};

use nix_command::{CommandKind, NixCommand};
use xi_repl::raw;

/// `repl_command` must build exactly `nix repl <flake_root>` - no extra
/// flags smuggled in, no flags dropped. `nix repl`'s `CommandSpec` has
/// `print_build_logs: false`, so (unlike e.g. `nix build`) nothing gets
/// inserted between the subcommand and the flake argument.
#[test]
fn repl_command_argv_is_exactly_nix_repl_on_the_flake_root() {
  let flake_root = Path::new("/some/flake/root");
  let argv = raw::repl_command(flake_root).argv();

  assert_eq!(
    argv.len(),
    3,
    "expected exactly [nix-binary, repl, flake-root], got {argv:?}"
  );

  let binary = argv[0].to_string_lossy();
  assert!(
    binary == "nix" || binary.ends_with("/nix"),
    "argv[0] should be a nix binary, got {binary}"
  );
  assert_eq!(argv[1].to_string_lossy(), "repl");
  assert_eq!(argv[2].to_string_lossy(), flake_root.to_string_lossy());
}

/// Ties `repl_command` back explicitly to `nix_command::CommandKind::Repl`
/// (not some other schema that happens to also take one positional arg):
/// a bare `NixCommand::new(CommandKind::Repl)` and `repl_command`'s own
/// invocation must agree on everything up to the flake-root argument
/// `repl_command` appends on top.
#[test]
fn repl_command_is_built_on_commandkind_repl() {
  let flake_root = Path::new("/some/flake/root");
  let bare = NixCommand::new(CommandKind::Repl).argv();
  let built = raw::repl_command(flake_root).argv();

  assert_eq!(
    built[..bare.len()],
    bare[..],
    "repl_command's argv must extend a bare CommandKind::Repl invocation, not \
     diverge from it"
  );
  assert_eq!(
    built.len(),
    bare.len() + 1,
    "exactly one argument - the flake root - should follow the bare \
     CommandKind::Repl prefix"
  );
}

/// Spawns the exact command `repl_command` builds, with stdio piped
/// (rather than inherited, so this can run headless) against a directory
/// with no `flake.nix`. Real `nix repl` prints its version banner to
/// stderr and fails fast - before ever trying to read a command from
/// stdin - when it cannot resolve the flake it was pointed at, so this
/// proves a genuine `nix repl` subprocess ran, and does so without any risk
/// of hanging on interactive input.
///
/// Skips (reporting why, rather than failing) if no real `nix` binary is
/// reachable - see this crate's final report for why CI needs one for this
/// specific assertion.
#[test]
fn repl_command_actually_spawns_a_real_nix_repl_process() {
  let Ok(dir) = tempfile::tempdir() else {
    eprintln!(
      "skipping repl_command_actually_spawns_a_real_nix_repl_process: could \
       not create a temp dir"
    );
    return;
  };

  let mut cmd = raw::repl_command(dir.path()).to_std_command();
  cmd
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

  let output = match cmd.output() {
    Ok(output) => output,
    Err(err) => {
      eprintln!(
        "skipping repl_command_actually_spawns_a_real_nix_repl_process: could \
         not spawn a `nix` binary ({err}) - this assertion needs a real `nix` \
         on PATH"
      );
      return;
    },
  };

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("Type :? for help"),
    "expected the real `nix repl` banner in stderr (proof this is a genuine \
     `nix repl` process, not a stub), got: {stderr}"
  );
  assert!(
    !output.status.success(),
    "a flake-less temp dir ({}) should make `nix repl` fail fast trying to \
     resolve it as a flake, proving it genuinely attempted to load the flake \
     root rather than silently succeeding without ever looking at it - got \
     status {:?}, stderr: {stderr}",
    dir.path().display(),
    output.status
  );
}

/// Calls [`raw::enter`] itself - the exact function `session::handle_raw`
/// dispatches `:raw` to - rather than just its command-builder half, so the
/// escape hatch's whole public entry point is proven not to silently
/// no-op. Safe to call directly despite `enter`'s own stdio-inheriting
/// `run_with_logs`: pointing it at a flake-less directory makes real `nix
/// repl` fail before it ever reads from stdin (verified by the previous
/// test's banner/non-success assertions against the same kind of
/// directory), so there is nothing to hang on even when stdio is
/// inherited. The wait is still bounded defensively, in case some
/// environment's `nix repl` behaves differently than verified above.
#[test]
fn enter_does_not_silently_no_op() {
  let Ok(dir) = tempfile::tempdir() else {
    eprintln!(
      "skipping enter_does_not_silently_no_op: could not create a temp dir"
    );
    return;
  };
  let path = dir.path().to_path_buf();

  let (tx, rx) = mpsc::channel();
  thread::spawn(move || {
    let _ = tx.send(raw::enter(&path, None));
  });

  match rx.recv_timeout(Duration::from_secs(20)) {
    Ok(Ok(())) => {
      panic!(
        "nix repl against a flake-less directory unexpectedly reported \
         success - the escape hatch should have surfaced a real failure from \
         a real `nix repl` process, not silently returned Ok"
      )
    },
    Ok(Err(_)) => {
      // Expected: a real `nix repl` subprocess ran against a directory it
      // cannot resolve as a flake and `enter` surfaced that failure.
    },
    Err(mpsc::RecvTimeoutError::Timeout) => {
      panic!(
        "enter() against a flake-less directory did not return within 20s - \
         this needs investigating as a possible hang, not just a slow/missing \
         `nix`, since the same scenario fails in well under a second when \
         driven directly via `repl_command` above"
      );
    },
    Err(mpsc::RecvTimeoutError::Disconnected) => {
      panic!("the thread running enter() panicked before sending a result");
    },
  }
}
