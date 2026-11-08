//! Daemon lifecycle (task 4.3) and the Phase 0 read-eval-print loop
//! (task 4.4).
//!
//! Round-trip latency for a warm `ExprEval` call against a nixpkgs-sized
//! flake is not yet measured  -  see `tests/LATENCY_NOTES.md` (task 4.5) for
//! the methodology and why the number is still pending.

use std::path::{Path, PathBuf};

use color_eyre::Result;
use reedline::{
  DefaultPrompt,
  DefaultPromptSegment,
  DescriptionMode,
  EditCommand,
  Emacs,
  FileBackedHistory,
  HISTORY_SIZE,
  IdeMenu,
  KeyCode,
  KeyModifiers,
  MenuBuilder,
  Reedline,
  ReedlineEvent,
  ReedlineMenu,
  Signal,
};
use xi_develop::{
  daemon::{
    client_v3::{self, QueryOutcome},
    lifecycle,
    protocol_v3::{Availability, ExprEvalResult, QueryReply},
  },
  dirs,
};
use yansi::{Color, Paint};

use crate::{
  completion::AttrCompleter,
  doc,
  error::ReplError,
  highlight::NixHighlighter,
  raw,
  render,
};

/// Name the completion menu is registered under, shared between
/// [`build_line_editor`]'s `with_menu` call and its keybindings.
const COMPLETION_MENU_NAME: &str = "xi_completion_menu";

/// Resolve the daemon socket for `flake_root`.
///
/// `no_daemon: false` (the default) reuses `xi-develop`'s own "connect, else
/// spawn and wait for the socket" lifecycle ([`lifecycle::ensure`])  -  the
/// same path `xi develop`'s shell entry uses, just scoped to this one `xi
/// repl` invocation rather than a long-lived shell session.
///
/// `no_daemon: true` never spawns. It only computes the socket path a
/// daemon for this flake *would* use (the exact derivation `xi-develop`'s
/// daemon lifecycle itself uses: [`dirs::flake_id`] +
/// [`dirs::daemon_socket_path`]), in case one happens to already be
/// reachable from an unrelated session (e.g. you are inside an `xi develop`
/// shell already). Liveness is checked lazily, per evaluation
/// (see [`eval`])  -  the prompt still opens immediately either way, there is
/// nothing to block on until the first expression is typed.
///
/// # Errors
///
/// Returns an error if `flake_root` cannot be used to spawn a daemon (only
/// reachable when `no_daemon` is false).
pub fn resolve_socket(flake_root: &Path, no_daemon: bool) -> Result<PathBuf> {
  if no_daemon {
    let fid = dirs::flake_id(flake_root);
    return Ok(dirs::daemon_socket_path(&fid));
  }
  lifecycle::ensure(flake_root)
}

/// Per-flake-root history file path (task 7.2).
///
/// Mirrors the same derivation `xi-develop`'s own daemon lifecycle already
/// uses to scope its per-flake state - [`dirs::flake_id`] hashing the
/// canonical flake path, then a directory under
/// [`dirs::xi_state_root`] keyed by that id, the same
/// `$XDG_STATE_HOME/xi/<component>/<flake_id>/` shape [`dirs::state_dir`]
/// uses for `develop`'s own state. This crate's history has nothing to do
/// with `xi-develop`'s daemon state, so it gets its own `repl` component
/// rather than reusing `dirs::state_dir` (which is hard-coded to
/// `develop`) - but the same flake, opened twice from different paths (or
/// two different flakes), still never share a history file.
#[must_use]
pub fn history_path(flake_root: &Path) -> PathBuf {
  let fid = dirs::flake_id(flake_root);
  dirs::xi_state_root().join("repl").join(fid).join("history")
}

/// Send one expression to the daemon's `ExprEval` query and decode its
/// typed, classified reply.
///
/// # Errors
///
/// Returns [`ReplError::DaemonUnreachable`] when the socket cannot be
/// connected to at all, and [`ReplError::UnexpectedReply`] when the daemon
/// answers with any shape an `ExprEval` query must never produce (a v2-only
/// daemon, or a `Pending`/`Missing`/`Degraded` availability  -  queries
/// answer synchronously by contract, see
/// `protocol_v3::query::Query::ExprEval`'s doc comment).
pub fn eval(
  socket_path: &Path,
  flake_root: &Path,
  expr: &str,
) -> Result<ExprEvalResult> {
  let outcome =
    client_v3::expr_eval(socket_path, expr, false).map_err(|_| {
      ReplError::DaemonUnreachable {
        socket: socket_path.to_path_buf(),
        flake:  flake_root.to_path_buf(),
      }
    })?;

  match outcome {
    QueryOutcome::V3(QueryReply::ExprEval(availability)) => {
      match availability {
        Availability::Ready { value } => Ok(value),
        Availability::Pending { job_id } => {
          Err(
            ReplError::UnexpectedReply {
              detail: format!(
                "replied Pending (job {job_id})  -  queries must answer \
                 synchronously"
              ),
            }
            .into(),
          )
        },
        Availability::Missing => {
          Err(
            ReplError::UnexpectedReply {
              detail: "replied Missing".to_owned(),
            }
            .into(),
          )
        },
        Availability::Degraded { reason } => {
          Err(
            ReplError::UnexpectedReply {
              detail: format!("daemon degraded: {reason}"),
            }
            .into(),
          )
        },
      }
    },
    QueryOutcome::V3(other) => {
      Err(
        ReplError::UnexpectedReply {
          detail: format!("{other:?}"),
        }
        .into(),
      )
    },
    QueryOutcome::V2(_) => {
      Err(
        ReplError::UnexpectedReply {
          detail: "daemon speaks v2 only  -  it has no ExprEval query"
            .to_owned(),
        }
        .into(),
      )
    },
  }
}

/// Build the `reedline` editor for `socket_path`/`flake_root`: Phase 0's
/// plain line editing, Phase 1's `.`/Tab-triggered completion
/// ([`AttrCompleter`]) with an `IdeMenu` preview pane, Phase 2's
/// `rnix-parser`-based syntax highlighting ([`NixHighlighter`]), and
/// Phase 3's per-flake-root history ([`history_path`]).
///
/// Pressing `.` both inserts the character and opens the completion menu
/// (task 5.1's "on `.` or Tab"); `Tab` opens it (or advances the selection
/// if it is already open) without inserting anything, matching `reedline`'s
/// own example keybindings for a completion menu.
///
/// # Errors
///
/// Returns an error if the history file's directory cannot be created or
/// the existing history file cannot be read.
fn build_line_editor(
  socket_path: &Path,
  flake_root: &Path,
) -> Result<Reedline> {
  let completer = Box::new(AttrCompleter::new(socket_path.to_path_buf()));

  let completion_menu = Box::new(
    IdeMenu::default()
      .with_name(COMPLETION_MENU_NAME)
      .with_description_mode(DescriptionMode::PreferRight),
  );

  let mut keybindings = reedline::default_emacs_keybindings();
  keybindings.add_binding(
    KeyModifiers::NONE,
    KeyCode::Tab,
    ReedlineEvent::UntilFound(vec![
      ReedlineEvent::Menu(COMPLETION_MENU_NAME.to_owned()),
      ReedlineEvent::MenuNext,
    ]),
  );
  keybindings.add_binding(
    KeyModifiers::NONE,
    KeyCode::Char('.'),
    ReedlineEvent::Multiple(vec![
      ReedlineEvent::Edit(vec![EditCommand::InsertChar('.')]),
      ReedlineEvent::Menu(COMPLETION_MENU_NAME.to_owned()),
    ]),
  );
  let edit_mode = Box::new(Emacs::new(keybindings));

  // `FileBackedHistory::with_file` creates the file's parent directories
  // itself (see its doc comment in `reedline`), so there is nothing to
  // `create_dir_all` here beyond what it already does.
  let history = Box::new(FileBackedHistory::with_file(
    HISTORY_SIZE,
    history_path(flake_root),
  )?);

  Ok(
    Reedline::create()
      .with_completer(completer)
      .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
      .with_edit_mode(edit_mode)
      .with_highlighter(Box::new(NixHighlighter))
      .with_history(history),
  )
}

/// Run the reedline read-eval-print loop against `socket_path`.
///
/// One expression per line; `reedline`'s own multiline handling (quote/paren
/// balancing) is used as-is for Phase 0  -  no custom Nix-aware multiline
/// detection yet.
///
/// # Errors
///
/// Returns an error only if the terminal itself cannot be read from (raw
/// mode setup failure, etc)  -  a failed evaluation is printed in red and the
/// loop continues.
pub fn run_loop(socket_path: &Path, flake_root: &Path) -> Result<()> {
  let mut line_editor = build_line_editor(socket_path, flake_root)?;
  let prompt = DefaultPrompt::new(
    DefaultPromptSegment::Basic("xi".to_owned()),
    DefaultPromptSegment::Empty,
  );

  loop {
    match line_editor.read_line(&prompt) {
      Ok(Signal::Success(line)) => {
        let input = line.trim();
        if input.is_empty() {
          continue;
        }

        if let Some(rest) = strip_meta_command(input, ":doc") {
          handle_doc(socket_path, flake_root, rest);
          continue;
        }

        if let Some(rest) = strip_meta_command(input, ":raw") {
          handle_raw(flake_root, rest);
          continue;
        }

        match eval(socket_path, flake_root, input) {
          Ok(ExprEvalResult::Error { message }) => {
            eprintln!("{}", Paint::new(message).fg(Color::Red));
            eprintln!(
              "{}",
              Paint::new(
                "xi repl: the daemon's eval path rejected this expression - \
                 dropping into a real `nix repl` for it (same as typing \
                 `:raw`)"
              )
              .fg(Color::Yellow)
            );
            handle_raw(flake_root, input);
          },
          Ok(result) => println!("{}", render::render(&result)),
          Err(err) => {
            eprintln!("{}", Paint::new(err.to_string()).fg(Color::Red));
          },
        }
      },
      Ok(Signal::CtrlC | Signal::CtrlD) => break,
      // `Signal` is `#[non_exhaustive]` (its `CustomPayload` variant is
      // only ever produced by a `ReedlineEvent::ExecuteHostCommand` keybind
      // we never register)  -  Phase 0 has no custom keybinds, so treat
      // anything else as a no-op rather than matching it by name.
      Ok(_) => {},
      Err(err) => {
        eprintln!("xi repl: input error: {err}");
        break;
      },
    }
  }

  Ok(())
}

/// Strip a `:name` meta-command prefix from `input`, requiring that the
/// prefix either consume the whole input or be followed by whitespace -
/// `:doc` must not also match a hypothetical future `:docfoo` command.
/// Returns the trimmed remainder (empty when the input was exactly
/// `:name`).
fn strip_meta_command<'a>(input: &'a str, name: &str) -> Option<&'a str> {
  let rest = input.strip_prefix(name)?;
  if rest.is_empty() || rest.starts_with(char::is_whitespace) {
    Some(rest.trim())
  } else {
    None
  }
}

/// Dispatch the `:doc` meta-command (task 7.1). `expr` is already trimmed
/// of the `:doc` prefix; an empty `expr` ("the user typed `:doc` alone") is
/// a usage error, not a doc-scope one, so it gets its own message rather
/// than [`doc::run`]'s "not a package" wording.
fn handle_doc(socket_path: &Path, flake_root: &Path, expr: &str) {
  if expr.is_empty() {
    eprintln!(
      "{}",
      Paint::new(":doc needs an expression, e.g. `:doc pkgs.hello`")
        .fg(Color::Yellow)
    );
    return;
  }
  doc::run(socket_path, flake_root, expr);
}

/// Dispatch the `:raw` meta-command (task 7.3). `rest` is already trimmed
/// of the `:raw` prefix; an empty `rest` means "just open a real
/// `nix repl`", a non-empty one is echoed as the expression to retype once
/// inside it (see [`raw`]'s doc comment for why it can't be ghost-typed).
fn handle_raw(flake_root: &Path, rest: &str) {
  let trigger = if rest.is_empty() { None } else { Some(rest) };
  if let Err(err) = raw::enter(flake_root, trigger) {
    eprintln!(
      "{}",
      Paint::new(format!("xi repl: nix repl failed: {err}")).fg(Color::Red)
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn strip_meta_command_matches_exact_name() {
    assert_eq!(strip_meta_command(":doc", ":doc"), Some(""));
  }

  #[test]
  fn strip_meta_command_matches_name_plus_argument() {
    assert_eq!(
      strip_meta_command(":doc pkgs.hello", ":doc"),
      Some("pkgs.hello")
    );
  }

  #[test]
  fn strip_meta_command_rejects_a_longer_command_with_the_same_prefix() {
    // `:docfoo` must not be treated as `:doc` with a glued-on argument.
    assert_eq!(strip_meta_command(":docfoo", ":doc"), None);
  }

  #[test]
  fn strip_meta_command_rejects_an_unrelated_command() {
    assert_eq!(strip_meta_command(":raw", ":doc"), None);
  }

  #[test]
  fn history_path_is_scoped_per_flake_root() {
    let a = history_path(Path::new("/tmp/flake-a"));
    let b = history_path(Path::new("/tmp/flake-b"));
    assert_ne!(a, b, "different flake roots must not share a history file");
    assert_eq!(
      history_path(Path::new("/tmp/flake-a")),
      a,
      "the same flake root must always resolve to the same history file"
    );
  }

  #[test]
  fn history_path_lives_under_its_own_repl_component() {
    let path = history_path(Path::new("/tmp/flake-a"));
    // Distinct from `dirs::state_dir`'s `develop` component - this history
    // has nothing to do with the daemon's own state.
    assert!(path.components().any(|c| c.as_os_str() == "repl"));
    assert!(!path.components().any(|c| c.as_os_str() == "develop"));
    assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("history"));
  }
}
