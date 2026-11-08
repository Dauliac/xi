//! `.`/Tab-triggered attribute-path completion with a preview pane
//! (`openspec/changes/xi-repl/tasks.md` tasks 5.1-5.3).
//!
//! [`AttrCompleter`] implements `reedline`'s [`Completer`] trait. On each
//! completion request it:
//!
//! 1. Splits the typed token on its last `.` into a base expression and a
//!    partial attribute name, then sends `ExprEval{expr: base, shallow: true}`
//!    to the daemon and turns the returned `Attrs(Vec<String>)` names (filtered
//!    by the partial) into `reedline` `Suggestion`s  -  this part is
//!    synchronous, matching task 5.1's wording exactly ("`.`/Tab-triggered
//!    completion ... candidate attribute names populate reedline's completion
//!    menu"). A token with no `.` yet (including an empty line - a bare Tab
//!    press before typing anything) sends the empty-expression "current scope"
//!    sentinel instead of giving up, which `handle_expr_eval` answers from
//!    already-cached devshell package names without shelling out - this lists
//!    what the running devshell provides, not the flake's own output names
//!    (`packages`, `nixosConfigurations`, ...); those still need the `.` after
//!    typing them, since listing the flake's actual top level needs the
//!    (potentially slow, uncached) `builtins.getFlake` path `session::eval`
//!    uses for any other expression.
//! 2. Enriches only a bounded window of those candidates (task 5.2) with a
//!    `meta.description`/version preview, one further shallow `ExprEval` per
//!    candidate, rendered through the same [`crate::render::render`] this crate
//!    already uses for top-level results (which itself defers to
//!    [`xi_core::nix_value::render_entry`] for derivations).
//! 3. Runs that enrichment on a background thread, feeding results back through
//!    an `mpsc` channel drained non-blockingly from
//!    [`Completer::poll_completion`] (task 5.3)  -  the same thread+channel
//!    shape `nix-command`'s `read_pipe`/`PipeEvent` uses for streaming
//!    subprocess output, applied here to streaming daemon round-trips instead.
//!
//! ## API limitation, noted honestly
//!
//! `reedline`'s `Completer` trait has no way to ask "which suggestions is
//! the active menu currently rendering" - pagination/scrolling is internal
//! to the `Menu` implementation (`ColumnarMenu`/`IdeMenu`), computed from
//! terminal size *after* `complete()` already returned. So "only the
//! candidates currently visible in the dropdown" (task 5.2) cannot be
//! tracked exactly from inside a `Completer`; [`ENRICH_WINDOW`] is a fixed
//! bound chosen to approximate a typical dropdown's visible row count
//! instead of enriching every match on every keystroke. A real viewport
//! would need a `reedline` API change (or forking the crate) to expose the
//! menu's visible range to the completer.
//!
//! The preview itself also can't carry `yansi` color codes as `reedline`
//! renders it: `IdeMenu`'s description box measures line width with
//! `unicode_width`, which doesn't know about ANSI escapes and would
//! miscount them as visible columns, corrupting the box's wrapping/padding.
//! [`strip_ansi`] in [`crate::render`] strips them back out before the text
//! reaches `Suggestion::description`, so the preview pane is the shared
//! renderer's *text*, not its color.

use std::{path::PathBuf, sync::mpsc, thread};

use reedline::{
  Completer,
  CompletionResult,
  CompletionStatus,
  Span,
  Suggestion,
};
use xi_develop::daemon::{
  client_v3::{self, QueryOutcome},
  protocol_v3::{Availability, ExprEvalResult, QueryReply},
};

/// Upper bound on how many candidates get enriched per completion request.
///
/// See the module doc comment's "API limitation" section  -  this stands in
/// for "only the candidates currently visible in the dropdown" (task 5.2),
/// which `reedline`'s `Completer` trait has no way to learn exactly.
const ENRICH_WINDOW: usize = 12;

/// Base expression and partial attribute name parsed out of the buffer
/// immediately left of the cursor.
struct AttrPrefix {
  base_expr: String,
  partial:   String,
  span:      Span,
}

/// A character that can appear inside a Nix attribute-path token
/// (`pkgs.hello-world`, quoted segments aside  -  quoting is not handled in
/// this phase).
fn is_token_char(ch: char) -> bool {
  ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '\'')
}

/// Parse the attribute-path token ending at `pos`, splitting it on its last
/// `.` into a base expression (sent to the daemon) and a partial name (used
/// to filter the daemon's answer locally, never sent over the wire).
///
/// When the token has no `.` at all (including an empty token - a bare Tab
/// press on an otherwise-empty line), `base_expr` comes back as `""`, the
/// same empty-expression sentinel `xi-develop`'s `handle_expr_eval` already
/// fast-paths to "what's in scope right now" (the running devshell's
/// resolved package names) without shelling out to `nix eval` - so Tab at
/// an empty prompt completes against that instead of returning nothing.
/// `partial` is then whatever's typed so far, to filter it locally.
///
/// Returns `None` only for a leading `.` with nothing before it (e.g.
/// `.hel`) - that is not valid Nix syntax to complete attributes *of*,
/// unlike a plain dot-less identifier prefix.
fn attr_prefix(line: &str, pos: usize) -> Option<AttrPrefix> {
  let pos = pos.min(line.len());
  let head = &line[..pos];

  let mut start = pos;
  for (idx, ch) in head.char_indices().rev() {
    if is_token_char(ch) {
      start = idx;
    } else {
      break;
    }
  }
  let token = &head[start..];

  let Some(dot_pos) = token.rfind('.') else {
    return Some(AttrPrefix {
      base_expr: String::new(),
      partial:   token.to_owned(),
      span:      Span::new(start, pos),
    });
  };

  let base_expr = &token[..dot_pos];
  let partial = &token[dot_pos + 1..];
  if base_expr.is_empty() {
    return None;
  }

  let partial_start = start + dot_pos + 1;
  Some(AttrPrefix {
    base_expr: base_expr.to_owned(),
    partial:   partial.to_owned(),
    span:      Span::new(partial_start, pos),
  })
}

/// One completion request's state: the parsed prefix, and the suggestions
/// for it  -  initially unenriched, filled in with preview text as
/// [`EnrichMsg::Item`]s arrive.
struct RequestState {
  request_id:  u64,
  base_expr:   String,
  partial:     String,
  span:        Span,
  suggestions: Vec<Suggestion>,
}

/// A message from the background enrichment thread back to
/// [`AttrCompleter::poll_completion`].
enum EnrichMsg {
  /// One candidate's preview text is ready.
  Item {
    request_id: u64,
    name:       String,
    preview:    String,
  },
  /// The enrichment thread has sent every candidate in its window and is
  /// about to exit.
  Done { request_id: u64 },
}

/// `reedline` [`Completer`] for Nix attribute paths, backed by the
/// `xi-develop` daemon's `ExprEval` query.
///
/// See the module doc comment for the full request/enrichment flow.
pub struct AttrCompleter {
  socket_path:          PathBuf,
  current:              Option<RequestState>,
  rx:                   Option<mpsc::Receiver<EnrichMsg>>,
  enrichment_in_flight: bool,
  next_request_id:      u64,
}

impl AttrCompleter {
  /// Build a completer that queries the daemon reachable at `socket_path`.
  ///
  /// Does not connect eagerly  -  the first connection attempt happens on
  /// the first `.`/Tab press, same as [`crate::session::eval`].
  #[must_use]
  pub const fn new(socket_path: PathBuf) -> Self {
    Self {
      socket_path,
      current: None,
      rx: None,
      enrichment_in_flight: false,
      next_request_id: 0,
    }
  }

  /// Fetch `base_expr`'s shallow attribute names from the daemon.
  ///
  /// Swallows every failure mode (unreachable daemon, a non-`Attrs` reply
  /// because `base_expr` isn't an attrset, a protocol mismatch) into an
  /// empty candidate list: [`Completer::complete`] has no error channel, so
  /// "no completions right now" is the only honest answer to give back, the
  /// same convention `reedline`'s own `HistoryCompleter` uses.
  fn fetch_attrs(&self, base_expr: &str) -> Vec<String> {
    match client_v3::expr_eval(&self.socket_path, base_expr, true) {
      Ok(QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready {
        value: ExprEvalResult::Attrs(names),
      }))) => names,
      _ => Vec::new(),
    }
  }

  /// Spawn the background enrichment thread for the first
  /// [`ENRICH_WINDOW`] `names`, replacing (and thereby abandoning) any
  /// enrichment still running for a previous request.
  ///
  /// Dropping the previous `rx` here disconnects its paired `tx`; the old
  /// thread's next `tx.send` then fails and it exits on its own, so slow
  /// enrichment for a prefix the user has already typed past never
  /// accumulates unbounded background threads.
  fn spawn_enrichment(
    &mut self,
    request_id: u64,
    base_expr: String,
    names: &[String],
  ) {
    let window: Vec<String> =
      names.iter().take(ENRICH_WINDOW).cloned().collect();
    let (tx, rx) = mpsc::channel();
    self.rx = Some(rx);
    self.enrichment_in_flight = !window.is_empty();

    if window.is_empty() {
      return;
    }

    let socket_path = self.socket_path.clone();
    thread::spawn(move || {
      for name in window {
        let full_path = format!("{base_expr}.{name}");
        let preview = client_v3::expr_eval(&socket_path, &full_path, true)
          .ok()
          .and_then(|outcome| {
            match outcome {
              QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready {
                value,
              })) => Some(value),
              _ => None,
            }
          })
          .map(|value| {
            crate::render::strip_ansi(&crate::render::render(&value))
          });

        let Some(preview) = preview else { continue };
        if tx
          .send(EnrichMsg::Item {
            request_id,
            name,
            preview,
          })
          .is_err()
        {
          return;
        }
      }
      let _ = tx.send(EnrichMsg::Done { request_id });
    });
  }
}

impl Completer for AttrCompleter {
  fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
    let Some(prefix) = attr_prefix(line, pos) else {
      self.current = None;
      return CompletionResult::fresh(Vec::<Suggestion>::new());
    };

    let is_same_request = self.current.as_ref().is_some_and(|c| {
      c.base_expr == prefix.base_expr
        && c.partial == prefix.partial
        && c.span == prefix.span
    });
    if is_same_request {
      // Nothing moved on screen since the last request  -  this call is
      // `settle_completions` re-asking after `poll_completion` reported
      // `Ready`. The suggestions already carry whatever enrichment has
      // landed so far; no need to re-query the daemon for the base attrs
      // or respawn enrichment.
      let suggestions = self
        .current
        .as_ref()
        .map(|c| c.suggestions.clone())
        .unwrap_or_default();
      return CompletionResult::fresh(suggestions);
    }

    self.next_request_id += 1;
    let request_id = self.next_request_id;

    let mut names = self.fetch_attrs(&prefix.base_expr);
    names.retain(|n| n.starts_with(&prefix.partial));
    names.sort();

    let suggestions: Vec<Suggestion> = names
      .iter()
      .map(|name| {
        Suggestion {
          value: name.clone(),
          span: prefix.span,
          append_whitespace: false,
          ..Default::default()
        }
      })
      .collect();

    self.spawn_enrichment(request_id, prefix.base_expr.clone(), &names);

    self.current = Some(RequestState {
      request_id,
      base_expr: prefix.base_expr,
      partial: prefix.partial,
      span: prefix.span,
      suggestions: suggestions.clone(),
    });

    CompletionResult::fresh(suggestions)
  }

  fn poll_completion(&mut self) -> CompletionStatus {
    let mut updated = false;

    if let Some(rx) = &self.rx {
      loop {
        match rx.try_recv() {
          Ok(EnrichMsg::Item {
            request_id,
            name,
            preview,
          }) => {
            if let Some(current) = &mut self.current
              && current.request_id == request_id
              && let Some(suggestion) =
                current.suggestions.iter_mut().find(|s| s.value == name)
            {
              suggestion.description = Some(preview);
              updated = true;
            }
            // A batch for a request we've since moved past: discard, the
            // thread that sent it is already winding down on its own.
          },
          Ok(EnrichMsg::Done { request_id }) => {
            if self
              .current
              .as_ref()
              .is_some_and(|c| c.request_id == request_id)
            {
              self.enrichment_in_flight = false;
            }
          },
          Err(mpsc::TryRecvError::Empty) => break,
          Err(mpsc::TryRecvError::Disconnected) => {
            self.rx = None;
            self.enrichment_in_flight = false;
            break;
          },
        }
      }
    }

    if updated {
      CompletionStatus::Ready
    } else if self.enrichment_in_flight {
      CompletionStatus::Pending
    } else {
      CompletionStatus::Idle
    }
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn no_dot_completes_against_current_scope() {
    let prefix = attr_prefix("hel", 3).expect("should complete against scope");
    assert_eq!(prefix.base_expr, "");
    assert_eq!(prefix.partial, "hel");
    assert_eq!(prefix.span, Span::new(0, 3));
  }

  #[test]
  fn bare_tab_on_an_empty_line_completes_against_current_scope() {
    let prefix = attr_prefix("", 0).expect("should complete against scope");
    assert_eq!(prefix.base_expr, "");
    assert_eq!(prefix.partial, "");
    assert_eq!(prefix.span, Span::new(0, 0));
  }

  #[test]
  fn leading_dot_with_nothing_before_it_is_rejected() {
    assert!(attr_prefix(".hel", 4).is_none());
  }

  #[test]
  fn splits_on_the_last_dot() {
    let prefix = attr_prefix("pkgs.hel", 8).expect("should parse");
    assert_eq!(prefix.base_expr, "pkgs");
    assert_eq!(prefix.partial, "hel");
    assert_eq!(prefix.span, Span::new(5, 8));
  }

  #[test]
  fn nested_path_keeps_everything_before_the_last_dot_as_base() {
    let prefix = attr_prefix("pkgs.hello.me", 13).expect("should parse");
    assert_eq!(prefix.base_expr, "pkgs.hello");
    assert_eq!(prefix.partial, "me");
  }

  #[test]
  fn trailing_dot_has_an_empty_partial() {
    let prefix = attr_prefix("pkgs.", 5).expect("should parse");
    assert_eq!(prefix.base_expr, "pkgs");
    assert_eq!(prefix.partial, "");
    assert_eq!(prefix.span, Span::new(5, 5));
  }

  #[test]
  fn prefix_is_computed_only_up_to_the_cursor() {
    // Trailing text after `pos` (e.g. the rest of the line past the
    // cursor) must not leak into the token.
    let prefix = attr_prefix("pkgs.hel lo", 8).expect("should parse");
    assert_eq!(prefix.base_expr, "pkgs");
    assert_eq!(prefix.partial, "hel");
  }

  #[test]
  fn fresh_completer_has_no_in_flight_enrichment() {
    let mut completer = AttrCompleter::new(PathBuf::from("/nonexistent.sock"));
    assert_eq!(completer.poll_completion(), CompletionStatus::Idle);
  }

  #[test]
  fn completing_against_an_unreachable_daemon_yields_no_suggestions() {
    // No daemon is listening on this path, so `fetch_attrs` swallows the
    // connect failure and `complete` returns an empty, authoritative
    // result rather than hanging or panicking.
    let mut completer = AttrCompleter::new(PathBuf::from("/nonexistent.sock"));
    let result = completer.complete("pkgs.hel", 8);
    assert!(result.suggestions().is_empty());
  }
}
