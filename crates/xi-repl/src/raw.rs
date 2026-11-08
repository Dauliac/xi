//! `:raw` escape hatch (`openspec/changes/xi-repl/tasks.md` task 7.3).
//!
//! Any expression the daemon's `ExprEval` path can't handle - a named
//! [`xi_develop::daemon::protocol_v3::ExprEvalResult::Error`], not a hang -
//! can be re-run against a real, unwrapped `nix repl` session for the rest
//! of that one evaluation. [`enter`] is also reachable directly from the
//! `:raw` meta-command, with or without a trailing expression.
//!
//! This calls into `nix-command`'s existing `CommandKind::Repl`
//! (`crates/nix-command/src/lib.rs`) exactly the way `xi-darwin`/`xi-home`/
//! `xi-nixos` already do for their own `nix repl` passthroughs - full stdio
//! inheritance via [`nix_command::NixCommand::run_with_logs`]. That
//! passthrough stays completely unmodified; this module only ever calls
//! into it as a subprocess.
//!
//! There is no pty layer in this workspace, so a `trigger` expression (the
//! one that failed, or the one typed after `:raw`) cannot be programmatically
//! typed into the child's stdin while the terminal is also shared with it
//! interactively - doing both at once needs a pty to multiplex. [`enter`]
//! prints it as a hint instead so the user can retype it once inside the
//! real session, which still satisfies "actually execute nix repl, not
//! silently no-op": the fallback always launches a genuine `nix repl`
//! process against `flake_root`, it just doesn't ghost-type for the user.

use std::path::Path;

use color_eyre::{Result, eyre::bail};
use nix_command::{CommandKind, NixCommand};
use yansi::{Color, Paint};

/// Build the `nix repl` invocation [`enter`] runs for `flake_root`.
///
/// Split out from [`enter`] as a plain, non-spawning function (task 8.4)
/// so `tests/bdd_raw_escape.rs` can assert on the exact
/// `nix_command::NixCommand`/argv the `:raw` escape hatch would run - and,
/// for the tests that do spawn it, drive its stdio itself - without going
/// through [`enter`]'s own stdio-inheriting [`NixCommand::run_with_logs`]
/// call.
#[must_use]
pub fn repl_command(flake_root: &Path) -> NixCommand {
  NixCommand::new(CommandKind::Repl)
    .arg(flake_root.to_string_lossy().into_owned())
    .with_required_env()
}

/// Drop into a real `nix repl` against `flake_root`, inheriting stdio, and
/// block until the user exits it (`:quit` or Ctrl-D) - control then returns
/// to the caller, which is `xi repl`'s own read-eval-print loop.
///
/// `trigger`, if given, is only echoed as a hint before the child starts;
/// see the module doc comment for why it is never fed into the child's
/// stdin.
///
/// # Errors
///
/// Returns an error if the `nix repl` subprocess cannot be spawned or
/// waited on, or if it exits with a non-success status.
pub fn enter(flake_root: &Path, trigger: Option<&str>) -> Result<()> {
  println!(
    "{}",
    Paint::new(hint_message(flake_root, trigger)).fg(Color::Yellow)
  );

  let status = repl_command(flake_root).run_with_logs()?;

  if !status.success() {
    bail!("nix repl exited with {status:?}");
  }

  Ok(())
}

/// Build [`enter`]'s pre-launch hint line - split out from the `println!`
/// so its wording is testable without spawning the real subprocess.
fn hint_message(flake_root: &Path, trigger: Option<&str>) -> String {
  let retype = trigger.map_or_else(String::new, |expr| {
    format!(" - retype `{expr}` once inside it")
  });
  format!(
    "xi repl: dropping into a real `nix repl` for {}{retype}. Exit with :quit \
     or Ctrl-D to return to xi repl.",
    flake_root.display()
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hint_without_trigger_has_no_retype_clause() {
    let message = hint_message(Path::new("/tmp/some-flake"), None);
    assert!(message.contains("some-flake"));
    assert!(!message.contains("retype"));
  }

  #[test]
  fn hint_with_trigger_mentions_it() {
    let message =
      hint_message(Path::new("/tmp/some-flake"), Some("pkgs.hello"));
    assert!(message.contains("retype"));
    assert!(message.contains("pkgs.hello"));
  }
}
