//! The `xi-sources-config` capability: one config surface for the whole
//! `xi-sources` family.
//!
//! [`super`] (the `xi-sources-model` capability) names the three on-disk
//! concepts and the [`Scope`] resolver but deliberately stops short of
//! concrete paths, because the model is OS-neutral and the paths are not.
//! This module binds them:
//!
//! - [`ScopePaths`] is the per-scope filesystem table (and the XDG fallbacks it
//!   leans on),
//! - [`SourcesConfig`] is one *partially specified* layer of settings, parsed
//!   from a `[sources]` TOML table or from `XI_SOURCES_*`,
//! - [`Layers`] stacks those layers and [`Layers::resolve`] walks the
//!   precedence ladder, recording where every answer came from so `xi sources
//!   status --explain` can print it.
//!
//! Every downstream capability (gitops, push, bootstrap, cache-prestage,
//! conflict) reads its configuration through this module rather than
//! parsing config itself.

use std::{
  collections::BTreeMap,
  fmt,
  path::{Path, PathBuf},
  time::Duration,
};

use serde::{Deserialize, Serialize};

use super::{
  Scope,
  scp_separator,
  secrets::{SecretSpec, merge_secrets, parse_secrets_table},
};

// ---------------------------------------------------------------------------
// Built-in defaults
// ---------------------------------------------------------------------------

/// Branch tracked when no layer names one.
pub const DEFAULT_BRANCH: &str = "main";

/// How often the gitops daemon re-checks the remote when no layer says.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_mins(1);

/// Every key the `[sources]` table accepts.
///
/// The list is closed: anything else is a load-time error, so a typo
/// surfaces at config load instead of silently doing nothing. Keys are
/// spelled in kebab-case here, which is also their `.xi.toml` spelling.
pub const SOURCES_KEYS: &[&str] = &[
  "remote",
  "branch",
  "working-dir",
  "poll-interval",
  "require-signed",
  "allowed-signers",
  "gpg-public-keys",
  "sync-mode",
  "installer",
  "reboot-window",
  "on-advance",
  "on-build",
  "on-switch",
  // A sub-table, not a scalar: `xi-sources-credentials` owns its
  // contents and rejects an unknown kind the same way this list
  // rejects an unknown key. See [`super::credentials`].
  "credentials",
];

/// Sub-tables the `[sources]` table accepts, alongside [`SOURCES_KEYS`].
///
/// Kept separate because every name in [`SOURCES_KEYS`] has a matching
/// `XI_SOURCES_*` variable ([`env_var_for`]) and a sub-table does not: a
/// map of records has no single-value env spelling.
pub const SOURCES_TABLE_KEYS: &[&str] = &["secrets"];

/// Every key an `[[sources.remote]]` entry accepts.
///
/// Closed for the same reason [`SOURCES_KEYS`] is: a misspelled `branch`
/// key inside a remote entry would otherwise silently poll the default
/// branch.
pub const REMOTE_ENTRY_KEYS: &[&str] = &["url", "branch", "poll-interval"];

/// The three lifecycle hook keys, in the order the daemon fires them.
pub const HOOK_KEYS: &[&str] = &["on-advance", "on-build", "on-switch"];

/// The `XI_SOURCES_*` environment variable for a `[sources]` key.
///
/// The rule is mechanical, per spec: uppercase the key and replace
/// hyphens with underscores. A nested credentials key is spelled as
/// its dotted path, so `credentials.ssh.identity-file` becomes
/// `XI_SOURCES_CREDENTIALS_SSH_IDENTITY_FILE`; the dot is mapped the
/// same way the hyphen is.
#[must_use]
pub fn env_var_for(key: &str) -> String {
  format!(
    "XI_SOURCES_{}",
    key.to_ascii_uppercase().replace(['-', '.'], "_")
  )
}

// ---------------------------------------------------------------------------
// Provenance  -  the five rungs of the ladder
// ---------------------------------------------------------------------------

/// Which surface supplied a resolved value. Highest precedence first.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
  /// A command-line flag, e.g. `--sources-remote`.
  CliFlag,
  /// An `XI_SOURCES_*` environment variable.
  EnvVar,
  /// The `[sources]` table of a `.xi.toml`.
  TomlFile,
  /// A Nix module default, written to the per-scope `sources.toml`.
  NixModule,
  /// The remote recorded in the state dir by a previous adoption.
  ///
  /// Below every configurable rung: an explicit setting always wins
  /// over what a past `switch` happened to adopt.
  StateDir,
  /// The built-in default compiled into xi.
  Default,
}

impl Provenance {
  /// The wire spelling, matching the serde representation.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::CliFlag => "cli-flag",
      Self::EnvVar => "env-var",
      Self::TomlFile => "toml-file",
      Self::NixModule => "nix-module",
      Self::StateDir => "state-dir",
      Self::Default => "default",
    }
  }
}

impl fmt::Display for Provenance {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

// ---------------------------------------------------------------------------
// Small closed enums
// ---------------------------------------------------------------------------

/// What to do when the sources dir advances past the working dir.
///
/// Owned by `xi-sources-conflict`; parsed here so it travels with the rest
/// of the sources config through the ladder. All three values describe the
/// same single direction - sources dir to working dir - and differ only in
/// who decides when it happens. The reverse direction is never automatic:
/// the operator's `git push` is the only path from the working dir back
/// into the sources dir.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
  /// Surface the available sync and wait for `xi <scope> sources sync`.
  /// The default.
  #[default]
  Prompt,
  /// Copy the sources dir over the working dir on the next interactive
  /// command, provided the working dir is clean.
  AutoCopy,
  /// Log the available sync and never act on it.
  Never,
}

/// Which Nix installer a bootstrap run should use.
///
/// Owned by `xi-sources-bootstrap`. There is no built-in default here: the
/// choice depends on the resolved flake's substituters, which this
/// capability cannot see. An unset value means "let bootstrap decide".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Installer {
  /// The Determinate Systems installer.
  Detsys,
  /// The Lix installer.
  Lix,
  /// Assume Nix is already present.
  None,
}

/// Parse a closed enum from its kebab-case spelling.
macro_rules! str_enum {
  ($ty:ident, $($text:literal => $variant:ident),+ $(,)?) => {
    impl $ty {
      /// The kebab-case spelling, matching the serde representation.
      #[must_use]
      pub const fn as_str(self) -> &'static str {
        match self {
          $(Self::$variant => $text,)+
        }
      }

      /// Every accepted spelling, in the order they are listed in errors.
      pub const ALL: &'static [&'static str] = &[$($text,)+];

      /// Parse from the `.xi.toml` / env spelling.
      #[must_use]
      pub fn from_config_str(raw: &str) -> Option<Self> {
        match raw {
          $($text => Some(Self::$variant),)+
          _ => None,
        }
      }
    }

    impl fmt::Display for $ty {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
      }
    }
  };
}

// `xi-sources-credentials` spells its own closed enums the same way, so
// the macro travels with the pattern rather than being re-typed there.
pub(super) use str_enum;

str_enum!(
  SyncMode,
  "prompt" => Prompt,
  "auto-copy" => AutoCopy,
  "never" => Never,
);

str_enum!(
  Installer,
  "detsys" => Detsys,
  "lix" => Lix,
  "none" => None,
);

// ---------------------------------------------------------------------------
// RebootWindow  -  the daily slot a reboot-requiring activation may use
// ---------------------------------------------------------------------------

/// Minutes in a day. The modulus every window arithmetic wraps around.
const MINUTES_PER_DAY: u32 = 24 * 60;

/// The spelling that disables the gate.
const REBOOT_WINDOW_ALWAYS: &str = "always";

/// When the gitops daemon may activate a generation that needs a reboot.
///
/// Owned by `xi-sources-gitops`, parsed here so a malformed window is
/// reported at config load with a file and line rather than at 3am by a
/// daemon that has already built the generation.
///
/// Stored as minutes-since-midnight rather than a clock type so this
/// crate stays free of a date-time dependency: the daemon supplies "now"
/// as a minute of the day and all the arithmetic is integer modular.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RebootWindow {
  /// No gate: a reboot-requiring activation may happen at any time.
  #[default]
  Always,
  /// A daily `[start, end)` slot, possibly wrapping past midnight.
  Daily {
    /// Inclusive start, as minutes since local midnight.
    start: u32,
    /// Exclusive end, as minutes since local midnight.
    end:   u32,
  },
}

impl RebootWindow {
  /// Parse `HH:MM-HH:MM`, or the literal `always`.
  ///
  /// A window whose end equals its start is rejected: read as a wrapping
  /// window it would mean "all day", and read literally it would mean
  /// "never", and guessing between the two is how a host never reboots.
  ///
  /// # Errors
  ///
  /// Returns the reason the window is unusable, without a location
  /// prefix; the caller adds the file+line or variable name.
  pub fn parse(raw: &str) -> Result<Self, String> {
    let text = raw.trim();
    if text.eq_ignore_ascii_case(REBOOT_WINDOW_ALWAYS) {
      return Ok(Self::Always);
    }

    let (start, end) = text.split_once('-').ok_or_else(|| {
      format!(
        "invalid 'reboot-window' value '{text}': expected 'HH:MM-HH:MM' or \
         'always'"
      )
    })?;
    let start = parse_hhmm(start)?;
    let end = parse_hhmm(end)?;
    if start == end {
      return Err(format!(
        "invalid 'reboot-window' value '{text}': start and end are the same \
         minute; use 'always' for no gate"
      ));
    }
    Ok(Self::Daily { start, end })
  }

  /// Whether `minute_of_day` falls inside the window.
  ///
  /// A window with `start > end` wraps past midnight, so `22:00-02:00`
  /// admits both 23:00 and 01:00.
  #[must_use]
  pub const fn admits(self, minute_of_day: u32) -> bool {
    match self {
      Self::Always => true,
      Self::Daily { start, end } => {
        let now = minute_of_day % MINUTES_PER_DAY;
        if start <= end {
          now >= start && now < end
        } else {
          now >= start || now < end
        }
      },
    }
  }

  /// Minutes to wait from `minute_of_day` until the window opens.
  ///
  /// Zero when the window already admits the current minute, so the
  /// caller can treat this as "sleep this long, then activate" without a
  /// separate branch.
  #[must_use]
  pub const fn minutes_until_open(self, minute_of_day: u32) -> u32 {
    if self.admits(minute_of_day) {
      return 0;
    }
    match self {
      Self::Always => 0,
      Self::Daily { start, .. } => {
        let now = minute_of_day % MINUTES_PER_DAY;
        (start + MINUTES_PER_DAY - now) % MINUTES_PER_DAY
      },
    }
  }
}

impl fmt::Display for RebootWindow {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match *self {
      Self::Always => f.write_str(REBOOT_WINDOW_ALWAYS),
      Self::Daily { start, end } => {
        write!(
          f,
          "{:02}:{:02}-{:02}:{:02}",
          start / 60,
          start % 60,
          end / 60,
          end % 60
        )
      },
    }
  }
}

/// Parse one `HH:MM` half of a window into minutes since midnight.
fn parse_hhmm(raw: &str) -> Result<u32, String> {
  let text = raw.trim();
  let (hh, mm) = text.split_once(':').ok_or_else(|| {
    format!("invalid 'reboot-window' time '{text}': expected 'HH:MM'")
  })?;
  let bad = |what: &str| {
    format!("invalid 'reboot-window' time '{text}': {what} out of range")
  };
  let hours: u32 = hh
    .trim()
    .parse()
    .map_err(|_e| bad("hours"))
    .and_then(|h: u32| if h < 24 { Ok(h) } else { Err(bad("hours")) })?;
  let minutes: u32 = mm
    .trim()
    .parse()
    .map_err(|_e| bad("minutes"))
    .and_then(|m: u32| if m < 60 { Ok(m) } else { Err(bad("minutes")) })?;
  Ok(hours * 60 + minutes)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod reboot_window_tests {
  //! Owned by `xi-sources-gitops` section 10.1: [`RebootWindow`] lives in
  //! this file because it is parsed alongside every other `[sources]`
  //! key, but the daily-window semantics it encodes - and the daemon's
  //! reboot-gate behaviour that reads them - belong to the gitops
  //! capability. These tests pin the type on its own, ahead of the
  //! daemon actually consulting it (`xi-sources-gitops` section 10.2,
  //! not yet written).

  use super::RebootWindow;

  #[test]
  fn the_literal_always_parses_to_no_gate() {
    assert_eq!(
      RebootWindow::parse("always").expect("parse"),
      RebootWindow::Always
    );
    assert_eq!(
      RebootWindow::parse("ALWAYS").expect("case-insensitive"),
      RebootWindow::Always
    );
  }

  #[test]
  fn a_non_wrapping_window_parses_to_its_minutes() {
    let window = RebootWindow::parse("02:00-04:00").expect("parse");
    assert_eq!(window, RebootWindow::Daily {
      start: 120,
      end:   240,
    });
  }

  #[test]
  fn a_malformed_window_is_rejected_with_a_message_naming_the_value() {
    let err =
      RebootWindow::parse("not-a-window").expect_err("garbage must not parse");
    assert!(err.contains("HH:MM"), "got: {err}");
  }

  #[test]
  fn a_window_with_no_separator_is_rejected_with_the_full_value() {
    let err =
      RebootWindow::parse("garbage").expect_err("no '-' must not parse");
    assert!(err.contains("garbage"), "got: {err}");
  }

  #[test]
  fn an_out_of_range_hour_or_minute_is_rejected() {
    assert!(RebootWindow::parse("24:00-01:00").is_err());
    assert!(RebootWindow::parse("01:60-02:00").is_err());
  }

  /// A window whose start equals its end is ambiguous (read as "all day"
  /// or "never" depending on direction), so the parser refuses it and
  /// asks the operator to say what they meant.
  #[test]
  fn a_zero_width_window_is_rejected() {
    let err =
      RebootWindow::parse("02:00-02:00").expect_err("zero-width must fail");
    assert!(err.contains("always"), "got: {err}");
  }

  #[test]
  fn always_admits_every_minute() {
    assert!(RebootWindow::Always.admits(0));
    assert!(RebootWindow::Always.admits(1439));
  }

  #[test]
  fn a_non_wrapping_window_admits_only_its_range() {
    let window = RebootWindow::Daily {
      start: 120,
      end:   240,
    };
    assert!(!window.admits(119));
    assert!(window.admits(120), "start is inclusive");
    assert!(window.admits(239));
    assert!(!window.admits(240), "end is exclusive");
  }

  /// SC-G-009's `02:00-04:00` example, and the midnight-wrapping case the
  /// spec's `admits` doc calls out (`22:00-02:00` admits 23:00 and 01:00).
  #[test]
  fn a_wrapping_window_admits_both_sides_of_midnight() {
    let window = RebootWindow::Daily {
      start: 22 * 60,
      end:   2 * 60,
    };
    assert!(window.admits(23 * 60), "23:00 is inside 22:00-02:00");
    assert!(window.admits(60), "01:00 is inside 22:00-02:00");
    assert!(!window.admits(12 * 60), "noon is outside 22:00-02:00");
  }

  #[test]
  fn minutes_until_open_is_zero_when_already_inside_the_window() {
    let window = RebootWindow::Daily {
      start: 120,
      end:   240,
    };
    assert_eq!(window.minutes_until_open(150), 0);
    assert_eq!(RebootWindow::Always.minutes_until_open(999), 0);
  }

  /// SC-G-009: "activation deferred until 02:00 the following day" from
  /// 14:00 with a `02:00-04:00` window - 12 hours.
  #[test]
  fn minutes_until_open_counts_forward_to_the_next_window_start() {
    let window = RebootWindow::Daily {
      start: 120,
      end:   240,
    };
    assert_eq!(window.minutes_until_open(14 * 60), 12 * 60);
  }

  #[test]
  fn minutes_until_open_wraps_correctly_across_midnight() {
    let window = RebootWindow::Daily {
      start: 22 * 60,
      end:   2 * 60,
    };
    // At 23:30, already inside the window.
    assert_eq!(window.minutes_until_open(23 * 60 + 30), 0);
    // At 10:00, the window opens at 22:00 - 12 hours away.
    assert_eq!(window.minutes_until_open(10 * 60), 12 * 60);
  }

  #[test]
  fn display_round_trips_through_parse() {
    for raw in ["always", "02:00-04:00", "22:00-02:00"] {
      let window = RebootWindow::parse(raw).expect("parse");
      assert_eq!(window.to_string(), raw);
      assert_eq!(
        RebootWindow::parse(&window.to_string()).expect("reparse"),
        window
      );
    }
  }
}

// ---------------------------------------------------------------------------
// RemoteSpec  -  one entry of the multi-remote fan-in
// ---------------------------------------------------------------------------

/// One remote to poll, as written in an `[[sources.remote]]` entry.
///
/// `branch` and `poll_interval` are optional per entry: an unset one
/// falls back to the `[sources]`-level key, which in turn falls back to
/// [`DEFAULT_BRANCH`] / [`DEFAULT_POLL_INTERVAL`]. That is what lets a
/// two-remote config name the branch once and still give the slow mirror
/// its own cadence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
  /// Git URL to fetch from.
  pub url:           String,
  /// Branch to track on this remote, if it differs from the scope's.
  pub branch:        Option<String>,
  /// Poll cadence for this remote, if it differs from the scope's.
  pub poll_interval: Option<Duration>,
}

impl RemoteSpec {
  /// A remote with no per-entry overrides, as the scalar `remote` form
  /// yields.
  #[must_use]
  pub const fn plain(url: String) -> Self {
    Self {
      url,
      branch: None,
      poll_interval: None,
    }
  }
}

/// One remote with every field filled in from the ladder.
///
/// The daemon schedules against this, so it never has to repeat the
/// fallback rules and two remotes can never disagree about what "unset"
/// meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRemote {
  /// Git URL to fetch from.
  pub url:           String,
  /// Branch to track.
  pub branch:        String,
  /// Poll cadence.
  pub poll_interval: Duration,
}

// ---------------------------------------------------------------------------
// ConfigError  -  always points at something the user can edit
// ---------------------------------------------------------------------------

/// A config problem, reported at load time rather than at first use.
///
/// Both variants carry enough location for an editor to jump to: a file
/// and a line number, or the name of the offending environment variable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
  /// Something is wrong inside a config file.
  #[error("{}:{line}: {message}", path.display())]
  File {
    /// The file the problem is in.
    path:    PathBuf,
    /// 1-based line number of the offending key.
    line:    usize,
    /// What is wrong, without the location prefix.
    message: String,
  },

  /// Something is wrong in an `XI_SOURCES_*` variable.
  #[error("{var}: {message}")]
  Env {
    /// The variable name, e.g. `XI_SOURCES_POLL_INTERVAL`.
    var:     String,
    /// What is wrong, without the location prefix.
    message: String,
  },
}

/// 1-based line number of a byte offset within `raw`.
///
/// Shared with [`super::credentials`], which reports errors inside the
/// `[sources.credentials.*]` sub-tables with the same `<file>:<line>:`
/// shape this module uses for the flat keys.
pub(crate) fn line_of(raw: &str, offset: usize) -> usize {
  let clamped = offset.min(raw.len());
  raw[..clamped].bytes().filter(|b| *b == b'\n').count() + 1
}

// ---------------------------------------------------------------------------
// Value validation  -  shared by the TOML and env parsers
// ---------------------------------------------------------------------------

/// Schemes xi knows how to clone from.
const REMOTE_SCHEMES: &[&str] = &["https", "http", "ssh", "git", "file"];

/// Reject a remote URL xi could never clone, at load time.
///
/// Accepts the three forms git itself accepts and xi supports: an explicit
/// `scheme://host/path` URL, git's scp-like `[user@]host:path`, and an
/// absolute local path (a local mirror or a test fixture). Everything else
/// is a typo.
///
/// # Errors
///
/// Returns the reason the URL is unusable, without a location prefix; the
/// caller adds the file+line or variable name.
pub fn validate_remote_url(raw: &str) -> Result<(), String> {
  let url = raw.trim();
  if url.is_empty() {
    return Err("invalid remote URL: empty".to_owned());
  }

  if let Some(idx) = url.find("://") {
    let scheme = url[..idx].to_ascii_lowercase();
    let rest = &url[idx + 3..];
    if !REMOTE_SCHEMES.contains(&scheme.as_str()) {
      return Err(format!(
        "invalid remote URL '{url}': unsupported scheme '{scheme}' (expected \
         one of {})",
        REMOTE_SCHEMES.join(", ")
      ));
    }
    let authority = rest
      .split_once('/')
      .map_or(rest, |(authority, _)| authority);
    // `file://` is host-less by design; everything else needs a host.
    if scheme != "file" && authority.trim_start_matches("//").is_empty() {
      return Err(format!("invalid remote URL '{url}': missing host"));
    }
    return Ok(());
  }

  if let Some(colon) = scp_separator(url) {
    let (host, path) = url.split_at(colon);
    if host.is_empty() || path.len() <= 1 {
      return Err(format!(
        "invalid remote URL '{url}': scp-style remotes need both a host and a \
         path, as in 'git@host:owner/repo.git'"
      ));
    }
    return Ok(());
  }

  if Path::new(url).is_absolute() {
    return Ok(());
  }

  Err(format!(
    "invalid remote URL '{url}': expected an https:// or ssh:// URL, an \
     scp-style 'host:path', or an absolute local path"
  ))
}

/// Reject a path that cannot be resolved from an arbitrary CWD.
///
/// Sources config is read by daemons and by `xi switch` runs started from
/// anywhere on the machine, so a relative path has no stable meaning.
///
/// # Errors
///
/// Returns the reason the path is unusable, without a location prefix.
pub fn validate_abs_path(key: &str, raw: &str) -> Result<PathBuf, String> {
  let path = Path::new(raw.trim());
  if raw.trim().is_empty() {
    return Err(format!("'{key}' is empty"));
  }
  if !path.is_absolute() {
    return Err(format!(
      "'{key}' must be an absolute path, got '{}'",
      path.display()
    ));
  }
  Ok(path.to_path_buf())
}

/// Parse a duration string such as `60s` or `5m`.
///
/// # Errors
///
/// Returns the reason the duration is unusable, without a location prefix.
pub fn parse_poll_interval(raw: &str) -> Result<Duration, String> {
  let parsed = humantime::parse_duration(raw.trim()).map_err(|e| {
    format!("invalid 'poll-interval' value '{raw}': {e} (try '60s' or '5m')")
  })?;
  if parsed.is_zero() {
    return Err(
      "invalid 'poll-interval' value: must be greater than zero".to_owned(),
    );
  }
  Ok(parsed)
}

/// Reject a branch name git would not accept as a `--branch` argument.
///
/// # Errors
///
/// Returns the reason the name is unusable, without a location prefix.
pub fn validate_branch(raw: &str) -> Result<(), String> {
  let name = raw.trim();
  if name.is_empty() {
    return Err("'branch' is empty".to_owned());
  }
  if name.starts_with('-') {
    return Err(format!(
      "invalid 'branch' value '{name}': must not start with '-'"
    ));
  }
  if name.chars().any(char::is_whitespace) {
    return Err(format!(
      "invalid 'branch' value '{name}': must not contain whitespace"
    ));
  }
  Ok(())
}

/// Reject a hook that could never be run.
///
/// Hooks are handed to `sh -c` verbatim, so there is no quoting to check
/// here; an empty command is the only thing that is certainly a mistake.
///
/// # Errors
///
/// Returns the reason the hook is unusable, without a location prefix.
pub fn validate_hook(key: &str, raw: &str) -> Result<String, String> {
  let command = raw.trim();
  if command.is_empty() {
    return Err(format!("'{key}' is empty"));
  }
  Ok(command.to_owned())
}

/// Parse the `remote` key, whichever of its two shapes was written.
///
/// Accepts a scalar URL, an `[[sources.remote]]` array of tables, and the
/// inline `remote = [{ url = "..." }]` spelling of the same array, since
/// TOML treats the last two as equivalent and a config repo should not
/// have to care.
///
/// # Errors
///
/// Returns the reason the value is unusable, without a location prefix.
fn parse_remote_item(
  item: &toml_edit::Item,
) -> Result<Vec<RemoteSpec>, String> {
  if let Some(url) = item.as_str() {
    validate_remote_url(url)?;
    return Ok(vec![RemoteSpec::plain(url.trim().to_owned())]);
  }

  if let Some(entries) = item.as_array_of_tables() {
    return entries
      .iter()
      .enumerate()
      .map(|(idx, entry)| {
        parse_remote_entry(idx, entry.iter().map(|(key, _)| key), |key| {
          entry.get(key).and_then(toml_edit::Item::as_value)
        })
      })
      .collect();
  }

  if let Some(array) = item.as_array() {
    return array
      .iter()
      .enumerate()
      .map(|(idx, value)| {
        let inline = value.as_inline_table().ok_or_else(|| {
          format!(
            "invalid 'remote' entry {}: expected a table with a 'url' key",
            idx + 1
          )
        })?;
        parse_remote_entry(idx, inline.iter().map(|(key, _)| key), |key| {
          inline.get(key)
        })
      })
      .collect();
  }

  Err(
    "'remote' must be a string URL or an array of [[sources.remote]] tables"
      .to_owned(),
  )
}

/// Validate one `[[sources.remote]]` entry, by index for the error text.
///
/// The entry's own line is not reachable here, so every message names the
/// 1-based entry number instead; the caller's location prefix points at
/// the `remote` key itself.
fn parse_remote_entry<'item>(
  idx: usize,
  keys: impl Iterator<Item = &'item str>,
  get: impl Fn(&str) -> Option<&'item toml_edit::Value>,
) -> Result<RemoteSpec, String> {
  let at = idx + 1;
  for key in keys {
    if !REMOTE_ENTRY_KEYS.contains(&key) {
      return Err(format!(
        "invalid 'remote' entry {at}: unknown key '{key}' (expected one of {})",
        REMOTE_ENTRY_KEYS.join(", ")
      ));
    }
  }
  let string = |key: &str| -> Result<Option<&str>, String> {
    get(key).map_or_else(
      || Ok(None),
      |value| {
        value.as_str().map(Some).ok_or_else(|| {
          format!("invalid 'remote' entry {at}: '{key}' must be a string")
        })
      },
    )
  };

  let url = string("url")?.ok_or_else(|| {
    format!("invalid 'remote' entry {at}: missing required key 'url'")
  })?;
  validate_remote_url(url).map_err(|m| format!("'remote' entry {at}: {m}"))?;

  let branch = match string("branch")? {
    Some(branch) => {
      validate_branch(branch)
        .map_err(|m| format!("'remote' entry {at}: {m}"))?;
      Some(branch.trim().to_owned())
    },
    None => None,
  };
  let poll_interval = match string("poll-interval")? {
    Some(raw) => {
      Some(
        parse_poll_interval(raw)
          .map_err(|m| format!("'remote' entry {at}: {m}"))?,
      )
    },
    None => None,
  };

  Ok(RemoteSpec {
    url: url.trim().to_owned(),
    branch,
    poll_interval,
  })
}

/// Build the "one of a, b, c" tail shared by every closed-enum error.
fn one_of(key: &str, raw: &str, allowed: &[&str]) -> String {
  format!(
    "invalid '{key}' value '{raw}': expected one of {}",
    allowed.join(", ")
  )
}

// ---------------------------------------------------------------------------
// SourcesConfig  -  one partially specified layer
// ---------------------------------------------------------------------------

/// One layer of sources settings: every key optional, nothing defaulted.
///
/// A layer says only what its surface actually set, so stacking layers in
/// [`Layers::resolve`] is a plain "first `Some` wins" walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourcesConfig {
  /// Remote git URL of the config repo.
  ///
  /// With an `[[sources.remote]]` array this is the first entry's URL, so
  /// the [`super::SourcesId`] identity of a multi-remote source is the
  /// primary remote and does not change when a mirror is added.
  pub remote:          Option<String>,
  /// Every remote to poll, in config order.
  ///
  /// `None` and a one-element list are different: the former means this
  /// layer said nothing about remotes, the latter that it named exactly
  /// one. Only the former is overridden by a lower rung.
  pub remotes:         Option<Vec<RemoteSpec>>,
  /// Branch to track. Defaults to [`DEFAULT_BRANCH`].
  pub branch:          Option<String>,
  /// The operator's editable clone, so `xi switch` works from any CWD.
  pub working_dir:     Option<PathBuf>,
  /// How often the gitops daemon re-checks the remote.
  pub poll_interval:   Option<Duration>,
  /// Refuse to advance past unsigned commits.
  pub require_signed:  Option<bool>,
  /// SSH `allowed_signers` file, one of the two trust roots.
  pub allowed_signers: Option<PathBuf>,
  /// GPG public keyring, the other trust root.
  pub gpg_public_keys: Option<PathBuf>,
  /// Working-dir reconciliation policy.
  pub sync_mode:       Option<SyncMode>,
  /// Which Nix installer a bootstrap run should use.
  pub installer:       Option<Installer>,
  /// When a reboot-requiring activation may run.
  pub reboot_window:   Option<RebootWindow>,
  /// Shell command run after a successful fast-forward.
  pub on_advance:      Option<String>,
  /// Shell command run after a successful build.
  pub on_build:        Option<String>,
  /// Shell command run after a successful activation.
  pub on_switch:       Option<String>,
  /// The `[sources.credentials.*]` sub-tree.
  ///
  /// Not an `Option`: the type is itself all-optional, and its
  /// `is_empty` says whether this layer set anything. Stacking happens
  /// leaf by leaf in [`super::credentials::CredentialsConfig::resolve`].
  pub credentials:     super::credentials::CredentialsConfig,
  /// Secrets declared by `[sources.secrets.<name>]`, keyed by name.
  ///
  /// Not an `Option` like the scalars: the ladder merges secrets by name
  /// rather than letting one rung replace the whole set, so an empty map
  /// is exactly "this layer declared none". See [`super::secrets`].
  pub secrets:         BTreeMap<String, SecretSpec>,
}

impl SourcesConfig {
  /// True when this layer sets nothing at all.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    *self == Self::default()
  }

  /// Parse the `[sources]` table out of a `.xi.toml` / `sources.toml`.
  ///
  /// A document with no `[sources]` table is not an error: it yields an
  /// empty layer, so a `.xi.toml` that predates this capability keeps
  /// working.
  ///
  /// # Errors
  ///
  /// Returns a [`ConfigError::File`] naming `path` and the 1-based line of
  /// the offending key for a TOML syntax error, an unknown key, a
  /// wrongly-typed value, or a value that fails validation.
  pub fn from_toml_str(path: &Path, raw: &str) -> Result<Self, ConfigError> {
    // `Document`, not `DocumentMut`: only the immutable parse retains
    // source spans, and without spans every error would report line 1.
    let doc = toml_edit::Document::parse(raw).map_err(|e| {
      ConfigError::File {
        path:    path.to_path_buf(),
        line:    e.span().map_or(1, |span| line_of(raw, span.start)),
        message: e.message().to_owned(),
      }
    })?;

    let Some(table) = doc.get("sources").and_then(toml_edit::Item::as_table)
    else {
      return Ok(Self::default());
    };

    // Line of a key inside `[sources]`, falling back to the table header
    // when toml_edit has no span (a synthesised document).
    let line_for = |key: &str| -> usize {
      table
        .key(key)
        .and_then(toml_edit::Key::span)
        .or_else(|| table.span())
        .map_or(1, |span| line_of(raw, span.start))
    };
    let fail = |key: &str, message: String| {
      ConfigError::File {
        path: path.to_path_buf(),
        line: line_for(key),
        message,
      }
    };

    for (key, _) in table {
      if !SOURCES_KEYS.contains(&key) && !SOURCES_TABLE_KEYS.contains(&key) {
        return Err(fail(key, format!("unknown key '{key}' in [sources]")));
      }
    }

    // Read a key as a string, rejecting any other TOML type.
    let string = |key: &str| -> Result<Option<&str>, ConfigError> {
      table.get(key).map_or_else(
        || Ok(None),
        |item| {
          item
            .as_str()
            .map(Some)
            .ok_or_else(|| fail(key, format!("'{key}' must be a string")))
        },
      )
    };
    let boolean = |key: &str| -> Result<Option<bool>, ConfigError> {
      table.get(key).map_or_else(
        || Ok(None),
        |item| {
          item
            .as_bool()
            .map(Some)
            .ok_or_else(|| fail(key, format!("'{key}' must be a boolean")))
        },
      )
    };

    let mut config = Self::default();

    // `remote` is the one polymorphic key: a scalar URL, or an
    // `[[sources.remote]]` array for the multi-remote fan-in. Both are
    // normalised to the same `remotes` list so nothing downstream has to
    // care which spelling the operator used.
    if let Some(item) = table.get("remote") {
      let remotes = parse_remote_item(item).map_err(|m| fail("remote", m))?;
      config.remote = remotes.first().map(|first| first.url.clone());
      config.remotes = Some(remotes);
    }
    if let Some(branch) = string("branch")? {
      validate_branch(branch).map_err(|m| fail("branch", m))?;
      config.branch = Some(branch.trim().to_owned());
    }
    if let Some(dir) = string("working-dir")? {
      config.working_dir = Some(
        validate_abs_path("working-dir", dir)
          .map_err(|m| fail("working-dir", m))?,
      );
    }
    if let Some(raw_interval) = string("poll-interval")? {
      config.poll_interval = Some(
        parse_poll_interval(raw_interval)
          .map_err(|m| fail("poll-interval", m))?,
      );
    }
    config.require_signed = boolean("require-signed")?;
    if let Some(signers) = string("allowed-signers")? {
      config.allowed_signers = Some(
        validate_abs_path("allowed-signers", signers)
          .map_err(|m| fail("allowed-signers", m))?,
      );
    }
    if let Some(keys) = string("gpg-public-keys")? {
      config.gpg_public_keys = Some(
        validate_abs_path("gpg-public-keys", keys)
          .map_err(|m| fail("gpg-public-keys", m))?,
      );
    }
    if let Some(raw_window) = string("reboot-window")? {
      config.reboot_window = Some(
        RebootWindow::parse(raw_window)
          .map_err(|m| fail("reboot-window", m))?,
      );
    }
    for (key, slot) in [
      ("on-advance", &mut config.on_advance),
      ("on-build", &mut config.on_build),
      ("on-switch", &mut config.on_switch),
    ] {
      if let Some(command) = string(key)? {
        *slot = Some(validate_hook(key, command).map_err(|m| fail(key, m))?);
      }
    }
    if let Some(raw_mode) = string("sync-mode")? {
      config.sync_mode =
        Some(SyncMode::from_config_str(raw_mode).ok_or_else(|| {
          fail("sync-mode", one_of("sync-mode", raw_mode, SyncMode::ALL))
        })?);
    }
    if let Some(raw_installer) = string("installer")? {
      config.installer =
        Some(Installer::from_config_str(raw_installer).ok_or_else(|| {
          fail(
            "installer",
            one_of("installer", raw_installer, Installer::ALL),
          )
        })?);
    }
    if let Some(item) = table.get("credentials") {
      config.credentials =
        super::credentials::CredentialsConfig::from_toml_item(path, raw, item)?;
    }

    if let Some(item) = table.get("secrets") {
      let secrets = item.as_table().ok_or_else(|| {
        fail(
          "secrets",
          "[sources.secrets] must be a table of named secret stanzas"
            .to_owned(),
        )
      })?;
      config.secrets = parse_secrets_table(path, raw, secrets)?;
    }

    Ok(config)
  }

  /// Parse the `[sources]` table out of a config file on disk.
  ///
  /// A file that does not exist yields an empty layer; a file that exists
  /// but cannot be read or parsed is an error, because silently ignoring
  /// it would make the ladder lie about provenance.
  ///
  /// # Errors
  ///
  /// Returns [`ConfigError::File`] on an unreadable or invalid file.
  pub fn from_toml_file(path: &Path) -> Result<Self, ConfigError> {
    match std::fs::read_to_string(path) {
      Ok(raw) => Self::from_toml_str(path, &raw),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
      Err(e) => {
        Err(ConfigError::File {
          path:    path.to_path_buf(),
          line:    1,
          message: e.to_string(),
        })
      },
    }
  }

  /// Read the `XI_SOURCES_*` layer from an arbitrary environment.
  ///
  /// Taking the lookup as a closure keeps this testable without mutating
  /// the process environment, which no test can do safely in parallel.
  ///
  /// # Errors
  ///
  /// Returns [`ConfigError::Env`] naming the offending variable when a
  /// value fails the same validation the TOML layer applies.
  pub fn from_env_with(
    lookup: &dyn Fn(&str) -> Option<String>,
  ) -> Result<Self, ConfigError> {
    let get = |key: &str| lookup(&env_var_for(key)).filter(|v| !v.is_empty());
    let fail = |key: &str, message: String| {
      ConfigError::Env {
        var: env_var_for(key),
        message,
      }
    };

    let mut config = Self::default();

    // The env rung names one remote, not an array: a shell variable has
    // no way to carry per-remote branches and cadences, and inventing a
    // separator would make `XI_SOURCES_REMOTE` mean something different
    // from the `remote` key it mirrors. Multi-remote is a file-only
    // spelling; this rung still overrides the whole list, which is what
    // makes `XI_SOURCES_REMOTE=... xi nixos switch` a usable override on
    // a host whose `.xi.toml` lists two mirrors.
    if let Some(remote) = get("remote") {
      validate_remote_url(&remote).map_err(|m| fail("remote", m))?;
      let url = remote.trim().to_owned();
      config.remote = Some(url.clone());
      config.remotes = Some(vec![RemoteSpec::plain(url)]);
    }
    if let Some(branch) = get("branch") {
      validate_branch(&branch).map_err(|m| fail("branch", m))?;
      config.branch = Some(branch.trim().to_owned());
    }
    if let Some(dir) = get("working-dir") {
      config.working_dir = Some(
        validate_abs_path("working-dir", &dir)
          .map_err(|m| fail("working-dir", m))?,
      );
    }
    if let Some(interval) = get("poll-interval") {
      config.poll_interval = Some(
        parse_poll_interval(&interval).map_err(|m| fail("poll-interval", m))?,
      );
    }
    if let Some(flag) = get("require-signed") {
      config.require_signed = Some(parse_bool(&flag).ok_or_else(|| {
        fail(
          "require-signed",
          format!(
            "invalid 'require-signed' value '{flag}': expected one of true, \
             false, 1, 0"
          ),
        )
      })?);
    }
    if let Some(signers) = get("allowed-signers") {
      config.allowed_signers = Some(
        validate_abs_path("allowed-signers", &signers)
          .map_err(|m| fail("allowed-signers", m))?,
      );
    }
    if let Some(keys) = get("gpg-public-keys") {
      config.gpg_public_keys = Some(
        validate_abs_path("gpg-public-keys", &keys)
          .map_err(|m| fail("gpg-public-keys", m))?,
      );
    }
    if let Some(window) = get("reboot-window") {
      config.reboot_window = Some(
        RebootWindow::parse(&window).map_err(|m| fail("reboot-window", m))?,
      );
    }
    for (key, slot) in [
      ("on-advance", &mut config.on_advance),
      ("on-build", &mut config.on_build),
      ("on-switch", &mut config.on_switch),
    ] {
      if let Some(command) = get(key) {
        *slot = Some(validate_hook(key, &command).map_err(|m| fail(key, m))?);
      }
    }
    if let Some(mode) = get("sync-mode") {
      config.sync_mode =
        Some(SyncMode::from_config_str(&mode).ok_or_else(|| {
          fail("sync-mode", one_of("sync-mode", &mode, SyncMode::ALL))
        })?);
    }
    if let Some(installer) = get("installer") {
      config.installer =
        Some(Installer::from_config_str(&installer).ok_or_else(|| {
          fail("installer", one_of("installer", &installer, Installer::ALL))
        })?);
    }
    config.credentials =
      super::credentials::CredentialsConfig::from_env_with(lookup)?;

    Ok(config)
  }

  /// Read the `XI_SOURCES_*` layer from the process environment.
  ///
  /// # Errors
  ///
  /// See [`SourcesConfig::from_env_with`].
  pub fn from_env() -> Result<Self, ConfigError> {
    Self::from_env_with(&|key| std::env::var(key).ok())
  }
}

/// Accept the spellings an env var realistically carries for a boolean.
///
/// Shared with [`super::credentials`] so `use-agent` and
/// `anonymous.allow` take the same spellings `require-signed` does.
pub(super) fn parse_bool(raw: &str) -> Option<bool> {
  match raw.trim().to_ascii_lowercase().as_str() {
    "true" | "1" | "yes" | "on" => Some(true),
    "false" | "0" | "no" | "off" => Some(false),
    _ => None,
  }
}

// ---------------------------------------------------------------------------
// Filesystem path table
// ---------------------------------------------------------------------------

/// The three paths this capability owns, for one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePaths {
  /// The managed clone of the config repo.
  pub sources_dir: PathBuf,
  /// Flat directory of small metadata files. Never holds source code.
  pub state_dir:   PathBuf,
  /// The Nix-module-written config file for this scope.
  pub config_file: PathBuf,
}

impl ScopePaths {
  /// Resolve the table for `scope` against the process environment.
  #[must_use]
  pub fn resolve(scope: Scope) -> Self {
    Self::resolve_with(scope, &|key| std::env::var(key).ok())
  }

  /// Resolve the table against an arbitrary environment.
  ///
  /// System paths are fixed; user paths follow the XDG Base Directory
  /// spec, including its fallbacks when the `XDG_*` variables are unset or
  /// empty. Taking the lookup as a closure is what makes the fallbacks
  /// testable without mutating the process environment.
  ///
  /// `XI_SOURCES_STATE_DIR` overrides the state dir at either scope. It is
  /// an env-only escape hatch, not a `[sources]` key: the state dir is
  /// where xi records what it already did, so letting a repo-committed
  /// `.xi.toml` redirect it would let a config repo rewrite the history
  /// of its own deployment. Only an absolute path is honoured.
  #[must_use]
  pub fn resolve_with(
    scope: Scope,
    lookup: &dyn Fn(&str) -> Option<String>,
  ) -> Self {
    let mut paths = match scope {
      Scope::System => {
        Self {
          sources_dir: PathBuf::from("/var/lib/xi/sources"),
          state_dir:   PathBuf::from("/var/lib/xi/state"),
          config_file: PathBuf::from("/etc/xi/sources.toml"),
        }
      },
      Scope::User => {
        Self {
          sources_dir: xdg_base(lookup, "XDG_DATA_HOME", ".local/share")
            .join("xi")
            .join("sources"),
          state_dir:   xdg_base(lookup, "XDG_STATE_HOME", ".local/state")
            .join("xi"),
          config_file: xdg_base(lookup, "XDG_CONFIG_HOME", ".config")
            .join("xi")
            .join("sources.toml"),
        }
      },
    };

    if let Some(dir) = lookup(STATE_DIR_ENV)
      .filter(|d| !d.is_empty())
      .map(PathBuf::from)
      .filter(|d| d.is_absolute())
    {
      paths.state_dir = dir;
    }

    paths
  }
}

/// Env-only override for the resolved state dir. See
/// [`ScopePaths::resolve_with`] for why it has no `[sources]` counterpart.
pub const STATE_DIR_ENV: &str = "XI_SOURCES_STATE_DIR";

/// `$XDG_STATE_HOME`, with the spec's `$HOME/.local/state` fallback.
///
/// Exposed because the gitops daemon's named Nix profile lives under the
/// same base dir as the state dir (`<state home>/nix/profiles/xi-sources`)
/// without being *inside* the state dir, and a second copy of the XDG
/// fallback rule is a second thing to get wrong.
#[must_use]
pub fn xdg_state_home(lookup: &dyn Fn(&str) -> Option<String>) -> PathBuf {
  xdg_base(lookup, "XDG_STATE_HOME", ".local/state")
}

/// One XDG base dir, with the spec's `$HOME`-relative fallback.
///
/// An unset *or empty* variable falls back, per the XDG spec's "if
/// $`XDG_FOO` is either not set or empty" wording.
pub(crate) fn xdg_base(
  lookup: &dyn Fn(&str) -> Option<String>,
  var: &str,
  home_relative: &str,
) -> PathBuf {
  if let Some(dir) = lookup(var).filter(|d| !d.is_empty()) {
    return PathBuf::from(dir);
  }
  lookup("HOME").filter(|h| !h.is_empty()).map_or_else(
    || PathBuf::from("/tmp"),
    |home| PathBuf::from(home).join(home_relative),
  )
}

// ---------------------------------------------------------------------------
// The precedence ladder
// ---------------------------------------------------------------------------

/// The four configurable rungs of the ladder, highest precedence first.
///
/// The fifth rung (built-in defaults) is not a layer because it is not
/// configurable; [`Layers::resolve`] supplies it.
#[derive(Debug, Clone, Default)]
pub struct Layers {
  /// Values named by CLI flags on this invocation.
  pub cli:        SourcesConfig,
  /// Values read from `XI_SOURCES_*`.
  pub env:        SourcesConfig,
  /// Values read from the project's `.xi.toml [sources]`.
  pub toml:       SourcesConfig,
  /// Values read from the Nix-module-written per-scope `sources.toml`.
  pub nix_module: SourcesConfig,
}

impl Layers {
  /// Walk the ladder, recording where each answer came from.
  #[must_use]
  pub fn resolve(&self) -> ResolvedSources {
    let mut provenance = BTreeMap::new();

    // One rung-walk per key. The closure returns the first layer that set
    // the key and records its provenance; `default` fills the last rung.
    macro_rules! pick {
      ($key:literal, $field:ident, $default:expr) => {{
        let found = [
          (Provenance::CliFlag, self.cli.$field.clone()),
          (Provenance::EnvVar, self.env.$field.clone()),
          (Provenance::TomlFile, self.toml.$field.clone()),
          (Provenance::NixModule, self.nix_module.$field.clone()),
        ]
        .into_iter()
        .find_map(|(rung, value)| value.map(|v| (rung, v)));

        match found {
          Some((rung, value)) => {
            provenance.insert($key.to_owned(), rung);
            Some(value)
          },
          None => {
            provenance.insert($key.to_owned(), Provenance::Default);
            $default
          },
        }
      }};
    }

    ResolvedSources {
      remote:          pick!("remote", remote, None),
      remotes:         pick!("remote", remotes, None).unwrap_or_default(),
      branch:          pick!("branch", branch, Some(DEFAULT_BRANCH.to_owned()))
        .unwrap_or_else(|| DEFAULT_BRANCH.to_owned()),
      working_dir:     pick!("working-dir", working_dir, None),
      poll_interval:   pick!(
        "poll-interval",
        poll_interval,
        Some(DEFAULT_POLL_INTERVAL)
      )
      .unwrap_or(DEFAULT_POLL_INTERVAL),
      require_signed:  pick!("require-signed", require_signed, Some(false))
        .unwrap_or(false),
      allowed_signers: pick!("allowed-signers", allowed_signers, None),
      gpg_public_keys: pick!("gpg-public-keys", gpg_public_keys, None),
      sync_mode:       pick!("sync-mode", sync_mode, Some(SyncMode::Prompt))
        .unwrap_or_default(),
      installer:       pick!("installer", installer, None),
      reboot_window:   pick!(
        "reboot-window",
        reboot_window,
        Some(RebootWindow::Always)
      )
      .unwrap_or_default(),
      on_advance:      pick!("on-advance", on_advance, None),
      on_build:        pick!("on-build", on_build, None),
      on_switch:       pick!("on-switch", on_switch, None),
      // The credentials sub-tree walks the same four rungs, leaf by
      // leaf, and records its provenance under dotted keys in the same
      // map, so `--explain` reports `credentials.ssh.identity-file`
      // exactly the way it reports `branch`.
      credentials:     super::credentials::CredentialsConfig::resolve(
        [
          (Provenance::CliFlag, &self.cli.credentials),
          (Provenance::EnvVar, &self.env.credentials),
          (Provenance::TomlFile, &self.toml.credentials),
          (Provenance::NixModule, &self.nix_module.credentials),
        ],
        &mut provenance,
      ),
      // Secrets do not go through `pick!`: a record set merges by name,
      // so a machine-local stanza can add a secret without having to
      // restate every secret the repo-wide file already declared.
      secrets:         [
        &self.nix_module.secrets,
        &self.toml.secrets,
        &self.env.secrets,
        &self.cli.secrets,
      ]
      .into_iter()
      .fold(BTreeMap::new(), |acc, layer| merge_secrets(layer, &acc)),
      provenance:      Provenances(provenance),
    }
  }
}

/// The `provenance` map of `xi sources status --explain`.
///
/// A newtype so the JSON shape (`{"branch": "cli-flag", ...}`) is pinned
/// by one type rather than by every call site that builds the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Provenances(pub BTreeMap<String, Provenance>);

impl Provenances {
  /// Where the value for `key` came from, if the key is known.
  #[must_use]
  pub fn get(&self, key: &str) -> Option<Provenance> {
    self.0.get(key).copied()
  }
}

/// Every sources key, resolved, with defaults applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSources {
  /// Primary remote git URL, if any layer named one.
  pub remote:          Option<String>,
  /// Every remote to poll, in config order. Empty when none was named.
  pub remotes:         Vec<RemoteSpec>,
  /// Branch to track. Never empty: defaults to [`DEFAULT_BRANCH`].
  pub branch:          String,
  /// The operator's editable clone, if pinned.
  pub working_dir:     Option<PathBuf>,
  /// Gitops poll cadence. Defaults to [`DEFAULT_POLL_INTERVAL`].
  pub poll_interval:   Duration,
  /// Whether unsigned commits block an advance. Defaults to `false`.
  pub require_signed:  bool,
  /// SSH `allowed_signers` file.
  pub allowed_signers: Option<PathBuf>,
  /// GPG public keyring.
  pub gpg_public_keys: Option<PathBuf>,
  /// Working-dir reconciliation policy. Defaults to [`SyncMode::Prompt`].
  pub sync_mode:       SyncMode,
  /// Installer choice, left to `xi-sources-bootstrap` when unset.
  pub installer:       Option<Installer>,
  /// When a reboot-requiring activation may run. Defaults to
  /// [`RebootWindow::Always`], i.e. no gate.
  pub reboot_window:   RebootWindow,
  /// Hook run after a successful fast-forward.
  pub on_advance:      Option<String>,
  /// Hook run after a successful build.
  pub on_build:        Option<String>,
  /// Hook run after a successful activation.
  pub on_switch:       Option<String>,
  /// The resolved credential surface for every git subprocess.
  pub credentials:     super::credentials::ResolvedCredentials,
  /// Secrets `xi push` uploads out-of-band, merged by name across rungs.
  pub secrets:         BTreeMap<String, SecretSpec>,
  /// Which rung answered for each key.
  pub provenance:      Provenances,
}

impl ResolvedSources {
  /// Reject the one combination the schema cannot express on its own.
  ///
  /// `require-signed = true` with neither trust root configured would let
  /// the gitops daemon "verify" against an empty trust root, which is
  /// worse than not verifying at all because it looks safe. Either root
  /// alone is enough: a repo may be SSH-signed, GPG-signed, or both, and
  /// the daemon accepts a tip that verifies against any configured root.
  ///
  /// # Errors
  ///
  /// Returns the reason, without a location prefix.
  pub fn check_consistency(&self) -> Result<(), String> {
    if self.require_signed
      && self.allowed_signers.is_none()
      && self.gpg_public_keys.is_none()
    {
      return Err(
        "'require-signed = true' needs 'allowed-signers' to point at an SSH \
         allowed_signers file, or 'gpg-public-keys' to point at a GPG keyring"
          .to_owned(),
      );
    }
    self.credentials.check_consistency()
  }

  /// Every remote to poll, with per-entry gaps filled from the ladder.
  ///
  /// The scalar `remote` spelling yields exactly one entry, so the daemon
  /// schedules a single-remote and a multi-remote source through the same
  /// code path. An unconfigured source yields an empty list rather than a
  /// placeholder: there is nothing to poll, and a daemon asked to start
  /// on one should say so rather than fetch a made-up URL.
  #[must_use]
  pub fn effective_remotes(&self) -> Vec<ResolvedRemote> {
    let specs: Vec<RemoteSpec> = if self.remotes.is_empty() {
      self
        .remote
        .clone()
        .map(RemoteSpec::plain)
        .into_iter()
        .collect()
    } else {
      self.remotes.clone()
    };

    specs
      .into_iter()
      .map(|spec| {
        ResolvedRemote {
          url:           spec.url,
          branch:        spec.branch.unwrap_or_else(|| self.branch.clone()),
          poll_interval: spec.poll_interval.unwrap_or(self.poll_interval),
        }
      })
      .collect()
  }

  /// The three hooks, paired with their config key names.
  ///
  /// Returned as a fixed-length array rather than a map so a new hook
  /// point cannot be added on one side and forgotten on the other.
  #[must_use]
  pub fn hooks(&self) -> [(&'static str, Option<&str>); 3] {
    [
      ("on-advance", self.on_advance.as_deref()),
      ("on-build", self.on_build.as_deref()),
      ("on-switch", self.on_switch.as_deref()),
    ]
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod multi_remote_tests {
  //! Owned by `xi-sources-gitops` section 5.1: the `[[sources.remote]]`
  //! array and its per-entry fallback to the `[sources]`-level `branch`
  //! and `poll-interval` lives here because it is parsed alongside every
  //! other `[sources]` key, but "every remote to poll, with per-entry
  //! gaps filled" is the multi-remote fan-in capability's own
  //! requirement, not the config loader's.
  //!
  //! `effective_remotes` had no test coverage at all before this change -
  //! neither the TOML array-of-tables parse path nor the fallback
  //! arithmetic.

  use std::time::Duration;

  use tempfile::TempDir;

  use super::{Layers, RemoteSpec, SourcesConfig};

  fn toml_layer(body: &str) -> SourcesConfig {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join(".xi.toml");
    std::fs::write(&path, body).expect("write");
    SourcesConfig::from_toml_file(&path).expect("parse")
  }

  #[test]
  fn an_unconfigured_source_has_no_remotes_to_poll() {
    let resolved = Layers::default().resolve();
    assert!(
      resolved.effective_remotes().is_empty(),
      "nothing configured means nothing to fetch"
    );
  }

  #[test]
  fn a_scalar_remote_yields_exactly_one_resolved_entry() {
    let toml = toml_layer(
      "[sources]\nremote = \"https://example.org/cfg\"\nbranch = \
       \"trunk\"\npoll-interval = \"2m\"\n",
    );
    let resolved = Layers {
      toml,
      ..Layers::default()
    }
    .resolve();

    let remotes = resolved.effective_remotes();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].url, "https://example.org/cfg");
    assert_eq!(remotes[0].branch, "trunk");
    assert_eq!(remotes[0].poll_interval, Duration::from_secs(120));
  }

  /// SC-G-005: two remotes, each independently resolvable, is the shape
  /// the daemon schedules against.
  #[test]
  fn an_array_of_tables_parses_into_two_remote_specs() {
    let toml = toml_layer(
      "[sources]\nbranch = \"main\"\npoll-interval = \"1m\"\n\n\
       [[sources.remote]]\nurl = \"https://github.example/cfg\"\n\n\
       [[sources.remote]]\nurl = \"https://gitlab.example/cfg-mirror\"\n\
       branch = \"mirror\"\npoll-interval = \"5m\"\n",
    );
    let resolved = Layers {
      toml,
      ..Layers::default()
    }
    .resolve();

    let remotes = resolved.effective_remotes();
    assert_eq!(remotes.len(), 2, "both entries must parse");

    // The first entry names neither `branch` nor `poll-interval`, so
    // both fall back to the `[sources]`-level values.
    assert_eq!(remotes[0].url, "https://github.example/cfg");
    assert_eq!(remotes[0].branch, "main");
    assert_eq!(remotes[0].poll_interval, Duration::from_secs(60));

    // The second entry overrides both, and its own values win.
    assert_eq!(remotes[1].url, "https://gitlab.example/cfg-mirror");
    assert_eq!(remotes[1].branch, "mirror");
    assert_eq!(remotes[1].poll_interval, Duration::from_secs(300));
  }

  /// The inline-array spelling (`remote = [{ url = "..." }]`) is TOML's
  /// other way of writing the same array of tables, and
  /// `parse_remote_item` accepts both; `effective_remotes` must not care
  /// which one the operator used.
  #[test]
  fn the_inline_array_spelling_resolves_identically_to_the_table_array() {
    let toml = toml_layer(
      "[sources]\nbranch = \"main\"\nremote = [{ url = \
       \"https://example.org/one\" }, { url = \"https://example.org/two\", \
       branch = \"dev\" }]\n",
    );
    let resolved = Layers {
      toml,
      ..Layers::default()
    }
    .resolve();

    let remotes = resolved.effective_remotes();
    assert_eq!(remotes.len(), 2);
    assert_eq!(remotes[0].branch, "main");
    assert_eq!(remotes[1].branch, "dev");
  }

  /// A remote spec built directly (as the Nix-module layer or `xi
  /// sources init` would) round-trips through `effective_remotes` the
  /// same way a parsed one does - the fallback logic does not depend on
  /// having come from TOML.
  #[test]
  fn effective_remotes_fills_gaps_from_the_scope_level_defaults() {
    let toml = SourcesConfig {
      branch: Some("main".to_owned()),
      poll_interval: Some(Duration::from_secs(60)),
      remotes: Some(vec![
        RemoteSpec::plain("https://example.org/a".to_owned()),
        RemoteSpec {
          url:           "https://example.org/b".to_owned(),
          branch:        Some("staging".to_owned()),
          poll_interval: Some(Duration::from_secs(30)),
        },
      ]),
      ..SourcesConfig::default()
    };
    let resolved = Layers {
      toml,
      ..Layers::default()
    }
    .resolve();

    let remotes = resolved.effective_remotes();
    assert_eq!(remotes[0].branch, "main");
    assert_eq!(remotes[0].poll_interval, Duration::from_secs(60));
    assert_eq!(remotes[1].branch, "staging");
    assert_eq!(remotes[1].poll_interval, Duration::from_secs(30));
  }
}

// ---------------------------------------------------------------------------
// Writing the block back out  -  `xi sources init`
// ---------------------------------------------------------------------------

/// What `xi sources init` did to the config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockWrite {
  /// The file did not exist and was created with just a `[sources]` block.
  Created,
  /// The file existed without a `[sources]` block; one was appended.
  Appended,
  /// An existing `[sources]` block was replaced under `--force`.
  Replaced,
}

/// Why `xi sources init` could not write the block.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
  /// The file already carries a `[sources]` block and `--force` was not
  /// given. Nothing was modified.
  #[error(
    "{}: [sources] is already configured; pass --force to replace it",
    path.display()
  )]
  AlreadyConfigured {
    /// The file that already has a block.
    path: PathBuf,
  },

  /// The existing file could not be parsed, so appending to it safely is
  /// impossible.
  #[error(transparent)]
  Invalid(#[from] ConfigError),

  /// The file could not be read or written.
  #[error("{}: {source}", path.display())]
  Io {
    /// The file that could not be read or written.
    path:   PathBuf,
    /// The underlying failure.
    #[source]
    source: std::io::Error,
  },
}

/// Render a remote list as the `[[sources.remote]]` array of tables.
fn remote_array(entries: &[RemoteSpec]) -> toml_edit::ArrayOfTables {
  let mut array = toml_edit::ArrayOfTables::new();
  for entry in entries {
    let mut row = toml_edit::Table::new();
    row.insert("url", toml_edit::value(entry.url.as_str()));
    if let Some(branch) = entry.branch.as_ref() {
      row.insert("branch", toml_edit::value(branch.as_str()));
    }
    if let Some(interval) = entry.poll_interval {
      row.insert(
        "poll-interval",
        toml_edit::value(humantime::format_duration(interval).to_string()),
      );
    }
    array.push(row);
  }
  array
}

impl SourcesConfig {
  /// Render this layer as a `[sources]` TOML table.
  ///
  /// Only keys this layer actually set are emitted, so `xi sources init`
  /// writes a minimal block rather than freezing today's defaults into
  /// the user's file.
  #[must_use]
  pub fn to_toml_table(&self) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();

    // The `remote` key is written first and on its own, because it is the
    // one key with two shapes. A list carrying no per-entry override
    // round-trips through the scalar spelling, so `xi sources init` on a
    // single-remote setup writes `remote = "..."` rather than a
    // one-element array; anything richer needs the array form to survive
    // a reload.
    match self.remotes.as_deref() {
      Some([only]) if only.branch.is_none() && only.poll_interval.is_none() => {
        table.insert("remote", toml_edit::value(only.url.as_str()));
      },
      Some(entries) if !entries.is_empty() => {
        table.insert(
          "remote",
          toml_edit::Item::ArrayOfTables(remote_array(entries)),
        );
      },
      Some(_) | None => {
        if let Some(remote) = self.remote.as_ref() {
          table.insert("remote", toml_edit::value(remote.as_str()));
        }
      },
    }

    let mut put = |key: &str, value: toml_edit::Value| {
      table.insert(key, toml_edit::Item::Value(value));
    };

    if let Some(branch) = self.branch.as_ref() {
      put("branch", branch.as_str().into());
    }
    if let Some(dir) = self.working_dir.as_ref() {
      put("working-dir", dir.display().to_string().into());
    }
    if let Some(interval) = self.poll_interval {
      put(
        "poll-interval",
        humantime::format_duration(interval).to_string().into(),
      );
    }
    if let Some(flag) = self.require_signed {
      put("require-signed", flag.into());
    }
    if let Some(signers) = self.allowed_signers.as_ref() {
      put("allowed-signers", signers.display().to_string().into());
    }
    if let Some(keys) = self.gpg_public_keys.as_ref() {
      put("gpg-public-keys", keys.display().to_string().into());
    }
    if let Some(mode) = self.sync_mode {
      put("sync-mode", mode.as_str().into());
    }
    if let Some(installer) = self.installer {
      put("installer", installer.as_str().into());
    }
    if let Some(window) = self.reboot_window {
      put("reboot-window", window.to_string().into());
    }
    for (key, command) in [
      ("on-advance", self.on_advance.as_ref()),
      ("on-build", self.on_build.as_ref()),
      ("on-switch", self.on_switch.as_ref()),
    ] {
      if let Some(command) = command {
        put(key, command.as_str().into());
      }
    }
    // `put` borrows `table` mutably, so every scalar key has to be
    // written above this line: the borrow ends at the closure's last
    // use, which is what lets the sub-table inserts below take `table`
    // again.
    if let Some(credentials) = self.credentials.to_toml_table() {
      table.insert("credentials", toml_edit::Item::Table(credentials));
    }

    table
  }

  /// Write this layer into `path` as a `[sources]` block.
  ///
  /// Uses `toml_edit`'s format-preserving document model, so an existing
  /// file keeps its comments, key order and spacing; only the `[sources]`
  /// table is touched.
  ///
  /// # Errors
  ///
  /// Returns [`WriteError::AlreadyConfigured`] when `path` already has a
  /// `[sources]` block and `force` is false. Nothing is written in that
  /// case: the refusal must not be half-applied.
  pub fn write_block(
    &self,
    path: &Path,
    force: bool,
  ) -> Result<BlockWrite, WriteError> {
    let existing = match std::fs::read_to_string(path) {
      Ok(raw) => Some(raw),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
      Err(source) => {
        return Err(WriteError::Io {
          path: path.to_path_buf(),
          source,
        });
      },
    };

    let (mut doc, outcome) = match existing {
      None => (toml_edit::DocumentMut::new(), BlockWrite::Created),
      Some(raw) => {
        // Reuse the reader's error mapping so a malformed file reports
        // the same `<file>:<line>:` shape here as it does on load.
        let doc = raw.parse::<toml_edit::DocumentMut>().map_err(|e| {
          WriteError::Invalid(ConfigError::File {
            path:    path.to_path_buf(),
            line:    e.span().map_or(1, |span| line_of(&raw, span.start)),
            message: e.message().to_owned(),
          })
        })?;
        let has_block = doc.contains_key("sources");
        if has_block && !force {
          return Err(WriteError::AlreadyConfigured {
            path: path.to_path_buf(),
          });
        }
        let outcome = if has_block {
          BlockWrite::Replaced
        } else {
          BlockWrite::Appended
        };
        (doc, outcome)
      },
    };

    doc.insert("sources", toml_edit::Item::Table(self.to_toml_table()));

    if let Some(parent) = path.parent()
      && !parent.as_os_str().is_empty()
    {
      std::fs::create_dir_all(parent).map_err(|source| {
        WriteError::Io {
          path: parent.to_path_buf(),
          source,
        }
      })?;
    }
    std::fs::write(path, doc.to_string()).map_err(|source| {
      WriteError::Io {
        path: path.to_path_buf(),
        source,
      }
    })?;

    Ok(outcome)
  }
}

// ---------------------------------------------------------------------------
// Remote vs installable  -  what a bare `switch` positional means
// ---------------------------------------------------------------------------

/// How `xi <scope> switch <arg>` read its positional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionalKind {
  /// A config repo to adopt as this scope's source.
  ConfigRemote,
  /// A flake installable, evaluated exactly as it was before this
  /// capability existed.
  Installable,
}

/// Flake-reference schemes that xi already resolves as installables.
///
/// `git+https://` and friends are deliberately here rather than in the
/// remote list: a user who spells the `git+` prefix is asking for the
/// flake-reference reading, and that spelling predates this capability.
const INSTALLABLE_SCHEMES: &[&str] = &[
  "git+https",
  "git+ssh",
  "git+file",
  "github",
  "gitlab",
  "sourcehut",
  "path",
  "flake",
  "tarball",
  "file",
];

/// Decide whether a `switch` positional names a config repo or a flake.
///
/// The rule, from `openspec/changes/xi-sources-config/design.md`:
/// scp-style `user@host:path` and bare `https`/`http`/`ssh` URLs are
/// config-repo remotes; everything xi already parses as an installable
/// keeps its meaning. Requiring a flag was rejected because it breaks
/// the "same command from zero or on a managed machine" property.
///
/// This is shape-only and never touches the network or the filesystem,
/// so it cannot be slow and cannot behave differently across machines.
#[must_use]
pub fn classify_positional(raw: &str) -> PositionalKind {
  let arg = raw.trim();

  if let Some(idx) = arg.find("://") {
    let scheme = arg[..idx].to_ascii_lowercase();
    if INSTALLABLE_SCHEMES.contains(&scheme.as_str()) {
      return PositionalKind::Installable;
    }
    if matches!(scheme.as_str(), "https" | "http" | "ssh") {
      return PositionalKind::ConfigRemote;
    }
    return PositionalKind::Installable;
  }

  // Scheme-prefixed flake refs without `://`, e.g. `github:owner/repo`.
  // Checked before the scp rule, since `github:owner/repo` also matches
  // "colon before any slash".
  if let Some((prefix, _)) = arg.split_once(':')
    && INSTALLABLE_SCHEMES.contains(&prefix.to_ascii_lowercase().as_str())
  {
    return PositionalKind::Installable;
  }

  // Filesystem and store paths are installables.
  if arg.starts_with('.') || arg.starts_with('/') || arg.is_empty() {
    return PositionalKind::Installable;
  }

  // What is left that looks like git's scp form is a config remote.
  if scp_separator(arg).is_some() {
    return PositionalKind::ConfigRemote;
  }

  PositionalKind::Installable
}
