//! Wire schema — every subcommand emits `Envelope<T>` on stdout.
//!
//! Version discipline (per `specs/001-xi-agent/data-model.md`):
//! - `schema` string starts with `xi.agent/v` and an integer major.
//! - Additive fields inside `data` are allowed within a major.
//! - Removals or renames bump the major.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SCHEMA_V1: &str = "xi.agent/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
  pub schema:      String,
  pub command:     String,
  pub generated:   DateTime<Utc>,
  pub duration_ms: u64,
  pub data:        Option<T>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub errors:      Vec<Diagnostic>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub warnings:    Vec<Diagnostic>,
}

impl<T: Serialize> Envelope<T> {
  #[must_use]
  pub fn ok(command: &str, data: T, duration_ms: u64) -> Self {
    Self {
      schema: SCHEMA_V1.to_owned(),
      command: command.to_owned(),
      generated: Utc::now(),
      duration_ms,
      data: Some(data),
      errors: Vec::new(),
      warnings: Vec::new(),
    }
  }

  #[must_use]
  pub fn partial(
    command: &str,
    data: T,
    duration_ms: u64,
    errors: Vec<Diagnostic>,
  ) -> Self {
    Self {
      schema: SCHEMA_V1.to_owned(),
      command: command.to_owned(),
      generated: Utc::now(),
      duration_ms,
      data: Some(data),
      errors,
      warnings: Vec::new(),
    }
  }

  #[must_use]
  pub fn fail(command: &str, error: Diagnostic, duration_ms: u64) -> Self {
    Self {
      schema: SCHEMA_V1.to_owned(),
      command: command.to_owned(),
      generated: Utc::now(),
      duration_ms,
      data: None,
      errors: vec![error],
      warnings: Vec::new(),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
  pub code:                                                String,
  pub message:                                             String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub source:                                              Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub hint:                                                Option<String>,
}

impl Diagnostic {
  #[must_use]
  pub fn new(code: &str, message: impl Into<String>) -> Self {
    Self {
      code:    code.to_owned(),
      message: message.into(),
      source:  None,
      hint:    None,
    }
  }

  #[must_use]
  pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
    self.hint = Some(hint.into());
    self
  }

  #[must_use]
  pub fn with_source(mut self, source: impl Into<String>) -> Self {
    self.source = Some(source.into());
    self
  }
}

// ---------- context payload ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AgentContext {
  pub workspace:  WorkspaceInfo,
  pub flake:      Option<FlakeSummary>,
  pub devshell:   DevshellPayload,
  pub git:        GitState,
  pub validation: ValidationPlan,
  pub xi_config:  Option<XiConfigSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkspaceInfo {
  pub root:           PathBuf,
  pub current_system: String,
  pub xi_version:     String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FlakeSummary {
  pub path:      PathBuf,
  pub lock_hash: Option<String>,
  pub systems:   Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GitState {
  pub head:            Option<String>,
  pub branch:          Option<String>,
  pub dirty:           bool,
  pub untracked_count: u32,
  pub ahead_behind:    Option<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct XiConfigSummary {
  pub path:        PathBuf,
  pub sections:    Vec<String>,
  pub fmt_backend: Option<String>,
}

// ---------- outputs payload ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OutputsPayload {
  pub system:  String,
  pub outputs: Vec<OutputEntry>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub hidden:  Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OutputEntry {
  pub category:    String,
  pub kind:        String,
  pub name:        String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  pub installable: String,
}

// ---------- devshell payload ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DevshellPayload {
  pub state:               DevshellState,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub target:              Option<String>,
  #[serde(default)]
  pub package_count:       u32,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub daemon_state:        Option<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub active_cache_pushes: Vec<String>,
  pub entered_command:     String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DevshellState {
  NotRunning,
  Evaluating,
  Ready,
  Stale,
  Degraded,
}

// ---------- stage payload ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StagePayload {
  pub entries: Vec<StageEntry>,
  pub clean:   bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StageEntry {
  pub path:          PathBuf,
  pub git_status:    GitStatus,
  pub staged:        bool,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub referenced_by: Vec<FlakeReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitStatus {
  Untracked,
  Modified,
  Ignored,
  Deleted,
  Renamed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FlakeReference {
  pub from: PathBuf,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub attr: Option<String>,
}

// ---------- manifest payload ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ManifestPayload {
  pub root:  PathBuf,
  pub files: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ManifestEntry {
  pub path:        PathBuf,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub imported_by: Vec<PathBuf>,
  pub kind:        ManifestKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestKind {
  FlakeRoot,
  Module,
  Overlay,
  Lib,
  Package,
  Other,
}

// ---------- validation payload ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ValidationPlan {
  pub steps: Vec<ValidationStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ValidationStep {
  pub id:         String,
  pub command:    Vec<String>,
  pub purpose:    String,
  pub blocking:   bool,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum ValidationEvent {
  Started {
    id: String,
    at: DateTime<Utc>,
  },
  Progress {
    id:      String,
    message: String,
  },
  Finished {
    id:          String,
    status:      ValidationStatus,
    duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error:       Option<String>,
  },
  Complete {
    total_ms:            u64,
    all_blocking_passed: bool,
  },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationStatus {
  Passed,
  Failed,
  Skipped,
  Blocked,
}

// ---------- install payload ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct InstallPayload {
  pub scope:   String,
  pub target:  String,
  pub entries: Vec<InstallEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct InstallEntry {
  pub skill:  String,
  pub path:   PathBuf,
  pub action: InstallAction,
  pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum InstallAction {
  Wrote,
  UpToDate,
  Skipped { reason: String },
}

// ---------- emitters ----------

/// Serialize an envelope to stdout as a single JSON line.
///
/// # Errors
/// Returns an error only if the process cannot write to stdout at all.
pub fn emit<T: Serialize>(env: &Envelope<T>) -> std::io::Result<()> {
  use std::io::Write as _;
  let mut stdout = std::io::stdout().lock();
  serde_json::to_writer(&mut stdout, env).map_err(std::io::Error::other)?;
  stdout.write_all(b"\n")?;
  stdout.flush()
}

/// Return the elapsed milliseconds since `start`, saturating on overflow.
#[must_use]
pub fn elapsed_ms(start: std::time::Instant) -> u64 {
  u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}
