//! Named color themes for the repl's rendered output ([`crate::render`])
//! and syntax highlighting ([`crate::highlight`]).
//!
//! [`Palette`] is the single source of truth for every semantic color role
//! both modules use; each curated [`ThemeName`] (beyond [`ThemeName::Default`],
//! which reproduces the plain ANSI colors those two modules hardcoded before
//! this existed) picks real 24-bit hex values from that theme's own published
//! palette rather than approximating with the 8-color ANSI set. `render.rs`
//! consumes [`Palette`]'s `yansi::Color`s directly; `highlight.rs` needs
//! `nu_ansi_term::Color` instead (a different crate than the rest of the
//! workspace's `yansi`, inherited from `reedline`'s `Highlighter` trait), so
//! [`to_nu_ansi`] converts on the way in rather than maintaining a second,
//! parallel palette definition.
//!
//! The resolved theme is process-wide for the lifetime of one `xi repl`
//! invocation (there is exactly one terminal, so no caller ever needs two
//! themes at once): [`set`] is called once, early, from [`crate::args`]
//! before the input loop starts, and every `render`/`highlight` call site
//! reads it back via [`current`]. A `OnceLock` keeps that without threading
//! a `&Palette` through every rendering function; [`current`] falls back to
//! [`ThemeName::Default`] when nothing has called [`set`] yet, which is
//! exactly what every existing unit test in `render.rs`/`highlight.rs`
//! relies on implicitly (none of them call [`set`]).

use std::{fmt, str::FromStr, sync::OnceLock};

use yansi::Color;

/// A curated, named color theme. `Default` reproduces the plain 8-color
/// ANSI palette `render.rs`/`highlight.rs` hardcoded before theming
/// existed; the rest are real hex values lifted from each theme's own
/// published palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeName {
  #[default]
  Default,
  Dracula,
  CatppuccinMocha,
  Nord,
  GruvboxDark,
  SolarizedDark,
}

/// Every curated theme, in the order [`ThemeName::parse_error`] lists them.
pub const ALL: &[ThemeName] = &[
  ThemeName::Default,
  ThemeName::Dracula,
  ThemeName::CatppuccinMocha,
  ThemeName::Nord,
  ThemeName::GruvboxDark,
  ThemeName::SolarizedDark,
];

impl ThemeName {
  /// The canonical `config.toml`/`--theme` spelling.
  #[must_use]
  pub const fn name(self) -> &'static str {
    match self {
      Self::Default => "default",
      Self::Dracula => "dracula",
      Self::CatppuccinMocha => "catppuccin-mocha",
      Self::Nord => "nord",
      Self::GruvboxDark => "gruvbox-dark",
      Self::SolarizedDark => "solarized-dark",
    }
  }
}

/// Error returned by [`ThemeName`]'s [`FromStr`] impl: `input` is not a
/// recognized theme name, so the message lists every name that is.
#[derive(Debug, Clone)]
pub struct UnknownTheme {
  input: String,
}

impl fmt::Display for UnknownTheme {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let names: Vec<&str> = ALL.iter().map(|t| t.name()).collect();
    write!(
      f,
      "unknown theme '{}' (expected one of: {})",
      self.input,
      names.join(", ")
    )
  }
}

impl std::error::Error for UnknownTheme {}

impl FromStr for ThemeName {
  type Err = UnknownTheme;

  /// Case-insensitive; `_` and `-` are interchangeable, and a theme
  /// family's first-listed flavor (e.g. `"catppuccin"` for
  /// [`Self::CatppuccinMocha`]) is accepted as a shorthand alias.
  fn from_str(input: &str) -> Result<Self, Self::Err> {
    let normalized = input.to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
      "default" => Ok(Self::Default),
      "dracula" => Ok(Self::Dracula),
      "catppuccin" | "catppuccin-mocha" => Ok(Self::CatppuccinMocha),
      "nord" => Ok(Self::Nord),
      "gruvbox" | "gruvbox-dark" => Ok(Self::GruvboxDark),
      "solarized" | "solarized-dark" => Ok(Self::SolarizedDark),
      _ => {
        Err(UnknownTheme {
          input: input.to_owned(),
        })
      },
    }
  }
}

/// Every semantic color role [`crate::render`] and [`crate::highlight`]
/// paint with. One role may back several call sites that happened to
/// share a color by coincidence in the original hardcoded version (e.g.
/// [`Self::string`] covers both a flat scalar result and a highlighted
/// string literal); they are kept as one role, not split, for exactly the
/// cases where every curated theme agrees they should move together.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
  /// Braces/brackets/parens, the `Attrs` list, and the tree view's `=`.
  pub structural: Color,
  /// Strings, paths, URIs, and a flat (non-tree) scalar result.
  pub string:     Color,
  /// Integers and floats.
  pub number:     Color,
  /// `true` in a tree view.
  pub bool_true:  Color,
  /// `false` in a tree view.
  pub bool_false: Color,
  /// Identifiers and attrset keys.
  pub identifier: Color,
  /// Keywords (`let`, `if`, ...) and the `<lambda>` placeholder.
  pub keyword:    Color,
  /// The `<thunk>` placeholder.
  pub thunk:      Color,
  /// The `<unrepresentable: ...>` placeholder.
  pub warning:    Color,
  /// Error messages and a highlighted parse error token.
  pub error:      Color,
  /// Comments, descriptions, and `null` - muted text that is present but
  /// deliberately de-emphasized rather than wrong.
  pub dim:        Color,
}

impl Palette {
  /// Look up the fixed palette for `theme`.
  #[must_use]
  pub const fn for_theme(theme: ThemeName) -> Self {
    match theme {
      ThemeName::Default => {
        Self {
          structural: Color::Cyan,
          string:     Color::Green,
          number:     Color::Yellow,
          bool_true:  Color::Green,
          bool_false: Color::Red,
          identifier: Color::Blue,
          keyword:    Color::Magenta,
          thunk:      Color::Yellow,
          warning:    Color::Yellow,
          error:      Color::Red,
          dim:        Color::Primary,
        }
      },
      // https://draculatheme.com/contribute
      ThemeName::Dracula => {
        Self {
          structural: Color::Rgb(0x8B, 0xE9, 0xFD),
          string:     Color::Rgb(0x50, 0xFA, 0x7B),
          number:     Color::Rgb(0xF1, 0xFA, 0x8C),
          bool_true:  Color::Rgb(0x50, 0xFA, 0x7B),
          bool_false: Color::Rgb(0xFF, 0x55, 0x55),
          identifier: Color::Rgb(0xBD, 0x93, 0xF9),
          keyword:    Color::Rgb(0xFF, 0x79, 0xC6),
          thunk:      Color::Rgb(0xFF, 0xB8, 0x6C),
          warning:    Color::Rgb(0xFF, 0xB8, 0x6C),
          error:      Color::Rgb(0xFF, 0x55, 0x55),
          dim:        Color::Rgb(0x62, 0x72, 0xA4),
        }
      },
      // https://catppuccin.com/palette (Mocha flavor)
      ThemeName::CatppuccinMocha => {
        Self {
          structural: Color::Rgb(0x89, 0xDC, 0xEB),
          string:     Color::Rgb(0xA6, 0xE3, 0xA1),
          number:     Color::Rgb(0xF9, 0xE2, 0xAF),
          bool_true:  Color::Rgb(0xA6, 0xE3, 0xA1),
          bool_false: Color::Rgb(0xF3, 0x8B, 0xA8),
          identifier: Color::Rgb(0x89, 0xB4, 0xFA),
          keyword:    Color::Rgb(0xCB, 0xA6, 0xF7),
          thunk:      Color::Rgb(0xFA, 0xB3, 0x87),
          warning:    Color::Rgb(0xFA, 0xB3, 0x87),
          error:      Color::Rgb(0xF3, 0x8B, 0xA8),
          dim:        Color::Rgb(0x6C, 0x70, 0x86),
        }
      },
      // https://www.nordtheme.com/docs/colors-and-palettes
      ThemeName::Nord => {
        Self {
          structural: Color::Rgb(0x88, 0xC0, 0xD0),
          string:     Color::Rgb(0xA3, 0xBE, 0x8C),
          number:     Color::Rgb(0xEB, 0xCB, 0x8B),
          bool_true:  Color::Rgb(0xA3, 0xBE, 0x8C),
          bool_false: Color::Rgb(0xBF, 0x61, 0x6A),
          identifier: Color::Rgb(0x81, 0xA1, 0xC1),
          keyword:    Color::Rgb(0xB4, 0x8E, 0xAD),
          thunk:      Color::Rgb(0xD0, 0x87, 0x70),
          warning:    Color::Rgb(0xD0, 0x87, 0x70),
          error:      Color::Rgb(0xBF, 0x61, 0x6A),
          dim:        Color::Rgb(0x4C, 0x56, 0x6A),
        }
      },
      // https://github.com/morhetz/gruvbox#color-scheme (dark, bright accents)
      ThemeName::GruvboxDark => {
        Self {
          structural: Color::Rgb(0x8E, 0xC0, 0x7C),
          string:     Color::Rgb(0xB8, 0xBB, 0x26),
          number:     Color::Rgb(0xFA, 0xBD, 0x2F),
          bool_true:  Color::Rgb(0xB8, 0xBB, 0x26),
          bool_false: Color::Rgb(0xFB, 0x49, 0x34),
          identifier: Color::Rgb(0x83, 0xA5, 0x98),
          keyword:    Color::Rgb(0xD3, 0x86, 0x9B),
          thunk:      Color::Rgb(0xFE, 0x80, 0x19),
          warning:    Color::Rgb(0xFE, 0x80, 0x19),
          error:      Color::Rgb(0xFB, 0x49, 0x34),
          dim:        Color::Rgb(0x92, 0x83, 0x74),
        }
      },
      // https://ethanschoonover.com/solarized/
      ThemeName::SolarizedDark => {
        Self {
          structural: Color::Rgb(0x2A, 0xA1, 0x98),
          string:     Color::Rgb(0x85, 0x99, 0x00),
          number:     Color::Rgb(0xB5, 0x89, 0x00),
          bool_true:  Color::Rgb(0x85, 0x99, 0x00),
          bool_false: Color::Rgb(0xDC, 0x32, 0x2F),
          identifier: Color::Rgb(0x26, 0x8B, 0xD2),
          keyword:    Color::Rgb(0x6C, 0x71, 0xC4),
          thunk:      Color::Rgb(0xCB, 0x4B, 0x16),
          warning:    Color::Rgb(0xCB, 0x4B, 0x16),
          error:      Color::Rgb(0xDC, 0x32, 0x2F),
          dim:        Color::Rgb(0x58, 0x6E, 0x75),
        }
      },
    }
  }
}

static CURRENT: OnceLock<Palette> = OnceLock::new();

/// Resolve `theme` as the process-wide palette every subsequent
/// [`current`] call returns.
///
/// Only the first call has any effect (there is exactly one terminal, so
/// no caller legitimately needs a second); see the module doc comment.
pub fn set(theme: ThemeName) {
  let _ = CURRENT.set(Palette::for_theme(theme));
}

/// The active palette: whatever [`set`] last resolved, or
/// [`ThemeName::Default`] if nothing ever called it.
#[must_use]
pub fn current() -> &'static Palette {
  CURRENT.get_or_init(|| Palette::for_theme(ThemeName::Default))
}

/// Convert a [`yansi::Color`] (this module's and `render.rs`'s currency)
/// into the [`nu_ansi_term::Color`] `highlight.rs` needs, since that is
/// the color type `reedline`'s `Highlighter` trait requires and maintaining
/// a second, parallel palette in that type would risk the two drifting
/// apart theme-by-theme.
#[must_use]
pub const fn to_nu_ansi(color: Color) -> nu_ansi_term::Color {
  use nu_ansi_term::Color as Nu;
  match color {
    Color::Primary => Nu::Default,
    Color::Fixed(n) => Nu::Fixed(n),
    Color::Rgb(r, g, b) => Nu::Rgb(r, g, b),
    Color::Black => Nu::Black,
    Color::Red => Nu::Red,
    Color::Green => Nu::Green,
    Color::Yellow => Nu::Yellow,
    Color::Blue => Nu::Blue,
    Color::Magenta => Nu::Magenta,
    Color::Cyan => Nu::Cyan,
    Color::White => Nu::White,
    Color::BrightBlack => Nu::DarkGray,
    Color::BrightRed => Nu::LightRed,
    Color::BrightGreen => Nu::LightGreen,
    Color::BrightYellow => Nu::LightYellow,
    Color::BrightBlue => Nu::LightBlue,
    Color::BrightMagenta => Nu::LightMagenta,
    Color::BrightCyan => Nu::LightCyan,
    Color::BrightWhite => Nu::LightGray,
  }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn default_theme_matches_the_original_hardcoded_ansi_colors() {
    let palette = Palette::for_theme(ThemeName::Default);
    assert_eq!(palette.structural, Color::Cyan);
    assert_eq!(palette.string, Color::Green);
    assert_eq!(palette.number, Color::Yellow);
    assert_eq!(palette.identifier, Color::Blue);
    assert_eq!(palette.keyword, Color::Magenta);
    assert_eq!(palette.error, Color::Red);
  }

  #[test]
  fn parses_known_names_case_and_separator_insensitively() {
    assert_eq!("Dracula".parse::<ThemeName>().unwrap(), ThemeName::Dracula);
    assert_eq!(
      "CATPPUCCIN_MOCHA".parse::<ThemeName>().unwrap(),
      ThemeName::CatppuccinMocha
    );
    assert_eq!(
      "gruvbox".parse::<ThemeName>().unwrap(),
      ThemeName::GruvboxDark
    );
  }

  #[test]
  fn unknown_name_lists_every_valid_one() {
    let err = "not-a-theme".parse::<ThemeName>().unwrap_err();
    let message = err.to_string();
    assert!(message.contains("not-a-theme"));
    for theme in ALL {
      assert!(
        message.contains(theme.name()),
        "{message} should mention {}",
        theme.name()
      );
    }
  }

  #[test]
  fn every_curated_theme_round_trips_its_own_name() {
    for theme in ALL {
      assert_eq!(theme.name().parse::<ThemeName>().unwrap(), *theme);
    }
  }

  #[test]
  fn to_nu_ansi_preserves_named_colors() {
    assert_eq!(to_nu_ansi(Color::Blue), nu_ansi_term::Color::Blue);
    assert_eq!(to_nu_ansi(Color::Magenta), nu_ansi_term::Color::Magenta);
  }

  #[test]
  fn to_nu_ansi_preserves_true_color() {
    assert_eq!(
      to_nu_ansi(Color::Rgb(0x8B, 0xE9, 0xFD)),
      nu_ansi_term::Color::Rgb(0x8B, 0xE9, 0xFD)
    );
  }
}
