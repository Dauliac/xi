//! `rnix-parser`-based syntax highlighting for the input line
//! (`openspec/changes/xi-repl/tasks.md` task 6.1).
//!
//! [`NixHighlighter`] implements `reedline`'s [`Highlighter`] trait by
//! tokenizing the buffer with [`rnix_parser::tokenize`]  -  a flat,
//! best-effort lexer that never fails, even on a line that is not yet
//! syntactically complete (which matters here: this runs on every
//! keystroke, long before the user has finished typing a valid
//! expression). A full parse is not needed for coloring, just the token
//! stream.
//!
//! `reedline`'s `Highlighter` trait returns `StyledText`, built from
//! `nu_ansi_term::Style`/`Color`  -  a different ANSI-coloring crate than
//! the rest of this workspace's `yansi`. The color *choices* in
//! [`style_for`] still follow the same convention used elsewhere in `xi`
//! (see `xi_core::nix_value` and [`crate::render`]): identifiers in blue,
//! strings/paths in green, numbers in yellow, keywords in magenta,
//! structural braces/brackets/parens in cyan (matching
//! [`xi_develop::daemon::protocol_v3::ExprEvalResult::Attrs`]'s cyan in
//! [`crate::render::render`]), comments dimmed, errors in red  -  just
//! expressed in the type `reedline` itself requires.

use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};
use rnix_parser::{SyntaxKind, tokenize};

/// Syntax-highlights a Nix expression as the user types it.
///
/// Stateless: each call re-tokenizes `line` from scratch. A REPL input
/// line is short enough (one expression) that re-lexing on every
/// keystroke is not worth caching.
pub struct NixHighlighter;

impl Highlighter for NixHighlighter {
  fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
    let mut styled = StyledText::new();
    for (kind, text) in tokenize(line) {
      styled.push((style_for(kind), text.to_owned()));
    }
    styled
  }
}

/// Map one token kind to its display style.
///
/// `SyntaxKind` is `#[non_exhaustive]` (rnix-parser may add kinds later),
/// so every arm not named explicitly falls through to the catch-all
/// plain [`Style`]  -  punctuation/operators (`+`, `.`, `;`, `?`, `==`,
/// …) and whitespace are deliberately left uncolored to avoid visual
/// noise, matching how `crate::render`/`xi_core::nix_value` only color
/// the parts of a line that carry meaning (names, values, tags) and
/// leave separators plain.
fn style_for(kind: SyntaxKind) -> Style {
  use SyntaxKind::{
    TOKEN_ASSERT,
    TOKEN_COMMENT,
    TOKEN_ELSE,
    TOKEN_ERROR,
    TOKEN_FLOAT,
    TOKEN_IDENT,
    TOKEN_IF,
    TOKEN_IN,
    TOKEN_INHERIT,
    TOKEN_INTEGER,
    TOKEN_INTERPOL_END,
    TOKEN_INTERPOL_START,
    TOKEN_L_BRACE,
    TOKEN_L_BRACK,
    TOKEN_L_PAREN,
    TOKEN_LET,
    TOKEN_OR,
    TOKEN_PATH_ABS,
    TOKEN_PATH_HOME,
    TOKEN_PATH_REL,
    TOKEN_PATH_SEARCH,
    TOKEN_R_BRACE,
    TOKEN_R_BRACK,
    TOKEN_R_PAREN,
    TOKEN_REC,
    TOKEN_STRING_CONTENT,
    TOKEN_STRING_END,
    TOKEN_STRING_START,
    TOKEN_THEN,
    TOKEN_URI,
    TOKEN_WITH,
  };

  match kind {
    // Keywords. `true`/`false`/`null` are plain bound identifiers in Nix,
    // not keywords - rnix-parser tokenizes them as `TOKEN_IDENT`, so they
    // fall through to the identifier arm below rather than getting a
    // special case here.
    TOKEN_ASSERT | TOKEN_ELSE | TOKEN_IF | TOKEN_IN | TOKEN_INHERIT
    | TOKEN_LET | TOKEN_OR | TOKEN_REC | TOKEN_THEN | TOKEN_WITH => {
      Color::Magenta.normal()
    },
    TOKEN_IDENT => Color::Blue.normal(),
    // Strings, interpolation delimiters, paths, and bare URIs all read as
    // one "literal text" color.
    TOKEN_STRING_START | TOKEN_STRING_CONTENT | TOKEN_STRING_END
    | TOKEN_INTERPOL_START | TOKEN_INTERPOL_END | TOKEN_URI
    | TOKEN_PATH_ABS | TOKEN_PATH_HOME | TOKEN_PATH_REL | TOKEN_PATH_SEARCH => {
      Color::Green.normal()
    },
    TOKEN_INTEGER | TOKEN_FLOAT => Color::Yellow.normal(),
    TOKEN_COMMENT => Style::new().dimmed(),
    TOKEN_L_BRACE | TOKEN_R_BRACE | TOKEN_L_BRACK | TOKEN_R_BRACK
    | TOKEN_L_PAREN | TOKEN_R_PAREN => Color::Cyan.normal(),
    TOKEN_ERROR => Color::Red.normal(),
    _ => Style::new(),
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  /// Flatten a [`StyledText`] back into a plain string, dropping styling,
  /// so tests can assert on token boundaries/content without caring about
  /// the exact `Style` value attached.
  fn plain_text(styled: &StyledText) -> String {
    styled
      .buffer
      .iter()
      .map(|(_, text)| text.as_str())
      .collect()
  }

  #[test]
  fn highlighting_round_trips_the_original_text() {
    let line = "let x = pkgs.hello; in x.outPath";
    let styled = NixHighlighter.highlight(line, line.len());
    assert_eq!(plain_text(&styled), line);
  }

  #[test]
  fn keyword_gets_the_keyword_style() {
    let styled = NixHighlighter.highlight("let a = 1; in a", 0);
    let (style, _) = styled
      .buffer
      .iter()
      .find(|(_, text)| text == "let")
      .expect("`let` should be its own token");
    assert_eq!(style.foreground, Some(Color::Magenta));
  }

  #[test]
  fn identifier_gets_the_identifier_style() {
    let styled = NixHighlighter.highlight("pkgs", 0);
    let (style, text) = &styled.buffer[0];
    assert_eq!(text, "pkgs");
    assert_eq!(style.foreground, Some(Color::Blue));
  }

  #[test]
  fn integer_gets_the_number_style() {
    let styled = NixHighlighter.highlight("42", 0);
    let (style, _) = &styled.buffer[0];
    assert_eq!(style.foreground, Some(Color::Yellow));
  }

  #[test]
  fn braces_get_the_structural_style() {
    let styled = NixHighlighter.highlight("{ }", 0);
    let (style, text) = &styled.buffer[0];
    assert_eq!(text, "{");
    assert_eq!(style.foreground, Some(Color::Cyan));
  }

  #[test]
  fn an_incomplete_line_still_highlights_without_panicking() {
    // Syntactically invalid mid-typing ("let " with nothing bound yet),
    // but tokenization never fails - only a full parse would reject this.
    let styled = NixHighlighter.highlight("let ", 4);
    assert_eq!(plain_text(&styled), "let ");
  }

  #[test]
  fn true_false_and_null_are_identifiers_not_keywords() {
    let styled = NixHighlighter.highlight("true", 0);
    let (style, text) = &styled.buffer[0];
    assert_eq!(text, "true");
    assert_eq!(style.foreground, Some(Color::Blue));
  }
}
