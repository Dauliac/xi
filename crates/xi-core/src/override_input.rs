//! The configuration cascade for `--override-input`.
//!
//! `--override-input` is the only Nix mechanism that beats `flake.lock`, so
//! it is the only way to point an input at a local checkout. This module
//! lets it be configured instead of retyped, while closing the three traps
//! raw Nix leaves open:
//!
//! - a bare path silently locks as `{"type":"git"}` and then hard-errors on
//!   untracked files, so every path value is emitted as `path:<abs>`;
//! - relative values resolve against the process CWD in raw Nix, so they are
//!   resolved here against the context the value was *written* in;
//! - a literal space stops `path:` parsing as a URL, so paths are
//!   percent-encoded.
//!
//! See `openspec/changes/xi-override-input-config/design.md` for the
//! measurements behind each rule.

use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
  path::{Component, Path, PathBuf},
};

use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum Error {
  #[error(
    "override_input value {value:?} for input {name:?} in the machine-global \
     config is relative; a machine-global config has no meaningful base \
     directory, so use an absolute or ~-prefixed path"
  )]
  RelativeInGlobalConfig { name: String, value: String },

  #[error(
    "XI_OVERRIDE_INPUT entry {token:?} is not a NAME=FLAKEREF pair (no '=')"
  )]
  EnvPairMissingEquals { token: String },

  #[error("XI_OVERRIDE_INPUT entry {token:?} has an empty input name")]
  EnvPairEmptyName { token: String },

  #[error(
    "override_input for {name:?} is {value:?}, which resolves to {resolved}  \
     -  that directory does not exist"
  )]
  PathMissing {
    name:     String,
    value:    String,
    resolved: String,
  },

  #[error(
    "override_input for {name:?} is {value:?}, which resolves to {resolved}  \
     -  that path is not a directory"
  )]
  PathNotADirectory {
    name:     String,
    value:    String,
    resolved: String,
  },

  #[error(
    "override_input for {name:?} is {value:?}, which resolves to {resolved}  \
     -  that directory has no flake.nix"
  )]
  PathHasNoFlake {
    name:     String,
    value:    String,
    resolved: String,
  },

  #[error(
    "cannot expand '~' in override_input value {value:?} for input {name:?}: \
     HOME is not set"
  )]
  NoHome { name: String, value: String },
}

/// Where an override value was written. This fixes two things: how a
/// relative path in it resolves, and where it sits in the cascade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverrideSource {
  /// `~/.config/xi/config.toml`. Relative values are refused: a
  /// machine-global file has no meaningful base, and silently picking `$HOME`
  /// or the CWD would be a guess.
  GlobalConfig,
  /// Committed project policy, `.xi.toml`. Base is the file's own directory.
  ProjectToml { dir: PathBuf },
  /// Gitignored machine state, `.xi.local.toml`. Base is the file's own
  /// directory.
  ProjectLocalToml { dir: PathBuf },
  /// `XI_OVERRIDE_INPUT`. Base is the flake root, so the value means the
  /// same thing from any subdirectory.
  Env { flake_root: PathBuf },
  /// `--override-input` typed at a prompt. Base is the process CWD, matching
  /// what Nix itself would do with that argument.
  Cli { cwd: PathBuf },
}

impl OverrideSource {
  /// Cascade rank, lowest priority first. Equal names at higher rank win.
  #[must_use]
  pub const fn rank(&self) -> u8 {
    match self {
      Self::GlobalConfig => 0,
      Self::ProjectToml { .. } => 1,
      Self::ProjectLocalToml { .. } => 2,
      Self::Env { .. } => 3,
      Self::Cli { .. } => 4,
    }
  }

  /// The directory a relative value in this source resolves against, or
  /// `None` when relative values are not allowed.
  #[must_use]
  pub fn base_dir(&self) -> Option<&Path> {
    match self {
      Self::GlobalConfig => None,
      Self::ProjectToml { dir } | Self::ProjectLocalToml { dir } => Some(dir),
      Self::Env { flake_root } => Some(flake_root),
      Self::Cli { cwd } => Some(cwd),
    }
  }

  /// Human-readable origin, used in the per-invocation report.
  #[must_use]
  pub const fn label(&self) -> &'static str {
    match self {
      Self::GlobalConfig => "config.toml",
      Self::ProjectToml { .. } => ".xi.toml",
      Self::ProjectLocalToml { .. } => ".xi.local.toml",
      Self::Env { .. } => "XI_OVERRIDE_INPUT",
      Self::Cli { .. } => "--override-input",
    }
  }
}

/// One override as written, before normalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
  pub name:   String,
  pub value:  String,
  pub source: OverrideSource,
}

impl Entry {
  pub fn new(
    name: impl Into<String>,
    value: impl Into<String>,
    source: OverrideSource,
  ) -> Self {
    Self {
      name: name.into(),
      value: value.into(),
      source,
    }
  }
}

/// One override after normalization, ready to emit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
  pub name:     String,
  /// Exactly what Nix receives as the second half of the flag.
  pub flakeref: String,
  /// The absolute path, when the value was a path. Reported to the user so a
  /// wrong base directory is diagnosable.
  pub path:     Option<PathBuf>,
  pub source:   OverrideSource,
}

/// Does this value name a filesystem path?
///
/// Requiring `./` for relative paths mirrors Nix's own installable
/// convention, where a bare word is an indirect flakeref. So a bare
/// `nixpkgs` is never guessed to be a directory and the rule has no
/// ambiguous cases.
#[must_use]
pub fn is_path_value(value: &str) -> bool {
  value.starts_with("path:")
    || value.starts_with('/')
    || value.starts_with("./")
    || value.starts_with("../")
    || value == "."
    || value == ".."
    || value.starts_with('~')
}

/// Lexically remove `.` and `..` components without touching the
/// filesystem.
///
/// Deliberately not `fs::canonicalize`: that resolves symlinks the user may
/// have chosen on purpose, and errors on paths that do not exist yet, which
/// would turn a clear xi error into an opaque io error.
#[must_use]
pub fn clean_lexical(path: &Path) -> PathBuf {
  let mut out: Vec<Component> = Vec::new();

  for comp in path.components() {
    match comp {
      Component::CurDir => {},
      Component::ParentDir => {
        match out.last() {
          Some(Component::Normal(_)) => {
            out.pop();
          },
          Some(Component::RootDir) => {},
          _ => out.push(comp),
        }
      },
      other => out.push(other),
    }
  }

  if out.is_empty() {
    return PathBuf::from(".");
  }

  out.iter().collect()
}

/// Percent-encode a path for use inside a `path:` flakeref.
///
/// `/` is preserved because it is the path separator the flakeref needs.
/// An existing valid `%XX` escape is left alone so a user who already
/// encoded their path does not get double-encoded; a stray `%` is escaped.
#[must_use]
pub fn percent_encode_path(raw: &str) -> String {
  const fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
  }

  let bytes = raw.as_bytes();
  let mut out = String::with_capacity(raw.len());

  let mut i = 0;
  while i < bytes.len() {
    let b = bytes[i];

    if b == b'%'
      && i + 2 < bytes.len()
      && bytes[i + 1].is_ascii_hexdigit()
      && bytes[i + 2].is_ascii_hexdigit()
    {
      // Already-encoded triplet: pass through verbatim.
      out.push('%');
      out.push(bytes[i + 1] as char);
      out.push(bytes[i + 2] as char);
      i += 3;
      continue;
    }

    if unreserved(b) {
      out.push(b as char);
    } else {
      let _ = write!(out, "%{b:02X}");
    }
    i += 1;
  }

  out
}

/// Parse `XI_OVERRIDE_INPUT` into `name` → `flakeref` pairs.
///
/// Pairs are separated by any run of whitespace, which makes the multi-line
/// `.envrc` form work for free. Whitespace is a safe separator precisely
/// because a flakeref cannot contain a raw space: a literal space makes
/// `path:` stop parsing as a URL.
///
/// Each pair splits on the **first** `=` only, so a flakeref's own query
/// string (`?ref=main&rev=abc`) survives.
///
/// # Errors
///
/// Returns an error naming the offending token when a pair has no `=` or an
/// empty input name. Silently dropping it would hide a typo that changes
/// what gets built.
pub fn parse_env_value(raw: &str) -> Result<Vec<(String, String)>, Error> {
  let mut pairs = Vec::new();

  for token in raw.split_whitespace() {
    let Some((name, value)) = token.split_once('=') else {
      return Err(Error::EnvPairMissingEquals {
        token: token.to_string(),
      });
    };

    if name.is_empty() {
      return Err(Error::EnvPairEmptyName {
        token: token.to_string(),
      });
    }

    pairs.push((name.to_string(), value.to_string()));
  }

  Ok(pairs)
}

/// Normalize one value into the flakeref Nix will receive.
///
/// Non-path values are forwarded verbatim. Path values go through the
/// five-step pipeline: tilde expansion, join with the source's base
/// directory, lexical clean, percent-encode, `path:` prefix.
///
/// # Errors
///
/// Returns an error when a relative value comes from the machine-global
/// config, when `~` cannot be expanded, or when the resolved path is not a
/// directory containing `flake.nix`.
pub fn normalize(entry: &Entry) -> Result<Resolved, Error> {
  let value = entry.value.as_str();

  if !is_path_value(value) {
    return Ok(Resolved {
      name:     entry.name.clone(),
      flakeref: value.to_string(),
      path:     None,
      source:   entry.source.clone(),
    });
  }

  // `path:../x` must be resolved too: strip the scheme, resolve, re-apply.
  let bare = value.strip_prefix("path:").unwrap_or(value);

  // 1. Expand a leading `~`. The shell does this for an unquoted env value but
  //    never inside a TOML string, so xi must.
  let expanded = expand_tilde(bare, entry)?;

  // 2. If relative, join with the base directory for this source.
  let absolute = if expanded.is_absolute() {
    expanded
  } else {
    let base = entry.source.base_dir().ok_or_else(|| {
      Error::RelativeInGlobalConfig {
        name:  entry.name.clone(),
        value: entry.value.clone(),
      }
    })?;
    base.join(expanded)
  };

  // 3. Clean `..` and `.` lexically, never via canonicalize.
  let cleaned = clean_lexical(&absolute);

  // Validate before encoding so the error carries a readable path.
  validate_flake_dir(&cleaned, entry)?;

  // 4 + 5. Percent-encode, then prefix. Never emit a bare path.
  let encoded = percent_encode_path(&cleaned.to_string_lossy());

  Ok(Resolved {
    name:     entry.name.clone(),
    flakeref: format!("path:{encoded}"),
    path:     Some(cleaned),
    source:   entry.source.clone(),
  })
}

fn expand_tilde(bare: &str, entry: &Entry) -> Result<PathBuf, Error> {
  if bare != "~" && !bare.starts_with("~/") {
    return Ok(PathBuf::from(bare));
  }

  let home = std::env::var("HOME")
    .ok()
    .filter(|h| !h.is_empty())
    .ok_or_else(|| {
      Error::NoHome {
        name:  entry.name.clone(),
        value: entry.value.clone(),
      }
    })?;

  if bare == "~" {
    return Ok(PathBuf::from(home));
  }

  Ok(PathBuf::from(home).join(&bare[2..]))
}

fn validate_flake_dir(dir: &Path, entry: &Entry) -> Result<(), Error> {
  let resolved = dir.display().to_string();

  if !dir.exists() {
    return Err(Error::PathMissing {
      name: entry.name.clone(),
      value: entry.value.clone(),
      resolved,
    });
  }
  if !dir.is_dir() {
    return Err(Error::PathNotADirectory {
      name: entry.name.clone(),
      value: entry.value.clone(),
      resolved,
    });
  }
  if !dir.join("flake.nix").exists() {
    return Err(Error::PathHasNoFlake {
      name: entry.name.clone(),
      value: entry.value.clone(),
      resolved,
    });
  }

  Ok(())
}

/// The input names of the flake being operated on, read from `flake.lock`.
///
/// A plain JSON read: no Nix evaluation, no network. Returns `None` when
/// there is no readable lock file, which means "unknown" and disables
/// filtering rather than dropping every override.
#[must_use]
pub fn flake_input_names(flake_dir: &Path) -> Option<BTreeSet<String>> {
  let raw = std::fs::read_to_string(flake_dir.join("flake.lock")).ok()?;
  let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;

  let inputs = doc
    .get("nodes")?
    .get("root")?
    .get("inputs")?
    .as_object()?
    .keys()
    .cloned()
    .collect::<BTreeSet<_>>();

  Some(inputs)
}

/// Resolve the whole cascade into the overrides to emit.
///
/// Merging is by input name, so `--override-input nix-lib <other>` replaces
/// only that entry and leaves the rest of the configured set applied.
/// Concatenating and relying on Nix's silent last-wins would build the same
/// thing but could not produce an honest report.
///
/// `known_inputs` drops overrides that name an input the target flake does
/// not have. Without it a single entry in the machine-global config would
/// print `warning: the flag '--override-input ...' does not match any input`
/// on every build in every unrelated repo. A transitive `a/b` key is checked
/// against the parent name `a` only, since `b`'s presence cannot be
/// established without resolving `a` first.
///
/// # Errors
///
/// Propagates the first normalization error, so a misconfigured override is
/// loud rather than silently skipped.
pub fn resolve(
  entries: &[Entry],
  known_inputs: Option<&BTreeSet<String>>,
) -> Result<Vec<Resolved>, Error> {
  // Merge by name, highest rank wins. Ties go to the later entry, which
  // matches Nix's last-wins for two flags naming the same input.
  let mut winners: BTreeMap<&str, &Entry> = BTreeMap::new();
  for entry in entries {
    match winners.get(entry.name.as_str()) {
      Some(existing) if existing.source.rank() > entry.source.rank() => {},
      _ => {
        winners.insert(entry.name.as_str(), entry);
      },
    }
  }

  let mut out = Vec::new();
  for entry in winners.into_values() {
    if let Some(known) = known_inputs {
      let parent = entry.name.split('/').next().unwrap_or(&entry.name);
      if !known.contains(parent) {
        continue;
      }
    }
    out.push(normalize(entry)?);
  }

  Ok(out)
}

/// Flatten resolved overrides into the flat `[name, flakeref, ...]` pair
/// representation the passthrough arg structs already use, so there stays
/// exactly one emission site per struct.
#[must_use]
pub fn to_flat_pairs(resolved: &[Resolved]) -> Vec<String> {
  let mut out = Vec::with_capacity(resolved.len() * 2);
  for r in resolved {
    out.push(r.name.clone());
    out.push(r.flakeref.clone());
  }
  out
}

/// Read the flat `[name, flakeref, ...]` pairs a passthrough struct holds
/// back into entries attributed to the command line.
#[must_use]
pub fn entries_from_flat_pairs(pairs: &[String], cwd: &Path) -> Vec<Entry> {
  pairs
    .chunks(2)
    .filter(|c| c.len() == 2)
    .map(|c| {
      Entry::new(c[0].clone(), c[1].clone(), OverrideSource::Cli {
        cwd: cwd.to_path_buf(),
      })
    })
    .collect()
}

/// Walk up from `start` looking for the nearest directory with a
/// `flake.nix`, which is the flake being operated on when no explicit flake
/// reference was given.
#[must_use]
pub fn discover_flake_dir(start: &Path) -> Option<PathBuf> {
  let mut dir = Some(start);
  while let Some(d) = dir {
    if d.join("flake.nix").exists() {
      return Some(d.to_path_buf());
    }
    dir = d.parent();
  }
  None
}

/// Everything needed to resolve the cascade for one invocation.
#[derive(Debug)]
pub struct ResolveContext<'a> {
  /// `build.override_input` from `~/.config/xi/config.toml`.
  pub global:           &'a BTreeMap<String, String>,
  /// Entries already tagged with their project file, from `.xi.toml` and
  /// `.xi.local.toml`.
  pub project:          &'a [Entry],
  /// Raw `XI_OVERRIDE_INPUT`, unparsed.
  pub env_raw:          Option<&'a str>,
  /// Flat `[name, flakeref, ...]` pairs from `--override-input` flags.
  pub cli_pairs:        &'a [String],
  /// Root of the flake being operated on. Used as the base directory for
  /// env values and to read `flake.lock` for filtering.
  pub flake_dir:        Option<&'a Path>,
  /// Process working directory, the base for CLI values.
  pub cwd:              &'a Path,
  pub kind:             nix_command::CommandKind,
  /// The `nix flake <sub>` subcommand, when the command is a flake one.
  pub flake_subcommand: Option<&'a str>,
}

/// The outcome of resolving the cascade.
#[derive(Debug, Default)]
pub struct Outcome {
  pub resolved:   Vec<Resolved>,
  /// Set when overrides were configured but deliberately not applied, so
  /// the caller can say so rather than leave the user guessing.
  pub suppressed: Option<String>,
}

/// Resolve the cascade for one command invocation, applying both gates.
///
/// # Errors
///
/// Propagates parse and normalization errors.
pub fn resolve_for_command(ctx: &ResolveContext<'_>) -> Result<Outcome, Error> {
  let configured = !ctx.global.is_empty()
    || !ctx.project.is_empty()
    || ctx.env_raw.is_some_and(|r| !r.trim().is_empty());

  // Gate 1: kinds that take no flake arguments must never see the flag.
  if !ctx.kind.accepts_flake_overrides() {
    return Ok(Outcome {
      resolved:   Vec::new(),
      suppressed: None,
    });
  }

  // Gate 2: `--override-input` implies `--no-write-lock-file`, so injecting
  // it into a lock-writing subcommand would silently stop it doing its job.
  if let Some(sub) = ctx.flake_subcommand
    && nix_command::is_lock_writing_flake_subcommand(sub)
  {
    return Ok(Outcome {
      resolved:   Vec::new(),
      suppressed: configured.then(|| {
        format!(
          "not applying configured override_input to 'nix flake {sub}': \
           --override-input implies --no-write-lock-file, which would stop it \
           writing flake.lock"
        )
      }),
    });
  }

  let mut entries: Vec<Entry> = Vec::new();

  for (name, value) in ctx.global {
    entries.push(Entry::new(name, value, OverrideSource::GlobalConfig));
  }

  entries.extend(ctx.project.iter().cloned());

  if let Some(raw) = ctx.env_raw {
    // Env values resolve against the flake root, so the value means the
    // same thing from any subdirectory. Without a known flake root there
    // is nothing honest to resolve against, so fall back to the CWD.
    let base = ctx.flake_dir.unwrap_or(ctx.cwd).to_path_buf();
    for (name, value) in parse_env_value(raw)? {
      entries.push(Entry::new(name, value, OverrideSource::Env {
        flake_root: base.clone(),
      }));
    }
  }

  entries.extend(entries_from_flat_pairs(ctx.cli_pairs, ctx.cwd));

  let known = ctx.flake_dir.and_then(flake_input_names);

  Ok(Outcome {
    resolved:   resolve(&entries, known.as_ref())?,
    suppressed: None,
  })
}

/// Report what happened, once per invocation.
///
/// A config file that silently redirects a build input is a real hazard, so
/// the report is a requirement and not polish.
pub fn report(outcome: &Outcome) {
  if let Some(reason) = &outcome.suppressed {
    tracing::warn!("{reason}");
  }

  for r in &outcome.resolved {
    match &r.path {
      Some(path) => {
        tracing::info!(
          "override-input {} -> {} (from {})",
          r.name,
          path.display(),
          r.source.label(),
        );
      },
      None => {
        tracing::info!(
          "override-input {} -> {} (from {})",
          r.name,
          r.flakeref,
          r.source.label(),
        );
      },
    }
  }
}

#[cfg(test)] mod tests;
