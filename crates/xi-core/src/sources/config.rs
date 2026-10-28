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

use super::{Scope, scp_separator};

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
  "scope",
  "sync-mode",
  "installer",
];

/// The `XI_SOURCES_*` environment variable for a `[sources]` key.
///
/// The rule is mechanical, per spec: uppercase the key and replace
/// hyphens with underscores.
#[must_use]
pub fn env_var_for(key: &str) -> String {
  format!("XI_SOURCES_{}", key.to_ascii_uppercase().replace('-', "_"))
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

/// What to do when the working dir and the sources dir disagree.
///
/// Owned by `xi-sources-conflict`; parsed here so it travels with the rest
/// of the sources config through the ladder.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
  /// Ask the operator. The default.
  #[default]
  Prompt,
  /// Copy the working dir over the sources dir without asking.
  AutoCopy,
  /// Never reconcile automatically.
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

/// `Scope` already carries `as_str()` and `Display` in the parent module,
/// so it only needs the parse side of the config spelling here.
impl Scope {
  /// Every accepted `[sources] scope` spelling.
  pub const ALL: &'static [&'static str] = &["system", "user"];

  /// Parse from the `.xi.toml` / env spelling.
  #[must_use]
  pub fn from_config_str(raw: &str) -> Option<Self> {
    match raw {
      "system" => Some(Self::System),
      "user" => Some(Self::User),
      _ => None,
    }
  }
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
fn line_of(raw: &str, offset: usize) -> usize {
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
  pub remote:          Option<String>,
  /// Branch to track. Defaults to [`DEFAULT_BRANCH`].
  pub branch:          Option<String>,
  /// The operator's editable clone, so `xi switch` works from any CWD.
  pub working_dir:     Option<PathBuf>,
  /// How often the gitops daemon re-checks the remote.
  pub poll_interval:   Option<Duration>,
  /// Refuse to advance past unsigned commits.
  pub require_signed:  Option<bool>,
  /// SSH `allowed_signers` file, required when `require_signed`.
  pub allowed_signers: Option<PathBuf>,
  /// Disambiguates when both scopes are configured and uid is 0.
  pub scope:           Option<Scope>,
  /// Working-dir reconciliation policy.
  pub sync_mode:       Option<SyncMode>,
  /// Which Nix installer a bootstrap run should use.
  pub installer:       Option<Installer>,
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
      if !SOURCES_KEYS.contains(&key) {
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

    if let Some(remote) = string("remote")? {
      validate_remote_url(remote).map_err(|m| fail("remote", m))?;
      config.remote = Some(remote.trim().to_owned());
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
    if let Some(raw_scope) = string("scope")? {
      config.scope =
        Some(Scope::from_config_str(raw_scope).ok_or_else(|| {
          fail("scope", one_of("scope", raw_scope, Scope::ALL))
        })?);
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

    if let Some(remote) = get("remote") {
      validate_remote_url(&remote).map_err(|m| fail("remote", m))?;
      config.remote = Some(remote.trim().to_owned());
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
    if let Some(scope) = get("scope") {
      config.scope = Some(
        Scope::from_config_str(&scope)
          .ok_or_else(|| fail("scope", one_of("scope", &scope, Scope::ALL)))?,
      );
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
fn parse_bool(raw: &str) -> Option<bool> {
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

/// One XDG base dir, with the spec's `$HOME`-relative fallback.
///
/// An unset *or empty* variable falls back, per the XDG spec's "if
/// $`XDG_FOO` is either not set or empty" wording.
fn xdg_base(
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
      scope:           pick!("scope", scope, None),
      sync_mode:       pick!("sync-mode", sync_mode, Some(SyncMode::Prompt))
        .unwrap_or_default(),
      installer:       pick!("installer", installer, None),
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
  /// Remote git URL, if any layer named one.
  pub remote:          Option<String>,
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
  /// Explicit scope, when the caller could not infer one.
  pub scope:           Option<Scope>,
  /// Working-dir reconciliation policy. Defaults to [`SyncMode::Prompt`].
  pub sync_mode:       SyncMode,
  /// Installer choice, left to `xi-sources-bootstrap` when unset.
  pub installer:       Option<Installer>,
  /// Which rung answered for each key.
  pub provenance:      Provenances,
}

impl ResolvedSources {
  /// Reject the one combination the schema cannot express on its own.
  ///
  /// `require-signed = true` without an `allowed-signers` file would let
  /// the gitops daemon "verify" against an empty trust root, which is
  /// worse than not verifying at all because it looks safe.
  ///
  /// # Errors
  ///
  /// Returns the reason, without a location prefix.
  pub fn check_consistency(&self) -> Result<(), String> {
    if self.require_signed && self.allowed_signers.is_none() {
      return Err(
        "'require-signed = true' needs 'allowed-signers' to point at an SSH \
         allowed_signers file"
          .to_owned(),
      );
    }
    Ok(())
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

impl SourcesConfig {
  /// Render this layer as a `[sources]` TOML table.
  ///
  /// Only keys this layer actually set are emitted, so `xi sources init`
  /// writes a minimal block rather than freezing today's defaults into
  /// the user's file.
  #[must_use]
  pub fn to_toml_table(&self) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    let mut put = |key: &str, value: toml_edit::Value| {
      table.insert(key, toml_edit::Item::Value(value));
    };

    if let Some(remote) = self.remote.as_ref() {
      put("remote", remote.as_str().into());
    }
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
    if let Some(scope) = self.scope {
      put("scope", scope.as_str().into());
    }
    if let Some(mode) = self.sync_mode {
      put("sync-mode", mode.as_str().into());
    }
    if let Some(installer) = self.installer {
      put("installer", installer.as_str().into());
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
