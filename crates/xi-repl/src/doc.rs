//! `:doc <expr>` meta-command (`openspec/changes/xi-repl/tasks.md` task
//! 7.1).
//!
//! Scope, deliberately narrow for v1 (proposal Open Question 2): package
//! `meta.description`/`meta.longDescription` only. Builtins/`lib` function
//! documentation is explicitly out of scope and [`run`] says so out loud -
//! it never just prints nothing for an expression that isn't a package.

use std::path::Path;

use xi_core::nix_value::{self, VersionInfo};
use xi_develop::daemon::protocol_v3::ExprEvalResult;
use yansi::{Color, Paint};

use crate::session;

/// `:doc` on an expression that isn't derivation-shaped: explains the v1
/// scope rather than printing nothing (task 7.1's explicit requirement).
const OUT_OF_SCOPE_MESSAGE: &str =
  "xi repl: `:doc` only looks up package \
   `meta.description`/`meta.longDescription` for v1 - this expression did not \
   evaluate to a derivation. Builtins/`lib` function documentation is \
   explicitly out of scope for now (see the xi-repl proposal's Open Question \
   2).";

/// `:doc` on a genuine package that simply has no `meta.description`/
/// `meta.longDescription` set.
const NO_META_MESSAGE: &str =
  "(no meta.description or meta.longDescription set)";

/// Run `:doc <expr>`: evaluate `expr` and, if it is derivation-shaped,
/// print its name/version (via the shared [`nix_value`] renderer) followed
/// by `meta.description`/`meta.longDescription`.
///
/// `meta.longDescription` is not part of
/// [`ExprEvalResult::Derivation`]'s wire shape (only `description` is - see
/// `xi_develop::daemon::protocol_v3::reply`'s doc comment), so it is
/// fetched with one extra, narrowly-scoped `ExprEval` call rather than
/// widening the daemon protocol for a field only this one command needs.
pub fn run(socket_path: &Path, flake_root: &Path, expr: &str) {
  match session::eval(socket_path, flake_root, expr) {
    Ok(ExprEvalResult::Derivation {
      name,
      version,
      description,
    }) => {
      let long_description =
        fetch_long_description(socket_path, flake_root, expr);
      println!(
        "{}",
        render_package_doc(
          name.as_deref(),
          version.as_deref(),
          description.as_deref(),
          long_description.as_deref(),
        )
      );
    },
    // A genuine evaluation failure is not a doc-scope question - surface
    // it exactly like the main loop does for any other eval error.
    Ok(ExprEvalResult::Error { message }) => {
      eprintln!("{}", Paint::new(message).fg(Color::Red));
    },
    Ok(_) => {
      eprintln!("{}", Paint::new(OUT_OF_SCOPE_MESSAGE).fg(Color::Yellow));
    },
    Err(err) => eprintln!("{}", Paint::new(err.to_string()).fg(Color::Red)),
  }
}

/// Fetch `(expr).meta.longDescription or null` as its own `ExprEval` call.
/// Any outcome other than a non-empty string (unreachable daemon, the
/// attribute missing, a non-string value from a malformed `meta`, …)
/// collapses to `None` - this is a best-effort enrichment on top of
/// `:doc`'s main result, not something worth a second named error for.
fn fetch_long_description(
  socket_path: &Path,
  flake_root: &Path,
  expr: &str,
) -> Option<String> {
  let probe = format!("({expr}).meta.longDescription or null");
  match session::eval(socket_path, flake_root, &probe) {
    Ok(ExprEvalResult::Scalar(serde_json::Value::String(s)))
      if !s.is_empty() =>
    {
      Some(s)
    },
    _ => None,
  }
}

/// Render `:doc`'s package-doc body - the pure, testable half of [`run`];
/// the eval round-trips and I/O stay there.
#[must_use]
fn render_package_doc(
  name: Option<&str>,
  version: Option<&str>,
  description: Option<&str>,
  long_description: Option<&str>,
) -> String {
  use std::fmt::Write as _;

  let name = name.unwrap_or("<unnamed derivation>");
  let version_info = version.map(|v| {
    VersionInfo {
      version: v,
      dirty:   false,
    }
  });
  let mut out = nix_value::render_entry(name, version_info.as_ref(), false);

  if description.is_none() && long_description.is_none() {
    let _ = write!(out, "\n{}", Paint::new(NO_META_MESSAGE).dim());
    return out;
  }

  if let Some(desc) = description {
    let _ = write!(out, "\n{}", Paint::new(desc).dim());
  }

  // Nixpkgs convention often repeats `description` as `longDescription`'s
  // first line; skip the duplicate rather than printing the same text
  // twice back to back.
  if let Some(long) = long_description
    && Some(long) != description
  {
    let _ = write!(out, "\n\n{long}");
  }

  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::render::strip_ansi;

  #[test]
  fn package_with_full_meta_shows_name_version_and_both_descriptions() {
    let rendered = render_package_doc(
      Some("hello"),
      Some("2.12"),
      Some("a friendly program"),
      Some("a much longer explanation of hello"),
    );
    let stripped = strip_ansi(&rendered);
    assert!(stripped.contains("hello"));
    assert!(stripped.contains("2.12"));
    assert!(stripped.contains("a friendly program"));
    assert!(stripped.contains("a much longer explanation of hello"));
  }

  #[test]
  fn package_with_no_meta_says_so_explicitly() {
    let rendered = render_package_doc(Some("hello"), Some("2.12"), None, None);
    let stripped = strip_ansi(&rendered);
    assert!(stripped.contains("no meta.description"));
  }

  #[test]
  fn duplicate_long_description_is_not_repeated() {
    let rendered = render_package_doc(
      Some("hello"),
      None,
      Some("same text"),
      Some("same text"),
    );
    let stripped = strip_ansi(&rendered);
    assert_eq!(stripped.matches("same text").count(), 1);
  }

  #[test]
  fn unnamed_derivation_falls_back_to_placeholder() {
    let rendered = render_package_doc(None, None, Some("desc"), None);
    assert!(strip_ansi(&rendered).contains("<unnamed derivation>"));
  }
}
