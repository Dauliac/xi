//! Value rendering: one colored line per [`ExprEvalResult`] kind (Phase 0),
//! plus Phase 2's structured tree view for attrset/list `Scalar` results
//! (`openspec/changes/xi-repl/tasks.md` task 6.2). The `Derivation` case
//! (task 6.3) and any derivation-shaped leaf found while walking a tree
//! (task 6.2's "using the shared renderer ... for any derivation-valued
//! leaf") both reuse [`xi_core::nix_value::render_entry`], so a package
//! printed by `xi repl`  -  top-level or nested  -  and one printed by
//! `xi flake show` agree on how the name/version/`[dirty]` line looks.
//!
//! ## Tree rendering scope: non-interactive
//!
//! Task 6.2 names "expand/collapse" as a nice-to-have, explicitly allowing
//! a non-interactive nested/indented view as acceptable Phase 2 scope. This
//! module builds the latter: [`render_tree`] prints the whole structure at
//! once, fully expanded, with 2-space indentation per nesting level. There
//! is no fold/collapse interaction  -  by the time [`render`] is called, the
//! `reedline` prompt has already returned control for the *next* line, so
//! there is no live widget left to toggle against; building one would mean
//! a second interactive render loop over the printed result, which is out
//! of scope here.

use std::fmt::Write as _;

use xi_core::nix_value::{self, VersionInfo};
use xi_develop::daemon::protocol_v3::ExprEvalResult;
use yansi::{Color, Paint};

/// Render one [`ExprEvalResult`] as a single colored line, ready to print.
#[must_use]
pub fn render(result: &ExprEvalResult) -> String {
  match result {
    ExprEvalResult::Attrs(names) => {
      format!("{}", Paint::new(names.join(", ")).fg(Color::Cyan))
    },
    ExprEvalResult::Derivation {
      name,
      version,
      description,
    } => {
      render_derivation(
        name.as_deref(),
        version.as_deref(),
        description.as_deref(),
      )
    },
    ExprEvalResult::Scalar(value) => render_scalar(value),
    ExprEvalResult::Lambda => {
      format!("{}", Paint::new("<lambda>").fg(Color::Magenta))
    },
    ExprEvalResult::Thunk => {
      format!("{}", Paint::new("<thunk>").fg(Color::Yellow))
    },
    ExprEvalResult::Unrepresentable => {
      format!(
        "{}",
        Paint::new("<unrepresentable: contains a function>").fg(Color::Yellow)
      )
    },
    ExprEvalResult::Error { message } => {
      format!("{}", Paint::new(message).fg(Color::Red))
    },
  }
}

fn render_derivation(
  name: Option<&str>,
  version: Option<&str>,
  description: Option<&str>,
) -> String {
  let name = name.unwrap_or("<unnamed derivation>");
  let version_info = version.map(|v| {
    VersionInfo {
      version: v,
      dirty:   false,
    }
  });
  let mut line = nix_value::render_entry(name, version_info.as_ref(), false);
  if let Some(desc) = description {
    let _ = write!(line, "  -  {}", Paint::new(desc).dim());
  }
  line
}

fn scalar_text(value: &serde_json::Value) -> String {
  match value {
    serde_json::Value::String(s) => s.clone(),
    other => other.to_string(),
  }
}

/// Render an `ExprEvalResult::Scalar`'s JSON payload: a non-empty attrset
/// or list goes through [`render_tree`]'s structured view (task 6.2); every
/// other shape (string, number, bool, null, and the degenerate empty
/// `{}`/`[]`, which `render_tree` itself renders as one-liners) keeps the
/// existing flat single-line rendering so Phase 0's established behavior
/// (e.g. a bare string result prints unquoted) does not change underfoot.
fn render_scalar(value: &serde_json::Value) -> String {
  match value {
    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
      render_tree(value, 0)
    },
    _ => format!("{}", Paint::new(scalar_text(value)).fg(Color::Green)),
  }
}

/// Render `value` as a colorized, indented tree at nesting depth `level`
/// (`level` counts how many enclosing attrsets/lists this value sits
/// inside; the value's own closing delimiter is indented to `level`, its
/// children to `level + 1`  -  see the module doc comment for why this
/// stays non-interactive).
fn render_tree(value: &serde_json::Value, level: usize) -> String {
  match value {
    serde_json::Value::Object(map) => render_object(map, level),
    serde_json::Value::Array(items) => render_array(items, level),
    serde_json::Value::String(s) => {
      format!("{}", Paint::new(format!("{s:?}")).fg(Color::Green))
    },
    serde_json::Value::Number(n) => {
      format!("{}", Paint::new(n.to_string()).fg(Color::Yellow))
    },
    // `true`/`false`/`null` are plain bound identifiers in Nix (not
    // keywords  -  see `crate::highlight`'s doc comment), so they share
    // the identifier-ish blue used for derivation/package names rather
    // than getting a color of their own.
    serde_json::Value::Bool(b) => {
      format!("{}", Paint::new(b.to_string()).fg(Color::Blue))
    },
    serde_json::Value::Null => {
      format!("{}", Paint::new("null").fg(Color::Blue))
    },
  }
}

/// Render a JSON object as a brace-delimited tree, unless it carries Nix's
/// `type = "derivation"` marker (the same convention
/// `xi_develop`'s `classify_wrapper` checks server-side for a *top-level*
/// result)  -  in which case it is rendered as one leaf line via
/// [`render_derivation`] instead of walked further.
///
/// A real derivation's attrset generally cannot survive this deep as plain
/// JSON (it carries function-valued attrs like `overrideAttrs`, which is
/// exactly why the daemon classifies a *top-level* derivation specially
/// before ever forcing full JSON serialization - see
/// `v3_handlers::classify_wrapper`). This still handles a derivation-shaped
/// leaf showing up nested: a JSON-safe subset of one (e.g. after
/// `removeAttrs`), or a future daemon revision that pre-flattens nested
/// derivations the same way.
fn render_object(
  map: &serde_json::Map<String, serde_json::Value>,
  level: usize,
) -> String {
  if let Some(leaf) = derivation_leaf(map) {
    return leaf;
  }
  if map.is_empty() {
    return format!("{}", Paint::new("{ }").fg(Color::Cyan));
  }

  let indent = "  ".repeat(level + 1);
  let close_indent = "  ".repeat(level);
  let mut out = format!("{}\n", Paint::new("{").fg(Color::Cyan));
  for (key, val) in map {
    let _ = writeln!(
      out,
      "{indent}{} {} {};",
      Paint::new(key).fg(Color::Blue),
      Paint::new("=").fg(Color::Cyan),
      render_tree(val, level + 1)
    );
  }
  let _ = write!(out, "{close_indent}{}", Paint::new("}").fg(Color::Cyan));
  out
}

/// Render a JSON array as a bracket-delimited tree, one element per line
/// (no separators between elements, matching Nix list syntax rather than
/// JSON's commas).
fn render_array(items: &[serde_json::Value], level: usize) -> String {
  if items.is_empty() {
    return format!("{}", Paint::new("[ ]").fg(Color::Cyan));
  }

  let indent = "  ".repeat(level + 1);
  let close_indent = "  ".repeat(level);
  let mut out = format!("{}\n", Paint::new("[").fg(Color::Cyan));
  for item in items {
    let _ = writeln!(out, "{indent}{}", render_tree(item, level + 1));
  }
  let _ = write!(out, "{close_indent}{}", Paint::new("]").fg(Color::Cyan));
  out
}

/// If `map` carries Nix's `type = "derivation"` marker, render it as one
/// leaf line via [`render_derivation`] (which defers to
/// [`xi_core::nix_value::render_entry`])  -  `None` for every other
/// attrset, which [`render_object`] then walks normally.
fn derivation_leaf(
  map: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
  if map.get("type").and_then(serde_json::Value::as_str) != Some("derivation") {
    return None;
  }
  let name = map.get("name").and_then(serde_json::Value::as_str);
  let version = map.get("version").and_then(serde_json::Value::as_str);
  let description = map
    .get("meta")
    .and_then(serde_json::Value::as_object)
    .and_then(|meta| meta.get("description"))
    .and_then(serde_json::Value::as_str);
  Some(render_derivation(name, version, description))
}

/// Strip ANSI escape sequences (CSI codes, e.g. the `yansi` coloring
/// [`render`] applies) back out of a rendered string.
///
/// Used by [`crate::completion`] before handing [`render`]'s output to
/// `reedline` as a `Suggestion::description`: `reedline`'s `IdeMenu`
/// measures description text with `unicode_width`, which has no notion of
/// ANSI escapes and would miscount their bytes as visible columns,
/// corrupting the preview pane's wrapping and padding. Stripping keeps the
/// shared renderer's *text* for the preview without its color.
#[must_use]
pub(crate) fn strip_ansi(input: &str) -> String {
  let mut out = String::with_capacity(input.len());
  let mut chars = input.chars();
  while let Some(ch) = chars.next() {
    if ch != '\u{1b}' {
      out.push(ch);
      continue;
    }
    // Only a `CSI` sequence (`ESC` `[` ... final byte in 0x40..=0x7E, `m`
    // for the SGR codes `yansi` emits) is swallowed whole. A lone `ESC`
    // not followed by `[` is not a sequence this renderer ever produces,
    // but passes the following char through rather than dropping it.
    match chars.next() {
      Some('[') => {
        for c in chars.by_ref() {
          if ('\u{40}'..='\u{7e}').contains(&c) {
            break;
          }
        }
      },
      Some(other) => out.push(other),
      None => {},
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn attrs_are_cyan_and_comma_joined() {
    let rendered = render(&ExprEvalResult::Attrs(vec!["a".into(), "b".into()]));
    assert!(rendered.contains("a, b"));
  }

  #[test]
  fn derivation_falls_back_to_placeholder_name() {
    let rendered = render(&ExprEvalResult::Derivation {
      name:        None,
      version:     None,
      description: None,
    });
    assert!(rendered.contains("<unnamed derivation>"));
  }

  #[test]
  fn derivation_includes_description() {
    let rendered = render(&ExprEvalResult::Derivation {
      name:        Some("hello".into()),
      version:     Some("2.12".into()),
      description: Some("a friendly program".into()),
    });
    assert!(rendered.contains("hello"));
    assert!(rendered.contains("2.12"));
    assert!(rendered.contains("a friendly program"));
  }

  #[test]
  fn scalar_string_is_unquoted() {
    let rendered = render(&ExprEvalResult::Scalar(serde_json::json!("plain")));
    assert!(rendered.contains("plain"));
    assert!(!rendered.contains('"'));
  }

  #[test]
  fn scalar_number_round_trips() {
    let rendered = render(&ExprEvalResult::Scalar(serde_json::json!(42)));
    assert!(rendered.contains('4'));
  }

  #[test]
  fn empty_attrset_is_a_one_liner() {
    let rendered = render(&ExprEvalResult::Scalar(serde_json::json!({})));
    assert_eq!(strip_ansi(&rendered), "{ }");
  }

  #[test]
  fn empty_list_is_a_one_liner() {
    let rendered = render(&ExprEvalResult::Scalar(serde_json::json!([])));
    assert_eq!(strip_ansi(&rendered), "[ ]");
  }

  #[test]
  fn nonempty_attrset_renders_as_an_indented_tree() {
    let rendered = render(&ExprEvalResult::Scalar(
      serde_json::json!({ "a": 1, "b": "two" }),
    ));
    let stripped = strip_ansi(&rendered);
    assert_eq!(
      stripped, "{\n  a = 1;\n  b = \"two\";\n}",
      "keys print in sorted order (serde_json's default BTreeMap), one per \
       indented line, strings quoted unlike the flat top-level case"
    );
  }

  #[test]
  fn nested_attrset_indents_one_level_deeper_and_aligns_its_closing_brace() {
    let rendered = render(&ExprEvalResult::Scalar(serde_json::json!({
      "outer": { "inner": 1 },
    })));
    let stripped = strip_ansi(&rendered);
    assert_eq!(
      stripped, "{\n  outer = {\n    inner = 1;\n  };\n}",
      "the nested closing brace should align under `outer`'s indent, not \
       under the root's"
    );
  }

  #[test]
  fn list_renders_one_element_per_line_with_no_separators() {
    let rendered =
      render(&ExprEvalResult::Scalar(serde_json::json!([1, 2, 3])));
    let stripped = strip_ansi(&rendered);
    assert_eq!(stripped, "[\n  1\n  2\n  3\n]");
  }

  #[test]
  fn derivation_shaped_leaf_within_a_tree_uses_the_shared_renderer() {
    let rendered = render(&ExprEvalResult::Scalar(serde_json::json!({
      "pkg": {
        "type": "derivation",
        "name": "hello",
        "version": "2.12",
        "meta": { "description": "a friendly program" },
      },
    })));
    let stripped = strip_ansi(&rendered);
    assert!(stripped.contains("hello"));
    assert!(stripped.contains("2.12"));
    assert!(stripped.contains("a friendly program"));
    // Rendered via the shared renderer, not dumped as a raw attrset: the
    // JSON-only field names must not leak into the printed line.
    assert!(!stripped.contains("type"));
    assert!(!stripped.contains("derivation"));
  }

  #[test]
  fn derivation_shaped_value_at_the_top_level_also_uses_the_shared_renderer() {
    // Defensive: a `Scalar` whose JSON *is itself* derivation-shaped (not
    // nested) should not print as a raw attrset either, even though this
    // path is normally pre-empted by the daemon's own `Derivation`
    // classification for a genuine top-level derivation.
    let rendered = render(&ExprEvalResult::Scalar(serde_json::json!({
      "type": "derivation",
      "name": "hello",
      "version": "2.12",
    })));
    let stripped = strip_ansi(&rendered);
    assert!(stripped.contains("hello"));
    assert!(stripped.contains("2.12"));
    assert!(!stripped.contains('{'));
  }

  #[test]
  fn lambda_and_thunk_have_distinct_placeholders() {
    assert!(render(&ExprEvalResult::Lambda).contains("<lambda>"));
    assert!(render(&ExprEvalResult::Thunk).contains("<thunk>"));
  }

  #[test]
  fn error_message_is_preserved() {
    let rendered = render(&ExprEvalResult::Error {
      message: "undefined variable 'foo'".into(),
    });
    assert!(rendered.contains("undefined variable 'foo'"));
  }

  #[test]
  fn strip_ansi_removes_real_render_output_escapes() {
    let rendered = render(&ExprEvalResult::Attrs(vec!["a".into(), "b".into()]));
    let stripped = strip_ansi(&rendered);
    assert_eq!(stripped, "a, b");
  }

  #[test]
  fn strip_ansi_is_a_no_op_on_plain_text() {
    assert_eq!(strip_ansi("plain text, no codes"), "plain text, no codes");
  }

  #[test]
  fn strip_ansi_handles_multiple_sgr_sequences_in_one_string() {
    let colored =
      format!("{}", yansi::Paint::new("hello").fg(yansi::Color::Blue))
        + " - "
        + &format!("{}", yansi::Paint::new("world").dim());
    assert_eq!(strip_ansi(&colored), "hello - world");
  }
}
