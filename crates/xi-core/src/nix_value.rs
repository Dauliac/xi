//! Shared rendering primitives for Nix derivation names and versions.
//!
//! Several front ends (`xi flake show`, `xi search`) print a derivation's
//! name/version pair with the same color convention  -  name in blue,
//! version in green parens  -  even though they source that data from
//! different shapes (raw `nix flake show --json` entries vs. search index
//! rows). This module centralises the name/version parsing and the shared
//! colored-string rendering so both front ends stay byte-identical without
//! duplicating the logic.
//!
//! Callers decide how to emit the returned strings (`println!`, a preview
//! pane, …)  -  nothing here writes to stdout.

use std::fmt::Write as _;

use yansi::{Color, Paint};

/// Parsed version info extracted from a derivation's full name.
pub struct VersionInfo<'a> {
  pub version: &'a str,
  pub dirty:   bool,
}

/// Split a derivation's full name (e.g. `"xi-4.4.0-ab81553-dirty"`) into a
/// package name and, if present, its version info.
///
/// The split point is the last `-` immediately followed by a digit. A
/// trailing `-dirty` marks the build as dirty; if it is itself preceded by
/// a `-<hex hash>` segment (6+ hex digits), that hash is stripped from the
/// reported version too.
///
/// Returns `(None, None)` only when `full_name` is empty of any such
/// split point and also empty  -  in practice `(Some(full_name), None)` is
/// returned whenever no version-like suffix is found.
#[must_use]
pub fn parse_name_version(
  full_name: &str,
) -> (Option<&str>, Option<VersionInfo<'_>>) {
  let mut split_pos = None;
  for (i, ch) in full_name.char_indices() {
    if ch == '-'
      && let Some(next) = full_name[i + 1..].chars().next()
      && next.is_ascii_digit()
    {
      split_pos = Some(i);
    }
  }

  let Some(pos) = split_pos else {
    return (Some(full_name), None);
  };

  let pkg = &full_name[..pos];
  let raw_version = &full_name[pos + 1..];

  #[allow(clippy::option_if_let_else)]
  let (version, dirty) =
    if let Some(stripped) = raw_version.strip_suffix("-dirty") {
      let clean = stripped
        .rsplit_once('-')
        .filter(|(_, hash)| {
          hash.len() >= 6 && hash.chars().all(|c| c.is_ascii_hexdigit())
        })
        .map_or(stripped, |(before, _)| before);
      (clean, true)
    } else {
      (raw_version, false)
    };

  (Some(pkg), Some(VersionInfo { version, dirty }))
}

/// Render `name`, optionally followed by `" (version)"`, with the shared
/// color convention: name in blue, version in green.
///
/// Returns a colored string  -  never prints. Callers that need leading
/// indentation or trailing annotations (description, `[default]`, …)
/// compose around this.
#[must_use]
pub fn render_name_version(name: &str, version: Option<&str>) -> String {
  let mut rendered = format!("{}", Paint::new(name).fg(Color::Blue));
  if let Some(v) = version
    && !v.is_empty()
  {
    let _ = write!(rendered, " ({})", Paint::new(v).fg(Color::Green));
  }
  rendered
}

/// Render a full entry line segment: name, optional version, and optional
/// `[default]` / `[dirty]` tags  -  in that order, matching `xi flake show`'s
/// convention.
///
/// Description rendering is intentionally excluded: callers format
/// descriptions differently (inline-dim vs. wrapped multi-line), so it stays
/// their responsibility.
#[must_use]
pub fn render_entry(
  name: &str,
  version_info: Option<&VersionInfo<'_>>,
  is_default: bool,
) -> String {
  let mut rendered =
    render_name_version(name, version_info.map(|vi| vi.version));

  if is_default {
    let _ = write!(rendered, " {}", Paint::new("[default]").fg(Color::Yellow));
  }

  if version_info.is_some_and(|vi| vi.dirty) {
    let _ = write!(rendered, " {}", Paint::new("[dirty]").fg(Color::Red));
  }

  rendered
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn clean_version() {
    let (pkg, vi) = parse_name_version("xi-4.4.0");
    assert_eq!(pkg, Some("xi"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "4.4.0");
    assert!(!vi.dirty);
  }

  #[test]
  fn dirty_version_with_hash() {
    let (pkg, vi) = parse_name_version("xi-4.4.0-ab81553-dirty");
    assert_eq!(pkg, Some("xi"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "4.4.0");
    assert!(vi.dirty);
  }

  #[test]
  fn dirty_version_without_hash() {
    let (pkg, vi) = parse_name_version("foo-1.2.3-dirty");
    assert_eq!(pkg, Some("foo"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "1.2.3");
    assert!(vi.dirty);
  }

  #[test]
  fn no_version() {
    let (pkg, vi) = parse_name_version("nix-shell");
    assert_eq!(pkg, Some("nix-shell"));
    assert!(vi.is_none());
  }

  #[test]
  fn complex_name_with_version() {
    let (pkg, vi) = parse_name_version("nix3-fmt-wrapper-2.25.0");
    assert_eq!(pkg, Some("nix3-fmt-wrapper"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "2.25.0");
    assert!(!vi.dirty);
  }

  #[test]
  fn empty_name() {
    let (pkg, vi) = parse_name_version("");
    assert_eq!(pkg, Some(""));
    assert!(vi.is_none());
  }

  #[test]
  fn version_with_trailing_non_numeric_segment() {
    // Only the LAST dash-digit boundary is used as the split point, so a
    // trailing non-version segment (e.g. nixpkgs' `postgresql-13-contrib`)
    // stays part of the reported version rather than being mistaken for a
    // second package-name component.
    let (pkg, vi) = parse_name_version("postgresql-13-contrib");
    assert_eq!(pkg, Some("postgresql"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "13-contrib");
    assert!(!vi.dirty);
  }

  #[test]
  fn render_entry_orders_name_version_default_dirty_tags() {
    // Pins the shared renderer's tag order: name, then version, then
    // [default], then [dirty]  -  the exact sequence `xi flake show` and
    // `xi search` must stay byte-identical on.
    let vi = VersionInfo {
      version: "1.0.0",
      dirty:   true,
    };
    let rendered = render_entry("pkg", Some(&vi), true);

    let name_pos = rendered.find("pkg").expect("name present");
    let version_pos = rendered.find("1.0.0").expect("version present");
    let default_pos = rendered.find("[default]").expect("default tag present");
    let dirty_pos = rendered.find("[dirty]").expect("dirty tag present");

    assert!(name_pos < version_pos);
    assert!(version_pos < default_pos);
    assert!(default_pos < dirty_pos);
  }
}
