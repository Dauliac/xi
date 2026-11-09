//! "Did you mean X?" typo suggestions for `ExprEvalResult::Error`s that
//! nix's own error reporter doesn't already cover.
//!
//! Nix itself suggests siblings for a missing attribute, but only when the
//! parent attrset is small enough - confirmed empirically: `{a=1;b=2;}.c`
//! gets "Did you mean one of a or b?", `builtins.notAFunction` does not
//! (`builtins` has ~200 entries, evidently past whatever size heuristic
//! nix's own reporter uses). It never suggests anything for an undefined
//! variable at all, since that is a lexical-scope lookup, not an
//! enumerable attrset the way a missing attribute is. [`did_you_mean`]
//! fills both gaps, reusing a shallow `ExprEval` the same way
//! `completion::AttrCompleter`'s base fetch does, and `xi-core::suggest`'s
//! `jaro_winkler`/similarity-threshold convention for a consistent feel
//! across the workspace.

use std::path::Path;

use strsim::jaro_winkler;
use xi_develop::daemon::{
  client_v3::{self, QueryOutcome},
  protocol_v3::{Availability, ExprEvalResult, FLAKE_SCOPE_EXPR, QueryReply},
};

/// Minimum Jaro-Winkler similarity to offer as a suggestion - same
/// threshold `xi_core::suggest` uses.
const SIMILARITY_THRESHOLD: f64 = 0.7;

/// Bound on how many names to suggest - more than a handful stops being a
/// "did you mean" hint and starts being a second attribute listing.
const MAX_SUGGESTIONS: usize = 3;

/// Build a "did you mean: ..." hint for `message` (an
/// [`ExprEvalResult::Error`]'s text) that resulted from evaluating `input`,
/// or `None` when the error isn't a shape this can help with, nix's own
/// message already covers it, or no candidate clears the similarity
/// threshold.
#[must_use]
pub fn did_you_mean(
  socket_path: &Path,
  input: &str,
  message: &str,
) -> Option<String> {
  if message.contains("Did you mean") {
    // Nix's own error reporter already offered a hint - do not pile a
    // second, redundant one on top.
    return None;
  }

  let (scope_expr, missing) = if let Some(name) =
    extract_quoted_between(message, "attribute '", "' missing")
  {
    // Reconstruct the parent expression by stripping the trailing
    // `.name` the user actually typed. If the missing segment isn't a
    // literal suffix of `input` (e.g. a middle segment of a longer
    // chain failed first), give up rather than guessing the wrong
    // scope.
    let suffix = format!(".{name}");
    let parent = input.strip_suffix(&suffix)?.to_owned();
    (parent, name)
  } else if let Some(name) =
    extract_quoted_between(message, "undefined variable '", "'")
  {
    (FLAKE_SCOPE_EXPR.to_owned(), name)
  } else {
    return None;
  };

  let mut candidates = fetch_candidate_names(socket_path, &scope_expr);
  if scope_expr == FLAKE_SCOPE_EXPR
    && !candidates.iter().any(|n| n == "builtins")
  {
    // The one bare identifier always in scope regardless of the flake's
    // own outputs - see `completion`'s identical injection for
    // Tab-completion at the top level.
    candidates.push("builtins".to_owned());
  }

  let mut scored: Vec<(String, f64)> = candidates
    .into_iter()
    .map(|name| {
      let score = jaro_winkler(&missing, &name);
      (name, score)
    })
    .filter(|(_, score)| *score >= SIMILARITY_THRESHOLD)
    .collect();
  scored.sort_by(|(_, a), (_, b)| {
    b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)
  });
  scored.truncate(MAX_SUGGESTIONS);

  if scored.is_empty() {
    return None;
  }

  let names: Vec<&str> = scored.iter().map(|(n, _)| n.as_str()).collect();
  Some(format!("xi repl: did you mean: {}?", names.join(", ")))
}

/// Extract the text between the first occurrence of `prefix` and the
/// following occurrence of `suffix` in `message`, e.g.
/// `extract_quoted_between(msg, "attribute '", "' missing")` for
/// `"error: attribute 'foo' missing\n..."` returns `Some("foo")`.
fn extract_quoted_between(
  message: &str,
  prefix: &str,
  suffix: &str,
) -> Option<String> {
  let start = message.find(prefix)? + prefix.len();
  let rest = &message[start..];
  let end = rest.find(suffix)?;
  Some(rest[..end].to_owned())
}

/// Fetch `scope_expr`'s shallow attribute names, swallowing every failure
/// mode (unreachable daemon, a non-`Attrs` reply, protocol mismatch) into
/// an empty list - same convention
/// `completion::AttrCompleter::fetch_attrs` uses; "no suggestions" is the
/// only honest answer when this fails.
fn fetch_candidate_names(socket_path: &Path, scope_expr: &str) -> Vec<String> {
  match client_v3::expr_eval(socket_path, scope_expr, true) {
    Ok(QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready {
      value: ExprEvalResult::Attrs(names),
    }))) => names,
    _ => Vec::new(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn extracts_a_missing_attribute_name() {
    let message = "error: attribute 'notAFunction' missing\n       at \
                   «string»:1:1:\n            1| builtins.notAFunction\n";
    assert_eq!(
      extract_quoted_between(message, "attribute '", "' missing"),
      Some("notAFunction".to_owned())
    );
  }

  #[test]
  fn extracts_an_undefined_variable_name() {
    let message = "error: undefined variable 'xyz'\n       at «string»:1:1:\n";
    assert_eq!(
      extract_quoted_between(message, "undefined variable '", "'"),
      Some("xyz".to_owned())
    );
  }

  #[test]
  fn no_hint_when_nix_already_suggested_one() {
    let message =
      "error: attribute 'c' missing\n       Did you mean one of a or b?";
    assert_eq!(
      did_you_mean(Path::new("/nonexistent.sock"), "{a=1;b=2;}.c", message),
      None
    );
  }

  #[test]
  fn no_hint_for_an_unreachable_daemon() {
    // fetch_candidate_names swallows the connect failure to an empty
    // list, so there is nothing to score against - a clean None, not a
    // panic or a stale guess.
    let message = "error: attribute 'notAFunction' missing\n";
    assert_eq!(
      did_you_mean(
        Path::new("/nonexistent.sock"),
        "builtins.notAFunction",
        message
      ),
      None
    );
  }

  #[test]
  fn no_hint_when_the_missing_name_is_not_a_literal_suffix() {
    // The missing segment was somewhere in the middle of a longer chain
    // that failed first - guessing a scope here would likely be wrong,
    // so this gives up rather than risk a misleading suggestion.
    let message = "error: attribute 'foo' missing\n";
    assert_eq!(
      did_you_mean(Path::new("/nonexistent.sock"), "foo.bar.baz", message),
      None
    );
  }

  #[test]
  fn no_hint_for_an_unrelated_error_shape() {
    let message = "error: syntax error, unexpected ')'\n";
    assert_eq!(
      did_you_mean(Path::new("/nonexistent.sock"), "(1 +)", message),
      None
    );
  }
}
