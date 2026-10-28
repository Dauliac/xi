//! The `xi sources` data model.
//!
//! This module is the single source of truth for the three on-disk concepts
//! every `xi-sources-*` capability reads from:
//!
//! - **sources dir**  -  the managed clone of the user's config repo. Owned by
//!   xi (or by the gitops daemon when enabled); the "what is deployed" view.
//! - **state dir**  -  a flat directory of small metadata files. Never contains
//!   source code.
//! - **working dir**  -  the user's *optional* editable clone. Its path is
//!   recorded in the state dir so a `switch` run from any CWD can find it.
//!
//! It also owns the [`Scope`] resolver, the [`SourcesId`] identity, the
//! [`RefLineage`] quadruple, and the `xi sources status` response envelope.
//!
//! The concrete filesystem path tables are **not** owned here  -  they belong
//! to the `xi-sources-config` capability. This module names the concepts and
//! provides a deterministic [`SourcesId::slug`] for config to build paths
//! from.

use std::{
  fmt,
  fs,
  io,
  path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub mod args;
pub mod config;
pub mod switch;

// ---------------------------------------------------------------------------
// Scope  -  system vs user
// ---------------------------------------------------------------------------

/// Which half of the machine a sources repo governs.
///
/// Exactly two variants, by spec. The mapping from CLI verb to scope is
/// fixed and lives in [`Scope::for_switch_verb`].
#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  Hash,
  PartialOrd,
  Ord,
  Serialize,
  Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
  /// Host-wide configuration (`xi nixos|darwin|system switch`).
  System,
  /// Per-user configuration (`xi home switch`).
  User,
}

impl Scope {
  /// The four concrete switch verbs, in the order they are printed when
  /// the user runs a verb-less `xi switch`.
  pub const SWITCH_VERBS: &'static [&'static str] =
    &["nixos", "darwin", "system", "home"];

  /// Resolve the scope a switch-style verb operates in.
  ///
  /// Returns `None` for any verb outside the fixed table, which the caller
  /// MUST surface as a parse error rather than defaulting.
  #[must_use]
  pub fn for_switch_verb(verb: &str) -> Option<Self> {
    match verb {
      "nixos" | "darwin" | "system" => Some(Self::System),
      "home" => Some(Self::User),
      _ => None,
    }
  }

  /// Stable lowercase token used in paths and on the wire.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::System => "system",
      Self::User => "user",
    }
  }
}

impl fmt::Display for Scope {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The error message for a verb-less `xi switch`.
///
/// Named here (rather than inline at the parse site) so the scope table and
/// the help text can never drift apart.
#[must_use]
pub fn verbless_switch_error() -> String {
  let verbs = Scope::SWITCH_VERBS
    .iter()
    .map(|verb| format!("  xi {verb} switch"))
    .collect::<Vec<_>>()
    .join("\n");
  format!("`xi switch` is ambiguous  -  name the scope explicitly:\n{verbs}")
}

// ---------------------------------------------------------------------------
// SourcesId  -  (scope, canonical url) identity
// ---------------------------------------------------------------------------

/// Canonicalise a git remote URL for identity purposes.
///
/// The documented normalisation is exactly three rules:
/// 1. scheme and host are lowercased,
/// 2. a trailing `.git` is stripped,
/// 3. userinfo (`user@`) is discarded.
///
/// The scp-like form git accepts (`[user@]host:path`) is rewritten to an
/// explicit `ssh://host/path` so it compares equal to the URL form. A bare
/// filesystem path has no host to normalise, so only rule 2 applies  -  path
/// case is significant on disk and is preserved.
#[must_use]
pub fn canonicalize_url(raw: &str) -> String {
  let trimmed = raw.trim().trim_end_matches('/');

  if let Some(idx) = trimmed.find("://") {
    let scheme = trimmed[..idx].to_ascii_lowercase();
    let rest = &trimmed[idx + 3..];
    let (authority, path) = rest
      .find('/')
      .map_or((rest, ""), |slash| (&rest[..slash], &rest[slash + 1..]));
    return assemble_url(&scheme, authority, path);
  }

  if let Some(colon) = scp_separator(trimmed) {
    return assemble_url("ssh", &trimmed[..colon], &trimmed[colon + 1..]);
  }

  strip_git_suffix(trimmed).to_owned()
}

/// Rebuild a canonical `scheme://host/path`, dropping userinfo.
fn assemble_url(scheme: &str, authority: &str, path: &str) -> String {
  let host = authority
    .rsplit_once('@')
    .map_or(authority, |(_userinfo, host)| host)
    .to_ascii_lowercase();
  let path = strip_git_suffix(path.trim_start_matches('/'));

  if path.is_empty() {
    format!("{scheme}://{host}")
  } else {
    format!("{scheme}://{host}/{path}")
  }
}

/// Byte offset of the scp-style `host:path` separator, if this is that form.
///
/// A colon that appears *after* a slash belongs to the path, not to the
/// authority, so such a string is not scp-like.
pub(crate) fn scp_separator(s: &str) -> Option<usize> {
  let colon = s.find(':')?;
  match s.find('/') {
    Some(slash) if slash < colon => None,
    _ => Some(colon),
  }
}

fn strip_git_suffix(s: &str) -> &str {
  s.strip_suffix(".git").unwrap_or(s)
}

/// The identity of a logical sources repo on this machine.
///
/// Keyed only on `(scope, canonical_url)` so it is stable across reboots,
/// hostname changes, and user renames. The same repo cloned into two paths
/// is one logical source; the same URL at two scopes is two.
#[derive(
  Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct SourcesId {
  /// Which half of the machine this source governs.
  pub scope:         Scope,
  /// The remote URL after [`canonicalize_url`].
  pub canonical_url: String,
}

impl SourcesId {
  /// Build an id, canonicalising `remote_url` on the way in.
  #[must_use]
  pub fn new(scope: Scope, remote_url: &str) -> Self {
    Self {
      scope,
      canonical_url: canonicalize_url(remote_url),
    }
  }

  /// A deterministic, filesystem-safe token identifying this source.
  ///
  /// `xi-sources-config` owns the full path tables; this is the stable leaf
  /// component it builds them from. Every byte outside `[A-Za-z0-9._-]` is
  /// folded to `-` so the token is portable across filesystems.
  #[must_use]
  pub fn slug(&self) -> String {
    let sanitized: String = self
      .canonical_url
      .chars()
      .map(|c| {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
          c
        } else {
          '-'
        }
      })
      .collect();
    format!("{}-{}", self.scope.as_str(), sanitized.trim_matches('-'))
  }
}

impl fmt::Display for SourcesId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}:{}", self.scope, self.canonical_url)
  }
}

// ---------------------------------------------------------------------------
// RefLineage  -  the four observable refs
// ---------------------------------------------------------------------------

/// A git object id, as lowercase hex.
pub type Sha = String;

/// A system/home generation identifier.
pub type GenerationId = String;

/// The `remote-head -> working-head -> sources-head -> active-generation`
/// lineage.
///
/// All four are optional: any can legitimately be unknown on a fresh
/// install. Conflict rules between them are owned by `xi-sources-conflict`;
/// this type only guarantees they are *observable*.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefLineage {
  /// Last sha the gitops daemon successfully fetched from the remote.
  pub remote_head:       Option<Sha>,
  /// HEAD of the working dir, if one is configured.
  pub working_head:      Option<Sha>,
  /// HEAD of the sources dir.
  pub sources_head:      Option<Sha>,
  /// Generation produced by the most recent successful switch.
  pub active_generation: Option<GenerationId>,
}

// ---------------------------------------------------------------------------
// State dir  -  a fixed, flat set of metadata files
// ---------------------------------------------------------------------------

/// Every filename the state dir is allowed to contain.
///
/// Anything else found there is reported as a `Degraded` reason rather than
/// silently ignored.
pub const STATE_FILE_NAMES: &[&str] = &[
  "remote-url",
  "remote-head",
  "working-head",
  "sources-head",
  "working-dir",
  "active-generation",
  "lock",
];

/// Hard cap on any single state file. The state dir holds metadata only;
/// anything approaching this size means source code leaked in.
pub const MAX_STATE_FILE_BYTES: usize = 64 * 1024;

/// Read one state file, treating "absent" as "unknown" rather than an error.
///
/// Returns `Ok(None)` when the file does not exist. Blank files are also
/// `None`: a zero-length `remote-head` carries no more information than a
/// missing one.
///
/// # Errors
///
/// Propagates any I/O error other than `NotFound`.
pub fn read_state_file(
  state_dir: &Path,
  name: &str,
) -> io::Result<Option<String>> {
  match fs::read_to_string(state_dir.join(name)) {
    Ok(raw) => {
      let trimmed = raw.trim();
      Ok(if trimmed.is_empty() {
        None
      } else {
        Some(trimmed.to_owned())
      })
    },
    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(err) => Err(err),
  }
}

/// Write one state file, enforcing the spec's two invariants.
///
/// The filename MUST be in [`STATE_FILE_NAMES`] and the payload MUST be at
/// most [`MAX_STATE_FILE_BYTES`]. The directory is created `0700` and the
/// file written `0600` on unix so state is readable only by its owner.
///
/// # Errors
///
/// Returns `InvalidInput` for an unknown filename, `InvalidData` for an
/// oversized payload, and propagates underlying I/O errors.
pub fn write_state_file(
  state_dir: &Path,
  name: &str,
  contents: &str,
) -> io::Result<()> {
  if !STATE_FILE_NAMES.contains(&name) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("`{name}` is not a sanctioned state-dir filename"),
    ));
  }
  if contents.len() > MAX_STATE_FILE_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      format!(
        "state file `{name}` is {} bytes, over the {MAX_STATE_FILE_BYTES} \
         byte cap",
        contents.len()
      ),
    ));
  }

  fs::create_dir_all(state_dir)?;
  harden_dir(state_dir)?;

  let path = state_dir.join(name);
  fs::write(&path, contents)?;
  harden_file(&path)
}

#[cfg(unix)]
fn harden_dir(dir: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt as _;
  fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(unix)]
fn harden_file(path: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt as _;
  fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn harden_dir(_dir: &Path) -> io::Result<()> {
  Ok(())
}

#[cfg(not(unix))]
fn harden_file(_path: &Path) -> io::Result<()> {
  Ok(())
}

/// Names present in the state dir that are outside [`STATE_FILE_NAMES`].
///
/// A missing state dir yields an empty list, not an error: "not yet
/// initialised" is a normal state, distinct from "contains junk".
///
/// # Errors
///
/// Propagates any I/O error other than `NotFound` on the directory itself.
pub fn unknown_state_files(state_dir: &Path) -> io::Result<Vec<String>> {
  let entries = match fs::read_dir(state_dir) {
    Ok(entries) => entries,
    Err(err) if err.kind() == io::ErrorKind::NotFound => {
      return Ok(Vec::new());
    },
    Err(err) => return Err(err),
  };

  let mut unknown: Vec<String> = entries
    .filter_map(|entry| {
      let name = entry.ok()?.file_name().to_string_lossy().into_owned();
      (!STATE_FILE_NAMES.contains(&name.as_str())).then_some(name)
    })
    .collect();
  unknown.sort();
  Ok(unknown)
}

// ---------------------------------------------------------------------------
// Gitops health  -  supplied by xi-sources-gitops, consumed here
// ---------------------------------------------------------------------------

/// Health of the gitops fetch loop for this source.
///
/// The model does not run the loop; `xi-sources-gitops` owns it and hands
/// this in. It exists here because the status mapping depends on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "gitops", rename_all = "snake_case")]
pub enum GitopsHealth {
  /// No gitops daemon is tracking this source.
  #[default]
  Disabled,
  /// The last fetch tick succeeded.
  Ok,
  /// The last fetch tick failed.
  Failed {
    /// Human-readable cause, surfaced verbatim in the `Degraded` reason.
    reason: String,
  },
}

// ---------------------------------------------------------------------------
// SourcesContext  -  the loaded model
// ---------------------------------------------------------------------------

/// The resolved on-disk model for one `(scope, remote)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcesContext {
  /// Stable identity of this source.
  pub sources_id:          SourcesId,
  /// The managed clone xi owns.
  pub sources_dir:         PathBuf,
  /// The flat metadata directory.
  pub state_dir:           PathBuf,
  /// The user's editable clone, if they recorded one.
  pub working_dir:         Option<PathBuf>,
  /// The four observable refs.
  pub refs:                RefLineage,
  /// Files in the state dir outside the sanctioned set.
  pub unknown_state_files: Vec<String>,
}

impl SourcesContext {
  /// Load the model from disk.
  ///
  /// Missing state files mean "unknown", never an error. A missing state
  /// dir, or a missing sources dir, likewise loads successfully  -  the
  /// distinction surfaces through [`SourcesContext::status`].
  ///
  /// # Errors
  ///
  /// Propagates I/O errors other than `NotFound`.
  pub fn load(
    sources_id: SourcesId,
    sources_dir: impl Into<PathBuf>,
    state_dir: impl Into<PathBuf>,
  ) -> io::Result<Self> {
    let state_dir = state_dir.into();

    let refs = RefLineage {
      remote_head:       read_state_file(&state_dir, "remote-head")?,
      working_head:      read_state_file(&state_dir, "working-head")?,
      sources_head:      read_state_file(&state_dir, "sources-head")?,
      active_generation: read_state_file(&state_dir, "active-generation")?,
    };
    let working_dir =
      read_state_file(&state_dir, "working-dir")?.map(PathBuf::from);
    let unknown_state_files = unknown_state_files(&state_dir)?;

    Ok(Self {
      sources_id,
      sources_dir: sources_dir.into(),
      state_dir,
      working_dir,
      refs,
      unknown_state_files,
    })
  }

  /// Compute the envelope status for this context.
  ///
  /// Precedence is deliberate: `Missing` outranks everything (there is
  /// nothing to describe), then the three `Degraded` causes, then
  /// `Pending`, then `Ready`. `Degraded` is checked before `Pending`
  /// because a stale pointer or a failed tick is actionable now, whereas
  /// "never switched" resolves itself on the next switch.
  #[must_use]
  pub fn status(&self, gitops: &GitopsHealth) -> SourcesStatus {
    if !self.sources_dir.is_dir() {
      return SourcesStatus::Missing {
        configure_with: ConfigureHint::all(),
      };
    }

    if let Some(unknown) = self.unknown_state_files.first() {
      return self.degraded(format!(
        "unrecognised file in state dir: `{unknown}` (expected only {})",
        STATE_FILE_NAMES.join(", ")
      ));
    }

    if let Some(working_dir) = self.working_dir.as_ref()
      && !working_dir.exists()
    {
      return self.degraded(format!(
        "recorded working dir no longer exists: {}",
        working_dir.display()
      ));
    }

    if let GitopsHealth::Failed { reason } = gitops {
      return self.degraded(format!("last gitops tick failed: {reason}"));
    }

    if self.refs.active_generation.is_none() {
      return SourcesStatus::Pending {
        snapshot: self.snapshot(),
      };
    }

    SourcesStatus::Ready {
      snapshot: self.snapshot(),
    }
  }

  /// The serialisable body carried by every non-`Missing` status.
  #[must_use]
  pub fn snapshot(&self) -> SourcesSnapshot {
    SourcesSnapshot {
      sources_id:  self.sources_id.clone(),
      sources_dir: self.sources_dir.clone(),
      state_dir:   self.state_dir.clone(),
      working_dir: self.working_dir.clone(),
      refs:        self.refs.clone(),
    }
  }

  fn degraded(&self, reason: String) -> SourcesStatus {
    SourcesStatus::Degraded {
      reason,
      snapshot: self.snapshot(),
    }
  }
}

// ---------------------------------------------------------------------------
// Response envelope
// ---------------------------------------------------------------------------

/// Schema version of the `xi sources status` envelope.
pub const SOURCES_SCHEMA_VERSION: u32 = 1;

/// Where a configuration value can come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HintKind {
  /// An environment variable.
  Env,
  /// A key in `.xi.toml`.
  Config,
  /// A CLI invocation.
  Command,
}

/// One concrete way the user could configure sources.
///
/// `Missing` carries the full set so the error names the fix rather than
/// just the absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigureHint {
  /// Which configuration surface this hint refers to.
  pub kind:   HintKind,
  /// The variable, key, or command.
  pub name:   String,
  /// What setting it accomplishes.
  pub detail: String,
}

impl ConfigureHint {
  /// Every sanctioned way to configure a sources repo.
  #[must_use]
  pub fn all() -> Vec<Self> {
    vec![
      Self {
        kind:   HintKind::Command,
        name:   "xi <scope> switch <remote-url>".to_owned(),
        detail: "adopt the config repo for this scope and switch to it"
          .to_owned(),
      },
      Self {
        kind:   HintKind::Env,
        name:   "XI_SOURCES_REMOTE".to_owned(),
        detail: "remote URL of the config repo".to_owned(),
      },
      Self {
        kind:   HintKind::Config,
        name:   "[sources] remote".to_owned(),
        detail: "remote URL of the config repo, in .xi.toml".to_owned(),
      },
    ]
  }
}

/// The SC-M-002 body: everything a caller needs to reason about the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcesSnapshot {
  /// Stable identity of this source.
  pub sources_id:  SourcesId,
  /// The managed clone xi owns.
  pub sources_dir: PathBuf,
  /// The flat metadata directory.
  pub state_dir:   PathBuf,
  /// The user's editable clone, if recorded.
  pub working_dir: Option<PathBuf>,
  /// The four observable refs, inlined into the body.
  #[serde(flatten)]
  pub refs:        RefLineage,
}

/// The four-way status of a sources repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SourcesStatus {
  /// Sources dir exists, a generation is active, gitops is healthy.
  Ready {
    /// The resolved model.
    #[serde(flatten)]
    snapshot: SourcesSnapshot,
  },
  /// Sources dir exists but nothing has been switched to yet.
  Pending {
    /// The resolved model.
    #[serde(flatten)]
    snapshot: SourcesSnapshot,
  },
  /// No sources dir for this scope.
  Missing {
    /// Every way the user could configure one.
    configure_with: Vec<ConfigureHint>,
  },
  /// Sources dir exists but the model is in a state needing attention.
  Degraded {
    /// What is wrong, in user-facing terms.
    reason:   String,
    /// The resolved model, so callers can still act on known refs.
    #[serde(flatten)]
    snapshot: SourcesSnapshot,
  },
}

/// The versioned envelope emitted by `xi sources status` on stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcesStatusResponse {
  /// Envelope schema version; see [`SOURCES_SCHEMA_VERSION`].
  pub schema_version: u32,
  /// The status and its body.
  #[serde(flatten)]
  pub status:         SourcesStatus,
  /// Which precedence rung answered for each config key.
  ///
  /// Only present under `xi sources status --explain`, so the default
  /// envelope keeps the exact shape `xi-sources-model` pinned.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub provenance:     Option<config::Provenances>,
}

impl SourcesStatusResponse {
  /// Wrap a status in the current envelope version.
  #[must_use]
  pub const fn new(status: SourcesStatus) -> Self {
    Self {
      schema_version: SOURCES_SCHEMA_VERSION,
      status,
      provenance: None,
    }
  }

  /// Attach the provenance map, for `--explain`.
  #[must_use]
  pub fn with_provenance(mut self, provenance: config::Provenances) -> Self {
    self.provenance = Some(provenance);
    self
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use tempfile::TempDir;

  use super::*;

  // -- 6.1 scope resolver, one case per verb ------------------------------

  #[test]
  fn nixos_switch_is_system_scope() {
    assert_eq!(Scope::for_switch_verb("nixos"), Some(Scope::System));
  }

  #[test]
  fn darwin_switch_is_system_scope() {
    assert_eq!(Scope::for_switch_verb("darwin"), Some(Scope::System));
  }

  #[test]
  fn system_switch_is_system_scope() {
    assert_eq!(Scope::for_switch_verb("system"), Some(Scope::System));
  }

  #[test]
  fn home_switch_is_user_scope() {
    assert_eq!(Scope::for_switch_verb("home"), Some(Scope::User));
  }

  #[test]
  fn verbless_switch_does_not_resolve() {
    assert_eq!(Scope::for_switch_verb(""), None);
    assert_eq!(Scope::for_switch_verb("switch"), None);
  }

  #[test]
  fn verbless_switch_error_names_all_four_verbs() {
    let msg = verbless_switch_error();
    for verb in Scope::SWITCH_VERBS {
      assert!(msg.contains(verb), "`{verb}` missing from: {msg}");
    }
  }

  // -- SourcesId identity -------------------------------------------------

  #[test]
  fn trailing_dot_git_is_irrelevant() {
    let with = SourcesId::new(Scope::User, "https://git.example/repo.git");
    let without = SourcesId::new(Scope::User, "https://git.example/repo");
    assert_eq!(with, without);
  }

  #[test]
  fn scheme_and_host_are_lowercased_userinfo_dropped() {
    let a = SourcesId::new(Scope::User, "HTTPS://Git.Example/Repo");
    let b = SourcesId::new(Scope::User, "https://alice@git.example/Repo");
    assert_eq!(a, b);
    assert_eq!(a.canonical_url, "https://git.example/Repo");
  }

  #[test]
  fn path_case_is_preserved() {
    let id = SourcesId::new(Scope::User, "https://git.example/CamelCase");
    assert_eq!(id.canonical_url, "https://git.example/CamelCase");
  }

  #[test]
  fn scp_form_matches_ssh_url_form() {
    let scp = SourcesId::new(Scope::System, "git@github.com:Dauliac/xi.git");
    let url = SourcesId::new(Scope::System, "ssh://github.com/Dauliac/xi");
    assert_eq!(scp, url);
    assert_eq!(scp.canonical_url, "ssh://github.com/Dauliac/xi");
  }

  #[test]
  fn url_with_port_is_not_mistaken_for_scp_form() {
    let id = SourcesId::new(Scope::User, "https://git.example:8443/repo");
    assert_eq!(id.canonical_url, "https://git.example:8443/repo");
  }

  #[test]
  fn scope_separates_identical_urls() {
    let user = SourcesId::new(Scope::User, "https://git.example/dotfiles");
    let system = SourcesId::new(Scope::System, "https://git.example/dotfiles");
    assert_ne!(user, system);
    assert_ne!(user.slug(), system.slug());
  }

  #[test]
  fn slug_is_filesystem_safe_and_deterministic() {
    let id = SourcesId::new(Scope::User, "https://git.example/a/b.git");
    let slug = id.slug();
    assert_eq!(slug, id.slug(), "slug must be deterministic");
    assert!(
      slug
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
      "slug not filesystem-safe: {slug}"
    );
    assert!(slug.starts_with("user-"), "slug missing scope: {slug}");
  }

  // -- state dir helpers --------------------------------------------------

  #[test]
  fn missing_state_file_is_unknown_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let got = read_state_file(tmp.path(), "remote-head").unwrap();
    assert_eq!(got, None);
  }

  #[test]
  fn blank_state_file_is_unknown() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("remote-head"), "  \n").unwrap();
    assert_eq!(read_state_file(tmp.path(), "remote-head").unwrap(), None);
  }

  #[test]
  fn write_state_file_rejects_unsanctioned_name() {
    let tmp = TempDir::new().unwrap();
    let err = write_state_file(tmp.path(), "source.rs", "fn main() {}")
      .expect_err("unknown filename must be rejected");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
  }

  #[test]
  fn write_state_file_rejects_oversized_payload() {
    let tmp = TempDir::new().unwrap();
    let big = "a".repeat(MAX_STATE_FILE_BYTES + 1);
    let err = write_state_file(tmp.path(), "remote-head", &big)
      .expect_err("oversized payload must be rejected");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  fn write_state_file_round_trips() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("state");
    write_state_file(&dir, "sources-head", "deadbeef").unwrap();
    assert_eq!(
      read_state_file(&dir, "sources-head").unwrap(),
      Some("deadbeef".to_owned())
    );
  }

  #[cfg(unix)]
  #[test]
  fn written_state_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("state");
    write_state_file(&dir, "remote-head", "cafe").unwrap();

    let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    let file_mode = fs::metadata(dir.join("remote-head"))
      .unwrap()
      .permissions()
      .mode()
      & 0o777;
    assert_eq!(dir_mode, 0o700);
    assert_eq!(file_mode, 0o600);
  }

  #[test]
  fn missing_state_dir_has_no_unknown_files() {
    let tmp = TempDir::new().unwrap();
    let absent = tmp.path().join("nope");
    assert!(unknown_state_files(&absent).unwrap().is_empty());
  }

  // -- 6.2 SourcesContext::load across the four statuses -------------------

  /// Build a `(sources_dir, state_dir)` pair under a tempdir. The sources
  /// dir is created only when `with_sources` is set, so the `Missing` case
  /// is reachable.
  fn fixture(tmp: &TempDir, with_sources: bool) -> (PathBuf, PathBuf) {
    let sources_dir = tmp.path().join("sources");
    let state_dir = tmp.path().join("state");
    if with_sources {
      fs::create_dir_all(&sources_dir).unwrap();
    }
    fs::create_dir_all(&state_dir).unwrap();
    (sources_dir, state_dir)
  }

  fn load(sources_dir: &Path, state_dir: &Path) -> SourcesContext {
    let id = SourcesId::new(Scope::System, "https://git.example/cfg");
    SourcesContext::load(id, sources_dir, state_dir).unwrap()
  }

  #[test]
  fn no_sources_dir_is_missing_and_names_the_fix() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, false);
    let ctx = load(&sources_dir, &state_dir);

    match ctx.status(&GitopsHealth::Disabled) {
      SourcesStatus::Missing { configure_with } => {
        assert!(!configure_with.is_empty());
        let names: Vec<&str> =
          configure_with.iter().map(|h| h.name.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("XI_SOURCES_REMOTE")));
        assert!(names.iter().any(|n| n.contains("[sources] remote")));
        assert!(names.iter().any(|n| n.contains("switch <remote-url>")));
      },
      other => panic!("expected Missing, got {other:?}"),
    }
  }

  #[test]
  fn sources_dir_without_generation_is_pending() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    write_state_file(&state_dir, "sources-head", "abc123").unwrap();

    let ctx = load(&sources_dir, &state_dir);
    assert!(matches!(
      ctx.status(&GitopsHealth::Disabled),
      SourcesStatus::Pending { .. }
    ));
  }

  #[test]
  fn switched_machine_is_ready() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    write_state_file(&state_dir, "sources-head", "abc123").unwrap();
    write_state_file(&state_dir, "active-generation", "42").unwrap();

    let ctx = load(&sources_dir, &state_dir);
    assert!(matches!(
      ctx.status(&GitopsHealth::Disabled),
      SourcesStatus::Ready { .. }
    ));
    assert!(matches!(
      ctx.status(&GitopsHealth::Ok),
      SourcesStatus::Ready { .. }
    ));
  }

  #[test]
  fn all_four_refs_are_observable() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    write_state_file(&state_dir, "sources-head", "abc123").unwrap();
    write_state_file(&state_dir, "active-generation", "42").unwrap();

    let ctx = load(&sources_dir, &state_dir);
    assert_eq!(ctx.refs.remote_head, None);
    assert_eq!(ctx.refs.working_head, None);
    assert_eq!(ctx.refs.sources_head, Some("abc123".to_owned()));
    assert_eq!(ctx.refs.active_generation, Some("42".to_owned()));
  }

  #[test]
  fn stale_working_dir_pointer_is_degraded_and_names_the_path() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    let stale = tmp.path().join("gone");
    write_state_file(&state_dir, "active-generation", "42").unwrap();
    write_state_file(&state_dir, "working-dir", &stale.display().to_string())
      .unwrap();

    let ctx = load(&sources_dir, &state_dir);
    match ctx.status(&GitopsHealth::Disabled) {
      SourcesStatus::Degraded { reason, .. } => {
        assert!(
          reason.contains(&stale.display().to_string()),
          "reason must name the stale path: {reason}"
        );
      },
      other => panic!("expected Degraded, got {other:?}"),
    }
  }

  #[test]
  fn live_working_dir_pointer_is_not_degraded() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    let working = tmp.path().join("work");
    fs::create_dir_all(&working).unwrap();
    write_state_file(&state_dir, "active-generation", "42").unwrap();
    write_state_file(&state_dir, "working-dir", &working.display().to_string())
      .unwrap();

    let ctx = load(&sources_dir, &state_dir);
    assert_eq!(ctx.working_dir, Some(working));
    assert!(matches!(
      ctx.status(&GitopsHealth::Disabled),
      SourcesStatus::Ready { .. }
    ));
  }

  #[test]
  fn failed_gitops_tick_is_degraded() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    write_state_file(&state_dir, "active-generation", "42").unwrap();

    let ctx = load(&sources_dir, &state_dir);
    let health = GitopsHealth::Failed {
      reason: "host unreachable".to_owned(),
    };
    match ctx.status(&health) {
      SourcesStatus::Degraded { reason, .. } => {
        assert!(reason.contains("host unreachable"), "got: {reason}");
      },
      other => panic!("expected Degraded, got {other:?}"),
    }
  }

  #[test]
  fn unknown_file_in_state_dir_is_reported_not_ignored() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    write_state_file(&state_dir, "active-generation", "42").unwrap();
    fs::write(state_dir.join("random-file.txt"), "junk").unwrap();

    let ctx = load(&sources_dir, &state_dir);
    assert_eq!(ctx.unknown_state_files, vec!["random-file.txt".to_owned()]);
    match ctx.status(&GitopsHealth::Disabled) {
      SourcesStatus::Degraded { reason, .. } => {
        assert!(reason.contains("random-file.txt"), "got: {reason}");
      },
      other => panic!("expected Degraded, got {other:?}"),
    }
  }

  #[test]
  fn missing_state_dir_loads_with_all_refs_unknown() {
    let tmp = TempDir::new().unwrap();
    let sources_dir = tmp.path().join("sources");
    fs::create_dir_all(&sources_dir).unwrap();
    let state_dir = tmp.path().join("never-created");

    let ctx = load(&sources_dir, &state_dir);
    assert_eq!(ctx.refs, RefLineage::default());
    assert_eq!(ctx.working_dir, None);
    assert!(ctx.unknown_state_files.is_empty());
  }

  // -- envelope -----------------------------------------------------------

  #[test]
  fn envelope_carries_schema_version_and_flat_status() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, false);
    let ctx = load(&sources_dir, &state_dir);
    let response =
      SourcesStatusResponse::new(ctx.status(&GitopsHealth::Disabled));

    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["schema_version"], SOURCES_SCHEMA_VERSION);
    assert_eq!(json["status"], "missing");
  }

  #[test]
  fn ready_envelope_inlines_the_four_refs() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    write_state_file(&state_dir, "sources-head", "abc123").unwrap();
    write_state_file(&state_dir, "active-generation", "42").unwrap();

    let ctx = load(&sources_dir, &state_dir);
    let response =
      SourcesStatusResponse::new(ctx.status(&GitopsHealth::Disabled));
    let json = serde_json::to_value(&response).unwrap();

    assert_eq!(json["status"], "ready");
    assert_eq!(json["sources_head"], "abc123");
    assert_eq!(json["active_generation"], "42");
    assert!(json["remote_head"].is_null());
    assert!(json["working_head"].is_null());
    assert!(json["sources_dir"].is_string());
    assert!(json["state_dir"].is_string());
  }

  #[test]
  fn envelope_round_trips_through_json() {
    let tmp = TempDir::new().unwrap();
    let (sources_dir, state_dir) = fixture(&tmp, true);
    write_state_file(&state_dir, "active-generation", "42").unwrap();

    let ctx = load(&sources_dir, &state_dir);
    let response =
      SourcesStatusResponse::new(ctx.status(&GitopsHealth::Disabled));
    let encoded = serde_json::to_string(&response).unwrap();
    let decoded: SourcesStatusResponse =
      serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, response);
  }
}
