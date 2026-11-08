//! `.`/Tab-triggered attribute-path completion with a preview pane
//! (`openspec/changes/xi-repl/tasks.md` tasks 5.1-5.3).
//!
//! [`AttrCompleter`] implements `reedline`'s [`Completer`] trait. On each
//! completion request it:
//!
//! 1. Splits the typed token on its last `.` into a base expression and a
//!    partial attribute name. A token with no `.` yet (including an empty line,
//!    e.g. a bare Tab press before typing anything) uses [`FLAKE_SCOPE_EXPR`]
//!    as the base instead of giving up  -  the daemon answers that with the
//!    flake's own top-level output names (`packages`, `nixosConfigurations`,
//!    `checks`, ...), the same thing real `nix repl`'s `:lf .` loads into
//!    scope.
//! 2. Resolves the base expression's attribute names. The *first* request for a
//!    given base expression sends `ExprEval{expr: base, shallow: true}` to the
//!    daemon on a background thread, never on `complete()`'s caller (reedline's
//!    input-handling thread), because that round-trip goes through the same
//!    `builtins.getFlake`-backed path as any other expression and can take tens
//!    of seconds, uncached, on a large flake (see
//!    `v3_handlers::run_expr_eval`'s doc comment and
//!    `client_v3::EXPR_EVAL_TIMEOUT`'s 1-minute ceiling); blocking the input
//!    thread on it would freeze the whole editor, not just the completion menu.
//!    `complete()` waits up to [`INITIAL_FETCH_GRACE`] for that background
//!    thread's answer before giving up and returning empty - without this, a
//!    fetch that resolves in, say, half a second still flashes an empty "no
//!    records found" menu for that half second before the real one replaces it,
//!    which reads as broken even on a perfectly healthy fetch. Past the grace
//!    window [`Completer::poll_completion`] takes over, reporting `Ready` once
//!    the answer lands so reedline re-asks `complete()` for the now-populated
//!    list - this path still never blocks the editor beyond the bounded grace
//!    window, however long the fetch itself ends up taking. The raw
//!    (unfiltered) names are cached per base expression, so narrowing the
//!    partial by typing further characters re-filters that cache locally
//!    instead of re-querying the daemon on every keystroke.
//! 3. Enriches only a bounded window of the (locally filtered) candidates (task
//!    5.2) with a `meta.description`/version preview, one further shallow
//!    `ExprEval` per candidate, rendered through the same
//!    [`crate::render::render`] this crate already uses for top-level results
//!    (which itself defers to [`xi_core::nix_value::render_entry`] for
//!    derivations). This also runs on a background thread, feeding results back
//!    through an `mpsc` channel drained non-blockingly from `poll_completion`
//!    (task 5.3)  -  the same thread+channel shape `nix-command`'s
//!    `read_pipe`/`PipeEvent` uses for streaming subprocess output, applied
//!    here to streaming daemon round-trips instead.
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
  protocol_v3::{
    Availability,
    DocResult,
    ExprEvalResult,
    FLAKE_SCOPE_EXPR,
    QueryReply,
  },
};

/// Upper bound on how many candidates get enriched per completion request.
///
/// See the module doc comment's "API limitation" section  -  this stands in
/// for "only the candidates currently visible in the dropdown" (task 5.2),
/// which `reedline`'s `Completer` trait has no way to learn exactly.
const ENRICH_WINDOW: usize = 12;

/// How long [`Completer::complete`] waits, synchronously but boundedly, for
/// a brand-new base fetch to land before giving up and returning an empty
/// result for [`Completer::poll_completion`] to settle later.
///
/// Chosen to comfortably cover a warm `git+file://`-backed fetch against a
/// real multi-crate flake (observed ~1-2s after the `path:` → `git+file://`
/// fix) without reintroducing an unbounded block: a fetch slower than this
/// still never freezes the editor, it just falls through to the async path
/// one keystroke later than a fetch that lands inside the window.
const INITIAL_FETCH_GRACE: std::time::Duration =
  std::time::Duration::from_millis(800);

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
/// press on an otherwise-empty line), `base_expr` comes back as
/// [`FLAKE_SCOPE_EXPR`] - the sentinel that asks the daemon for the
/// flake's own top-level output names (`packages`, `nixosConfigurations`,
/// `checks`, ...), the same thing real `nix repl`'s `:lf .` loads into
/// scope. `partial` is then whatever's typed so far, to filter it
/// locally.
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
      base_expr: FLAKE_SCOPE_EXPR.to_owned(),
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
/// for it  -  initially empty (while the base fetch is in flight) or
/// unenriched, filled in with preview text as [`EnrichMsg::Item`]s arrive.
struct RequestState {
  request_id:  u64,
  base_expr:   String,
  partial:     String,
  span:        Span,
  suggestions: Vec<Suggestion>,
}

/// The raw (unfiltered by `partial`) attribute names last fetched for a
/// given base expression, kept so narrowing the partial by typing further
/// characters re-filters locally instead of re-querying the daemon - see
/// the module doc comment, point 2.
struct BaseCache {
  base_expr:  String,
  span_start: usize,
  names:      Vec<String>,
}

/// A message from a background thread (base-fetch or enrichment) back to
/// [`AttrCompleter::poll_completion`].
enum EnrichMsg {
  /// The base expression's raw attribute names are ready - unfiltered by
  /// `partial`, which may have kept changing while the fetch was in flight.
  Base {
    request_id: u64,
    names:      Vec<String>,
  },
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
  base_cache:           Option<BaseCache>,
  rx:                   Option<mpsc::Receiver<EnrichMsg>>,
  base_in_flight:       bool,
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
      base_cache: None,
      rx: None,
      base_in_flight: false,
      enrichment_in_flight: false,
      next_request_id: 0,
    }
  }

  /// Fetch `base_expr`'s shallow attribute names from the daemon.
  ///
  /// Swallows every failure mode (unreachable daemon, a non-`Attrs` reply
  /// because `base_expr` isn't an attrset, a protocol mismatch) into an
  /// empty candidate list: the caller (a background thread; see
  /// [`AttrCompleter::spawn_base_fetch`]) has no error channel back to the
  /// editor either, so "no completions right now" is the only honest answer
  /// to give back, the same convention `reedline`'s own `HistoryCompleter`
  /// uses.
  fn fetch_attrs(
    socket_path: &std::path::Path,
    base_expr: &str,
  ) -> Vec<String> {
    match client_v3::expr_eval(socket_path, base_expr, true) {
      Ok(QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready {
        value: ExprEvalResult::Attrs(names),
      }))) => names,
      _ => Vec::new(),
    }
  }

  /// Spawn the background thread that resolves `base_expr`'s raw attribute
  /// names, replacing (and thereby abandoning) any base fetch or enrichment
  /// still running for a previous request - same cancellation-by-dropped-`rx`
  /// mechanism [`AttrCompleter::spawn_enrichment`] uses, see its doc comment.
  ///
  /// This is the call that can take the better part of a minute on a large,
  /// uncached flake (see the module doc comment, point 2); running it here
  /// instead of inline in [`Completer::complete`] is the whole point - the
  /// editor's input thread calls `complete()` and must get control back
  /// immediately, or every keystroke after a `.`/Tab would freeze the
  /// terminal until the daemon answers.
  fn spawn_base_fetch(&mut self, request_id: u64, base_expr: String) {
    let (tx, rx) = mpsc::channel();
    self.rx = Some(rx);
    self.base_in_flight = true;

    let socket_path = self.socket_path.clone();
    thread::spawn(move || {
      let mut names = Self::fetch_attrs(&socket_path, &base_expr);
      // At the top level, also offer `builtins` itself - the one bare
      // identifier every Nix expression has in scope regardless of the
      // flake's own outputs, and the gateway to completing Nix's actual
      // *functions* (`builtins.map`, `builtins.toString`, ...) rather than
      // only flake data. Dotting into it from here already works through
      // the ordinary (non-`FLAKE_SCOPE_EXPR`) completion path, since typed
      // text like `builtins.to` is just an ordinary expression by then.
      if base_expr == FLAKE_SCOPE_EXPR && !names.iter().any(|n| n == "builtins")
      {
        names.push("builtins".to_owned());
      }
      let _ = tx.send(EnrichMsg::Base { request_id, names });
    });
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
        // When completing at the top level, `name` (e.g. "packages") is
        // already a complete, directly-evaluable expression on its own
        // once bound via the daemon's `with`-flake wrapper - prefixing it
        // with the `FLAKE_SCOPE_EXPR` sentinel itself would send nonsense
        // like `__xi_repl_flake_scope__.packages`.
        let full_path = if base_expr == FLAKE_SCOPE_EXPR {
          name.clone()
        } else {
          format!("{base_expr}.{name}")
        };
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
            if matches!(value, ExprEvalResult::Lambda) {
              // A plain "<lambda>" placeholder is a dead end for exactly
              // the candidates a user most wants documentation for -
              // nix's own functions. Try real `nix repl`-backed docs
              // first (see Query::ExprDoc's doc comment); fall back to
              // the placeholder only when there genuinely are none.
              doc_preview_or_fallback(&socket_path, &full_path, &value)
            } else {
              crate::render::strip_ansi(&crate::render::render(&value))
            }
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

/// Filter `names` to those starting with `partial`, sort, and turn them into
/// `reedline` [`Suggestion`]s anchored at `span`.
fn filtered_suggestions(
  names: &[String],
  partial: &str,
  span: Span,
) -> (Vec<String>, Vec<Suggestion>) {
  let mut filtered: Vec<String> = names
    .iter()
    .filter(|n| n.starts_with(partial))
    .cloned()
    .collect();
  filtered.sort();

  let suggestions = filtered
    .iter()
    .map(|name| {
      Suggestion {
        value: name.clone(),
        span,
        append_whitespace: false,
        ..Default::default()
      }
    })
    .collect();

  (filtered, suggestions)
}

/// Fetch real documentation for `expr` (a lambda-classified candidate) via
/// `Query::ExprDoc`, falling back to the ordinary placeholder render of
/// `fallback` (the already-known [`ExprEvalResult::Lambda`]) when there is
/// none, the lookup errors, or the daemon is unreachable.
///
/// Called from the enrichment background thread, same as the ordinary
/// classify-and-render path it supplements - never on `complete()`'s
/// caller.
fn doc_preview_or_fallback(
  socket_path: &std::path::Path,
  expr: &str,
  fallback: &ExprEvalResult,
) -> String {
  let doc = client_v3::expr_doc(socket_path, expr)
    .ok()
    .and_then(|outcome| {
      match outcome {
        QueryOutcome::V3(QueryReply::ExprDoc(Availability::Ready {
          value: DocResult::Found { doc },
        })) => Some(doc),
        _ => None,
      }
    });

  doc.unwrap_or_else(|| {
    crate::render::strip_ansi(&crate::render::render(fallback))
  })
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

    // A base fetch for this exact attribute segment is already in flight
    // (an earlier keystroke in this same segment kicked it off and it
    // hasn't landed yet): don't spawn a second `nix eval` subprocess on top
    // of it, just update what we'll filter by once the one already running
    // answers. Without this check, typing several characters while a slow
    // (uncached, tens-of-seconds) fetch is still in flight for a large
    // flake piles up one redundant subprocess per keystroke, each paying
    // the full cost again for an answer that gets thrown away.
    let same_segment_already_fetching = self.base_in_flight
      && self.current.as_ref().is_some_and(|c| {
        c.base_expr == prefix.base_expr && c.span.start == prefix.span.start
      });
    if same_segment_already_fetching {
      if let Some(current) = &mut self.current {
        current.partial = prefix.partial;
        current.span = prefix.span;
      }
      return CompletionResult::fresh(Vec::<Suggestion>::new());
    }

    self.next_request_id += 1;
    let request_id = self.next_request_id;

    // Cache hit: the raw names for this exact attribute segment are already
    // known (an earlier keystroke in this same segment fetched them), so
    // just re-filter locally by the new partial - no daemon round-trip.
    let cached_names = self
      .base_cache
      .as_ref()
      .filter(|c| {
        c.base_expr == prefix.base_expr && c.span_start == prefix.span.start
      })
      .map(|c| c.names.clone());

    if let Some(names) = cached_names {
      let (filtered, suggestions) =
        filtered_suggestions(&names, &prefix.partial, prefix.span);
      self.spawn_enrichment(request_id, prefix.base_expr.clone(), &filtered);
      self.current = Some(RequestState {
        request_id,
        base_expr: prefix.base_expr,
        partial: prefix.partial,
        span: prefix.span,
        suggestions: suggestions.clone(),
      });
      return CompletionResult::fresh(suggestions);
    }

    // Cache miss: a brand-new attribute segment. Resolve it off-thread (see
    // `spawn_base_fetch`'s doc comment for why).
    self.current = Some(RequestState {
      request_id,
      base_expr: prefix.base_expr.clone(),
      partial: prefix.partial.clone(),
      span: prefix.span,
      suggestions: Vec::new(),
    });
    self.spawn_base_fetch(request_id, prefix.base_expr.clone());

    // Give the fetch a short, bounded grace window before falling through
    // to the empty/async path - see INITIAL_FETCH_GRACE's doc comment for
    // why. A fetch that lands within the window skips the "no records
    // found" flash entirely.
    if let Some(rx) = &self.rx
      && let Ok(EnrichMsg::Base {
        request_id: landed_id,
        names,
      }) = rx.recv_timeout(INITIAL_FETCH_GRACE)
      && landed_id == request_id
    {
      let (filtered, suggestions) =
        filtered_suggestions(&names, &prefix.partial, prefix.span);
      self.base_cache = Some(BaseCache {
        base_expr: prefix.base_expr.clone(),
        span_start: prefix.span.start,
        names,
      });
      self.base_in_flight = false;
      self.spawn_enrichment(request_id, prefix.base_expr, &filtered);
      if let Some(current) = &mut self.current {
        current.suggestions.clone_from(&suggestions);
      }
      return CompletionResult::fresh(suggestions);
    }

    CompletionResult::fresh(Vec::<Suggestion>::new())
  }

  fn poll_completion(&mut self) -> CompletionStatus {
    let mut updated = false;
    // Deferred out of the loop below because spawning enrichment needs
    // `&mut self` as a whole, which would conflict with the `&self.rx`
    // borrow the loop holds for its duration.
    let mut pending_enrich: Option<(u64, String, Vec<String>)> = None;

    if let Some(rx) = &self.rx {
      loop {
        match rx.try_recv() {
          Ok(EnrichMsg::Base { request_id, names }) => {
            if let Some(current) = &mut self.current
              && current.request_id == request_id
            {
              let (filtered, suggestions) =
                filtered_suggestions(&names, &current.partial, current.span);
              self.base_cache = Some(BaseCache {
                base_expr: current.base_expr.clone(),
                span_start: current.span.start,
                names,
              });
              current.suggestions = suggestions;
              pending_enrich =
                Some((request_id, current.base_expr.clone(), filtered));
              updated = true;
            }
            self.base_in_flight = false;
            // A fetch for a request we've since moved past: discard - its
            // answer is stale and nothing downstream is waiting on it.
          },
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
            self.base_in_flight = false;
            self.enrichment_in_flight = false;
            break;
          },
        }
      }
    }

    if let Some((request_id, base_expr, names)) = pending_enrich {
      self.spawn_enrichment(request_id, base_expr, &names);
    }

    if updated {
      CompletionStatus::Ready
    } else if self.base_in_flight || self.enrichment_in_flight {
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
  fn no_dot_completes_against_flake_scope() {
    let prefix = attr_prefix("hel", 3).expect("should complete against scope");
    assert_eq!(prefix.base_expr, FLAKE_SCOPE_EXPR);
    assert_eq!(prefix.partial, "hel");
    assert_eq!(prefix.span, Span::new(0, 3));
  }

  #[test]
  fn bare_tab_on_an_empty_line_completes_against_flake_scope() {
    let prefix = attr_prefix("", 0).expect("should complete against scope");
    assert_eq!(prefix.base_expr, FLAKE_SCOPE_EXPR);
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
    // No daemon is listening on this path, so the background base-fetch
    // thread's connect fails immediately and `complete` returns an empty
    // result right away - not hanging, not panicking.
    let mut completer = AttrCompleter::new(PathBuf::from("/nonexistent.sock"));
    let result = completer.complete("pkgs.hel", 8);
    assert!(result.suggestions().is_empty());
  }

  /// A fake v3 daemon that answers exactly one `ExprEval` query after a
  /// deliberate delay, to prove `complete()` does not block on it.
  ///
  /// Mirrors `tests/bdd_repl_happy_path.rs`'s `MiniDaemon`, duplicated here
  /// (rather than shared) because `completion` is a private module  -  an
  /// integration test in `tests/` cannot reach `AttrCompleter` to drive this
  /// scenario, so the test has to live in this module instead.
  struct SlowMiniDaemon {
    socket:  PathBuf,
    _handle: thread::JoinHandle<()>,
    _tmp:    tempfile::TempDir,
  }

  impl SlowMiniDaemon {
    fn spawn(delay: std::time::Duration, names: Vec<String>) -> Self {
      use std::{
        io::{BufReader, BufWriter},
        os::unix::net::UnixListener,
      };

      use xi_develop::daemon::{
        protocol::{read_message, write_message},
        protocol_v3::{Envelope, Payload, Query, Reply},
      };

      let tmp = tempfile::tempdir().expect("mktemp");
      let socket = tmp.path().join("v3.sock");
      let listener = UnixListener::bind(&socket).expect("bind");

      let (ready_tx, ready_rx) = mpsc::channel::<()>();
      let handle = thread::spawn(move || {
        ready_tx.send(()).ok();
        let Ok((stream, _)) = listener.accept() else {
          return;
        };
        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);
        let Ok(env) = read_message::<_, Envelope>(&mut reader) else {
          return;
        };
        let Payload::Query(Query::ExprEval { .. }) = env.payload else {
          return;
        };
        thread::sleep(delay);
        let reply = Envelope::new(
          env.id,
          Payload::Reply(Reply::Query(QueryReply::ExprEval(
            Availability::Ready {
              value: ExprEvalResult::Attrs(names),
            },
          ))),
        );
        let _ = write_message(&mut writer, &reply);
      });
      ready_rx.recv().expect("listener ready");

      Self {
        socket,
        _handle: handle,
        _tmp: tmp,
      }
    }
  }

  #[test]
  fn complete_does_not_block_on_a_slow_daemon() {
    // Comfortably past INITIAL_FETCH_GRACE, so the grace window itself
    // elapses and this exercises the fully-async fallback path.
    let slow_delay = INITIAL_FETCH_GRACE + std::time::Duration::from_secs(1);
    let daemon = SlowMiniDaemon::spawn(slow_delay, vec!["hello".to_owned()]);
    let mut completer = AttrCompleter::new(daemon.socket);

    let started = std::time::Instant::now();
    let first = completer.complete("pkgs.hel", 8);
    // The whole point of backgrounding the base fetch: `complete()` returns
    // once the bounded grace window elapses, long before the fake daemon's
    // much longer artificial delay does.
    assert!(started.elapsed() < slow_delay);
    assert!(first.suggestions().is_empty());
    assert_eq!(completer.poll_completion(), CompletionStatus::Pending);

    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
      let status = completer.poll_completion();
      if status == CompletionStatus::Ready {
        break;
      }
      assert!(
        std::time::Instant::now() < deadline,
        "daemon reply never arrived"
      );
      thread::sleep(std::time::Duration::from_millis(10));
    }

    let settled = completer.complete("pkgs.hel", 8);
    let names: Vec<_> = settled
      .suggestions()
      .iter()
      .map(|s| s.value.clone())
      .collect();
    assert_eq!(names, vec!["hello".to_owned()]);
  }

  #[test]
  fn complete_returns_populated_suggestions_within_the_grace_window() {
    // Well under INITIAL_FETCH_GRACE, so `complete()`'s own bounded wait
    // should catch the answer and return it directly - no "no records
    // found" flash, no need to poll.
    let daemon =
      SlowMiniDaemon::spawn(std::time::Duration::from_millis(50), vec![
        "hello".to_owned(),
      ]);
    let mut completer = AttrCompleter::new(daemon.socket);

    let started = std::time::Instant::now();
    let first = completer.complete("pkgs.hel", 8);
    assert!(started.elapsed() < INITIAL_FETCH_GRACE);
    let names: Vec<_> = first
      .suggestions()
      .iter()
      .map(|s| s.value.clone())
      .collect();
    assert_eq!(names, vec!["hello".to_owned()]);
  }

  #[test]
  fn top_level_completion_always_offers_builtins() {
    // The daemon only ever hands back the flake's own outputs
    // ("packages") - `builtins` must still show up, merged in client-side,
    // so completing Nix's own functions (`builtins.map`, ...) is
    // discoverable from an empty prompt, not just flake data.
    let daemon =
      SlowMiniDaemon::spawn(std::time::Duration::from_millis(10), vec![
        "packages".to_owned(),
      ]);
    let mut completer = AttrCompleter::new(daemon.socket);

    let result = completer.complete("", 0);
    let names: Vec<_> = result
      .suggestions()
      .iter()
      .map(|s| s.value.clone())
      .collect();
    assert_eq!(names, vec!["builtins".to_owned(), "packages".to_owned()]);
  }

  /// A fake v3 daemon that counts how many connections it accepts while
  /// otherwise behaving like [`SlowMiniDaemon`] - used to prove a run of
  /// keystrokes against the same still-pending attribute segment triggers
  /// exactly one fetch, not one per keystroke.
  struct CountingSlowMiniDaemon {
    socket:      PathBuf,
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    _handle:     thread::JoinHandle<()>,
    _tmp:        tempfile::TempDir,
  }

  impl CountingSlowMiniDaemon {
    fn spawn(delay: std::time::Duration, names: Vec<String>) -> Self {
      use std::{
        io::{BufReader, BufWriter},
        os::unix::net::UnixListener,
        sync::{Arc, atomic::AtomicUsize},
      };

      use xi_develop::daemon::{
        protocol::{read_message, write_message},
        protocol_v3::{Envelope, Payload, Query, Reply},
      };

      let tmp = tempfile::tempdir().expect("mktemp");
      let socket = tmp.path().join("v3.sock");
      let listener = UnixListener::bind(&socket).expect("bind");
      let connections = Arc::new(AtomicUsize::new(0));

      let (ready_tx, ready_rx) = mpsc::channel::<()>();
      let handle = thread::spawn({
        let connections = Arc::clone(&connections);
        move || {
          ready_tx.send(()).ok();
          let Ok((stream, _)) = listener.accept() else {
            return;
          };
          connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
          let mut reader = BufReader::new(&stream);
          let mut writer = BufWriter::new(&stream);
          let Ok(env) = read_message::<_, Envelope>(&mut reader) else {
            return;
          };
          let Payload::Query(Query::ExprEval { .. }) = env.payload else {
            return;
          };
          thread::sleep(delay);
          let reply = Envelope::new(
            env.id,
            Payload::Reply(Reply::Query(QueryReply::ExprEval(
              Availability::Ready {
                value: ExprEvalResult::Attrs(names),
              },
            ))),
          );
          let _ = write_message(&mut writer, &reply);
        }
      });
      ready_rx.recv().expect("listener ready");

      Self {
        socket,
        connections,
        _handle: handle,
        _tmp: tmp,
      }
    }
  }

  #[test]
  fn typing_within_a_still_pending_segment_fetches_only_once() {
    // Comfortably past INITIAL_FETCH_GRACE: the first `complete()` call's
    // own bounded wait must time out and fall through to the async path,
    // otherwise the fetch lands before the second/third keystrokes even
    // happen and the dedup branch under test never gets exercised.
    let slow_delay = INITIAL_FETCH_GRACE + std::time::Duration::from_secs(1);
    let daemon =
      CountingSlowMiniDaemon::spawn(slow_delay, vec!["hello".to_owned()]);
    let mut completer = AttrCompleter::new(daemon.socket);

    // Three keystrokes narrowing the same segment ("h", "he", "hel"), all
    // typed before the fake daemon's artificial delay elapses.
    completer.complete("pkgs.h", 6);
    completer.complete("pkgs.he", 7);
    let third = completer.complete("pkgs.hel", 8);
    assert!(third.suggestions().is_empty());

    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(5);
    while completer.poll_completion() != CompletionStatus::Ready {
      assert!(
        std::time::Instant::now() < deadline,
        "daemon reply never arrived"
      );
      thread::sleep(std::time::Duration::from_millis(10));
    }

    assert_eq!(
      daemon.connections.load(std::sync::atomic::Ordering::SeqCst),
      1,
      "typing within one still-pending segment must not fetch more than once"
    );

    // The final answer reflects the latest-typed partial ("hel"), not a
    // stale one from whichever keystroke actually triggered the fetch.
    let settled = completer.complete("pkgs.hel", 8);
    let names: Vec<_> = settled
      .suggestions()
      .iter()
      .map(|s| s.value.clone())
      .collect();
    assert_eq!(names, vec!["hello".to_owned()]);
  }
}
