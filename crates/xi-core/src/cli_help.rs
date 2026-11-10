//! Single source of truth for clap `--help` styling and for printing
//! manually-assembled (non-clap-native) help text.
//!
//! clap's own help/error renderer (triggered when clap itself handles
//! `--help`/`-h`/usage errors via `err.exit()`) colorizes through
//! `anstream` internally, so it honors tty detection, `NO_COLOR`, and
//! `CLICOLOR_FORCE` automatically. Any help text that is instead
//! hand-assembled outside that path (e.g. a grouped command listing
//! spliced together from clap's rendered help) must go through the same
//! styling definition and the same tty/env-aware stream, or it drifts
//! from - and looks inconsistent with - clap's native rendering. This
//! module exists so there is exactly one place that happens.

use std::io::Write as _;

use clap::builder::Styles;

/// The [`Styles`] applied to every xi `clap::Command` tree.
///
/// Bold headers, bold-yellow literals (command/flag names), everything
/// else left unstyled.
#[must_use]
pub const fn styles() -> Styles {
  Styles::plain().header(anstyle::Style::new().bold()).literal(
    anstyle::Style::new()
      .bold()
      .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
  )
}

/// Print `text` to stdout through an `anstream` auto-detecting stream.
///
/// Embedded ANSI escapes in `text` are kept when stdout is a tty (and
/// `NO_COLOR`/`CLICOLOR_FORCE` allow it) and stripped otherwise - the
/// same behavior clap's own help printer already applies, so manually
/// assembled help text looks exactly as "colored or not" as clap-native
/// help does in the same environment.
pub fn print_styled(text: &str) {
  let mut out = anstream::AutoStream::auto(std::io::stdout());
  let _ = write!(out, "{text}");
}

/// Render `cmd`'s long help with its `Commands:` section grouped into
/// bounded contexts, instead of the flat list clap renders by default.
///
/// clap has no native concept of subcommand groups (only a single
/// app-wide `subcommand_help_heading`), so this renders clap's own long
/// help, keeps clap's own ANSI styling via
/// [`clap::builder::StyledStr::ansi`], and splices group headings into
/// the `Commands:` block by matching each line's leading command name
/// against `groups`. `groups` is an ordered list of `(heading,
/// command_names)`; a heading is inserted right before the first line
/// whose leading token matches that group's first-listed command name.
///
/// The heading itself is styled with this module's own [`styles`]'s
/// header style, so it can never visually drift from clap's native
/// header styling (e.g. for `Usage:`, `Commands:`, `Options:`).
#[must_use]
pub fn grouped_help(
  cmd: &mut clap::Command,
  groups: &[(&str, &[&str])],
) -> String {
  let rendered = cmd.render_long_help().ansi().to_string();
  splice_command_groups(&rendered, groups)
}

fn splice_command_groups(help: &str, groups: &[(&str, &[&str])]) -> String {
  use std::fmt::Write as _;

  let Some(commands_start) = help.find("Commands:") else {
    return help.to_owned();
  };
  let section_start = commands_start + "Commands:".len();
  let Some(options_rel) = help[section_start..].find("Options:") else {
    return help.to_owned();
  };
  let options_start = section_start + options_rel;

  let mut heading_before: std::collections::HashMap<&str, &str> =
    std::collections::HashMap::new();
  for (group, names) in groups {
    if let Some(&first) = names.first() {
      heading_before.insert(first, group);
    }
  }

  let header_style = *styles().get_header();

  let mut body = String::new();
  for line in help[section_start..options_start].lines() {
    // The base help was rendered with `.ansi()`, so command names carry
    // their native bold styling (e.g. `\x1b[1malpha\x1b[0m`) - strip
    // those codes before matching the leading token against `groups`,
    // but keep styling it the original, still-styled `line` below.
    let plain = strip_ansi(line);
    if plain.trim().is_empty() {
      continue;
    }
    let name = plain.split_whitespace().next().unwrap_or("");
    if let Some(group) = heading_before.get(name) {
      if !body.is_empty() {
        body.push('\n');
      }
      let _ = writeln!(
        body,
        "  {header_style}[{group}]{}",
        header_style.render_reset()
      );
    }
    body.push_str("  ");
    body.push_str(line);
    body.push('\n');
  }

  format!(
    "{}Commands:\n\n{body}\n{}",
    &help[..commands_start],
    &help[options_start..]
  )
}

/// Strip ANSI CSI (`\x1b[...<letter>`) sequences from `s`, leaving any
/// other characters untouched.
///
/// Used only to recover a line's plain leading token for group matching;
/// the original, still-styled line is what actually gets written to the
/// output.
fn strip_ansi(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut chars = s.chars().peekable();
  while let Some(c) = chars.next() {
    if c == '\u{1b}' && chars.peek() == Some(&'[') {
      chars.next(); // consume '['
      for c2 in chars.by_ref() {
        if c2.is_ascii_alphabetic() {
          break;
        }
      }
    } else {
      out.push(c);
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn styles_header_is_bold() {
    let s = styles();
    let header = s.get_header();
    assert!(header.get_effects().contains(anstyle::Effects::BOLD));
  }

  #[test]
  fn styles_literal_is_bold_yellow() {
    let s = styles();
    let literal = s.get_literal();
    assert!(literal.get_effects().contains(anstyle::Effects::BOLD));
    assert_eq!(
      literal.get_fg_color(),
      Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))
    );
  }

  #[test]
  fn grouped_help_inserts_heading_and_keeps_both_commands() {
    let mut cmd = clap::Command::new("fakeapp")
      .subcommand(clap::Command::new("alpha").about("First command"))
      .subcommand(clap::Command::new("beta").about("Second command"));

    let groups: &[(&str, &[&str])] =
      &[("My Group", &["alpha", "beta"])];

    let rendered = grouped_help(&mut cmd, groups);

    assert!(rendered.contains("[My Group]"));
    assert!(rendered.contains("alpha"));
    assert!(rendered.contains("beta"));
  }

  #[test]
  fn grouped_help_falls_back_when_no_commands_section() {
    let mut cmd = clap::Command::new("fakeapp");
    let groups: &[(&str, &[&str])] = &[("Group", &["alpha"])];

    let rendered = grouped_help(&mut cmd, groups);

    // No subcommands means no "Commands:" section; the renderer must
    // return the base help unchanged rather than panicking or
    // corrupting it.
    assert!(!rendered.contains("[Group]"));
  }
}
