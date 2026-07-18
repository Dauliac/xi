//! Unified terminal styling — icons, colors, and formatted output.
//!
//! All user-visible formatting goes through this module to ensure
//! consistent appearance across all xi crates.

/// ANSI color codes.
pub mod color {
  pub const RED: &str = "\x1b[31m";
  pub const GREEN: &str = "\x1b[32m";
  pub const YELLOW: &str = "\x1b[33m";
  pub const BLUE: &str = "\x1b[34m";
  pub const WHITE: &str = "\x1b[37m";
  pub const BOLD: &str = "\x1b[1m";
  pub const DIM: &str = "\x1b[2m";
  pub const RESET: &str = "\x1b[0m";
}

/// Standard icons for consistent status display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
  /// ✓ — success, complete, trusted
  Success,
  /// ✗ — error, failure
  Error,
  /// ⟳ — loading, in-progress, building
  Loading,
  /// ▲ — warning, pending, attention
  Warn,
  /// ● — info, neutral status
  Info,
  /// + — added (diff)
  Added,
  /// - — removed (diff)
  Removed,
  /// ~ — changed (diff)
  Changed,
}

impl Icon {
  /// The icon character.
  #[must_use]
  pub const fn glyph(self) -> &'static str {
    match self {
      Self::Success => "✓",
      Self::Error => "✗",
      Self::Loading => "⟳",
      Self::Warn => "▲",
      Self::Info => "●",
      Self::Added => "+",
      Self::Removed => "-",
      Self::Changed => "~",
    }
  }

  /// The ANSI color for this icon.
  #[must_use]
  pub const fn color(self) -> &'static str {
    match self {
      Self::Success | Self::Added => color::GREEN,
      Self::Error | Self::Removed => color::RED,
      Self::Loading => color::BLUE,
      Self::Warn | Self::Changed => color::YELLOW,
      Self::Info => color::WHITE,
    }
  }

  /// Render the icon with its color: e.g. `"\x1b[32m✓\x1b[0m"`.
  #[must_use]
  pub fn render(self) -> String {
    format!("{}{}{}", self.color(), self.glyph(), color::RESET)
  }

  /// Render the icon with color and a bracketed label:
  /// e.g. `"\x1b[32m✓ [xi]\x1b[0m"`.
  #[must_use]
  pub fn render_with_label(self, label: &str) -> String {
    format!(
      "{}{} [{}]{}",
      self.color(),
      self.glyph(),
      label,
      color::RESET
    )
  }
}

/// Wrap text in bold: `"\x1b[1m{text}\x1b[0m"`.
#[must_use]
pub fn bold(text: &str) -> String {
  format!("{}{text}{}", color::BOLD, color::RESET)
}

/// Wrap text in dim: `"\x1b[2m{text}\x1b[0m"`.
#[must_use]
pub fn dim(text: &str) -> String {
  format!("{}{text}{}", color::DIM, color::RESET)
}

/// Wrap text in a specific color.
#[must_use]
pub fn colored(text: &str, ansi_color: &str) -> String {
  format!("{ansi_color}{text}{}", color::RESET)
}

/// Format a status line: `"  {icon} {message}"`.
#[must_use]
pub fn status_line(icon: Icon, message: &str) -> String {
  format!("{} {message}", icon.render())
}

/// Format a labeled status line with prefix:
/// `"  {icon} [{label}] {message}"`.
#[must_use]
pub fn labeled_status(icon: Icon, label: &str, message: &str) -> String {
  format!("{} {message}", icon.render_with_label(label))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn icon_glyph() {
    assert_eq!(Icon::Success.glyph(), "✓");
    assert_eq!(Icon::Error.glyph(), "✗");
    assert_eq!(Icon::Loading.glyph(), "⟳");
    assert_eq!(Icon::Warn.glyph(), "▲");
    assert_eq!(Icon::Info.glyph(), "●");
  }

  #[test]
  fn icon_render_contains_glyph_and_reset() {
    let rendered = Icon::Success.render();
    assert!(rendered.contains("✓"));
    assert!(rendered.contains(color::RESET));
    assert!(rendered.starts_with(color::GREEN));
  }

  #[test]
  fn icon_render_with_label() {
    let rendered = Icon::Error.render_with_label("cache");
    assert!(rendered.contains("✗"));
    assert!(rendered.contains("[cache]"));
    assert!(rendered.starts_with(color::RED));
  }

  #[test]
  fn bold_wraps_text() {
    let s = bold("hello");
    assert!(s.starts_with(color::BOLD));
    assert!(s.contains("hello"));
    assert!(s.ends_with(color::RESET));
  }

  #[test]
  fn dim_wraps_text() {
    let s = dim("path");
    assert!(s.starts_with(color::DIM));
    assert!(s.contains("path"));
  }
}
