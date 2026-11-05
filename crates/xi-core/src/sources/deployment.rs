//! `xi <scope> status` - a human-readable deployment overview.
//!
//! Distinct from `xi <scope> sources status`, which is the machine
//! envelope the ladder and the daemons read back. This is the "what is
//! actually running on this machine, and what built it" report: xi's
//! own version and git revision, how this scope is set up (push/manual
//! vs gitops pull, local-path vs remote sources, dirty or not), the
//! sources model's git hashes, the active generation's closure size,
//! and which system-level deployment mechanism (NixOS, system-manager,
//! or nix-darwin) is in effect, so a user-scope report can say
//! something useful about who else might be touching home-manager on
//! this host.
//!
//! Named `status`, not `show`: `xi show` already exists at the
//! top level for "show the outputs of a flake", an unrelated meaning,
//! and `status` matches the sibling `xi <scope> sources status`.

use std::{path::Path, process::Command};

use color_eyre::Result;
use serde::Serialize;

use super::{
  GitopsHealth,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatus,
  args::resolve_layers,
  config::{ScopePaths, SourcesConfig},
  log::SYSTEMD_UNIT,
  scp_separator,
};
use crate::{
  flake_output::FlakeOutput,
  installable::{Installable, resolve_toplevel},
  style::{self, Icon},
};

/// Which mechanism, if any, owns this host's system-wide configuration.
///
/// [`Scope::System`] covers NixOS, nix-darwin, and system-manager alike
/// (see [`Scope`]'s own doc comment); this tells the three apart for
/// reporting, not for anything the sources model itself needs to branch
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SystemDeploymentKind {
  /// `/etc/NIXOS` exists: a NixOS host.
  NixOs,
  /// A system-manager profile exists: a non-NixOS Linux host managed by
  /// `xi system switch`.
  SystemManager,
  /// Running on Darwin, under nix-darwin.
  Darwin,
  /// None of the above markers were found.
  None,
}

/// Why [`SystemDeploymentKind::detect_with_sources`] refused to resolve a
/// mechanism from the sources: either namespace declared `hostname`
/// (ambiguous) or neither did (nothing to fall back to). Both are a real
/// ambiguity per the spec, not something to guess past.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MechanismAmbiguity {
  /// Both `nixosConfigurations.<hostname>` and `systemConfigs.<hostname>`
  /// are declared.
  #[error(
    "host '{hostname}' is declared under both nixosConfigurations and \
     systemConfigs; refusing to guess which system mechanism owns it"
  )]
  Both {
    /// The hostname that was looked up.
    hostname: String,
  },
  /// Host state was silent and neither namespace declares `hostname`.
  #[error(
    "host state is silent and neither nixosConfigurations.{hostname} nor \
     systemConfigs.{hostname} is declared; cannot determine the system \
     mechanism"
  )]
  Neither {
    /// The hostname that was looked up.
    hostname: String,
  },
}

impl SystemDeploymentKind {
  /// Probe well-known marker paths for the active mechanism.
  ///
  /// Best-effort: a marker that cannot be read is treated the same as
  /// one that is absent, this is a status report, not a precondition
  /// check that should fail the command.
  #[must_use]
  pub fn detect() -> Self {
    Self::detect_from_host_state()
  }

  /// The host-state rung of [`Self::detect`] and [`Self::detect_with_sources`],
  /// unchanged from `detect`'s original body: a marker that cannot be
  /// read is treated the same as one that is absent.
  #[must_use]
  fn detect_from_host_state() -> Self {
    if cfg!(target_os = "macos") {
      return Self::Darwin;
    }
    if Path::new("/etc/NIXOS").exists() {
      return Self::NixOs;
    }
    if Path::new("/nix/var/nix/profiles/system-manager-profiles/system-manager")
      .exists()
    {
      return Self::SystemManager;
    }
    Self::None
  }

  /// Full OS-scope mechanism detection: host state first; when host state
  /// is silent (nothing installed yet, or genuinely ambiguous), fall
  /// through to whichever namespace the adopted or about-to-be-adopted
  /// flake declares `hostname` under.
  ///
  /// `attribute_exists` answers "does this flake attribute reference
  /// exist", backed by a real Nix evaluation in production and a fixture
  /// set in tests; the reference itself is built via [`resolve_toplevel`]
  /// rather than a second hand-rolled `<namespace>.<hostname>` formatter.
  ///
  /// # Errors
  ///
  /// Returns [`MechanismAmbiguity`] when both or neither namespace
  /// declares `hostname`  -  host state is silent and the sources do not
  /// give a single answer either.
  pub fn detect_with_sources(
    flake_ref: &str,
    hostname: &str,
    attribute_exists: &dyn Fn(&str) -> bool,
  ) -> Result<Self, MechanismAmbiguity> {
    Self::resolve_with_sources(
      Self::detect_from_host_state(),
      flake_ref,
      hostname,
      attribute_exists,
    )
  }

  /// [`Self::detect_with_sources`], with the host-state result taken as
  /// a parameter rather than re-probed, so tests can drive the
  /// host-state-present and host-state-silent branches deterministically
  /// without faking `/etc/NIXOS` or a system-manager profile on disk.
  fn resolve_with_sources(
    host_state: Self,
    flake_ref: &str,
    hostname: &str,
    attribute_exists: &dyn Fn(&str) -> bool,
  ) -> Result<Self, MechanismAmbiguity> {
    if host_state != Self::None {
      return Ok(host_state);
    }

    let nixos = attribute_exists(&mechanism_attr(
      flake_ref,
      hostname,
      FlakeOutput::NixosConfigurations,
    ));
    let system_manager = attribute_exists(&mechanism_attr(
      flake_ref,
      hostname,
      FlakeOutput::SystemConfigs,
    ));

    match (nixos, system_manager) {
      (true, false) => Ok(Self::NixOs),
      (false, true) => Ok(Self::SystemManager),
      (true, true) => Err(MechanismAmbiguity::Both {
        hostname: hostname.to_owned(),
      }),
      (false, false) => Err(MechanismAmbiguity::Neither {
        hostname: hostname.to_owned(),
      }),
    }
  }

  /// Short, human-facing name.
  #[must_use]
  pub const fn label(self) -> &'static str {
    match self {
      Self::NixOs => "NixOS",
      Self::SystemManager => "system-manager",
      Self::Darwin => "nix-darwin",
      Self::None => "none detected",
    }
  }
}

/// Build the `<flake_ref>#<namespace>.<hostname>` reference
/// [`resolve_toplevel`] would resolve for `output`, so the sources-fallback
/// rung asks about the exact attribute a real activation would build,
/// rather than a second implementation of that hostname match.
///
/// `resolve_toplevel` cannot fail for an empty starting attribute and an
/// empty build path (the only way it is ever called here); the fallback
/// formatter exists only so this stays total without an `expect`.
fn mechanism_attr(flake_ref: &str, hostname: &str, output: FlakeOutput) -> String {
  let installable = resolve_toplevel(
    hostname,
    Installable::Flake {
      reference: flake_ref.to_owned(),
      attribute: Vec::new(),
    },
    output,
    &[],
  )
  .unwrap_or_else(|_| Installable::Flake {
    reference: flake_ref.to_owned(),
    attribute: vec![output.as_str().to_owned(), hostname.to_owned()],
  });
  installable.to_args().into_iter().next().unwrap_or_default()
}

/// Whether a `home-manager-<user>.service` unit exists, the telltale
/// sign that this user's home-manager is embedded in a NixOS or
/// system-manager activation rather than run standalone.
#[must_use]
pub fn embedded_home_manager_unit(username: &str) -> bool {
  embedded_home_manager_unit_under(Path::new("/"), username)
}

/// [`embedded_home_manager_unit`], rooted under `root` instead of `/`, so
/// tests can drive both branches against a fixture filesystem rather
/// than the real `/etc/systemd/system`.
fn embedded_home_manager_unit_under(root: &Path, username: &str) -> bool {
  root
    .join(format!(
      "etc/systemd/system/home-manager-{username}.service"
    ))
    .exists()
}

/// Which `homeConfigurations` output name (if any) backs a standalone
/// home-manager activation for this user, checking the host-qualified
/// `<user>@<host>` form before the bare `<user>` form.
///
/// Takes the flake's already-enumerated `homeConfigurations` attribute
/// names rather than evaluating the flake itself, so callers that already
/// hold that list (and tests) don't re-run `nix eval` to ask this
/// question.
#[must_use]
pub fn discover_standalone_home_output<'a>(
  username: &str,
  hostname: &str,
  available: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
  let available: Vec<&str> = available.into_iter().collect();
  let qualified = format!("{username}@{hostname}");
  if available.contains(&qualified.as_str()) {
    return Some(qualified);
  }
  available
    .into_iter()
    .find(|&name| name == username)
    .map(str::to_owned)
}

/// Which of embedded, standalone, both, or neither home-manager
/// activation applies to a user.
///
/// Independent of [`SystemDeploymentKind`]: the four combinations here
/// are not a fourth OS-scope mechanism, they describe how `Scope::User`
/// is (or isn't) wired relative to whatever `Scope::System` mechanism, if
/// any, is in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HomeActivationMode {
  /// A `home-manager-<user>.service` unit is present; no discoverable
  /// standalone output.
  Embedded,
  /// A discoverable standalone output exists; no embedded unit.
  Standalone,
  /// Both the embedded unit and a discoverable standalone output exist.
  Both,
  /// Neither signal is present.
  None,
}

impl HomeActivationMode {
  /// Resolve from the two independent signals: whether the embedded unit
  /// is present, and whether a standalone output was discovered (e.g. via
  /// [`discover_standalone_home_output`]).
  #[must_use]
  pub const fn resolve(embedded: bool, standalone: bool) -> Self {
    match (embedded, standalone) {
      (true, true) => Self::Both,
      (true, false) => Self::Embedded,
      (false, true) => Self::Standalone,
      (false, false) => Self::None,
    }
  }
}

/// Whether this source's remote is a local filesystem path rather than
/// a URL or scp-style spec, and if so, that path.
///
/// Mirrors the three forms [`super::config::validate_remote_url`]
/// accepts: a `scheme://` URL, an scp-style `[user@]host:path`, or a
/// bare absolute path. Only the last one is "local" in the sense this
/// reports: a clone xi reads straight from disk, not over any
/// transport.
#[must_use]
pub fn local_sources_path(remote: &str) -> Option<&str> {
  let trimmed = remote.trim();
  if trimmed.is_empty()
    || trimmed.contains("://")
    || scp_separator(trimmed).is_some()
  {
    return None;
  }
  Path::new(trimmed).is_absolute().then_some(trimmed)
}

/// Split an `active-generation` state value into the store path and
/// whether [`super::conflict::DIRTY_SOURCE_MARKER`] is present.
///
/// The marker is appended in-line (`"<path> (dirty-source)"`, see
/// `switch::mark_dirty_source`), so a plain string match is all reading
/// it back needs.
#[must_use]
pub fn split_dirty_marker(generation: &str) -> (&str, bool) {
  let marker = super::conflict::DIRTY_SOURCE_MARKER;
  generation
    .strip_suffix(&format!(" ({marker})"))
    .map_or((generation, false), |path| (path, true))
}

/// Whether the gitops daemon's systemd unit is active for this scope.
///
/// `None` means the question could not be answered at all (no
/// `systemctl` on this host, most likely a non-systemd system); that is
/// different from `Some(false)`, which means systemd answered and the
/// unit is not running, either never enabled or stopped.
#[must_use]
pub fn gitops_daemon_active(scope: Scope) -> Option<bool> {
  let mut cmd = Command::new("systemctl");
  if scope == Scope::User {
    cmd.arg("--user");
  }
  cmd.args(["is-active", SYSTEMD_UNIT]);
  Some(cmd.output().ok()?.status.success())
}

/// Closure size (bytes) and path count of a store path, via `nix
/// path-info`.
///
/// Best-effort: a missing or unreadable `nix` binary, a store path
/// that no longer exists (garbage-collected since it was recorded), or
/// unparsable output all just mean this metric is omitted, never a
/// reason to fail the whole status report.
#[must_use]
pub fn closure_stats(store_path: &str) -> Option<ClosureStats> {
  let nix = nix_command::find_real_nix_binary();

  let size_out = Command::new(&nix)
    .args(["path-info", "--closure-size", "--json", store_path])
    .output()
    .ok()?;
  let size_json: serde_json::Value =
    serde_json::from_slice(&size_out.stdout).ok()?;
  let total_size = sum_json_numeric_field(&size_json, "closureSize")?;

  let count_out = Command::new(&nix)
    .args(["path-info", "--recursive", "--json", store_path])
    .output()
    .ok()?;
  let count_json: serde_json::Value =
    serde_json::from_slice(&count_out.stdout).ok()?;
  let path_count = json_entry_count(&count_json);

  Some(ClosureStats {
    total_size_bytes: total_size,
    path_count,
  })
}

/// `nix path-info --json` is an array in modern `nix`, an object keyed
/// by store path in older ones; handle both rather than pinning a
/// version.
fn sum_json_numeric_field(
  value: &serde_json::Value,
  field: &str,
) -> Option<u64> {
  let entries: Vec<&serde_json::Value> = match value {
    serde_json::Value::Array(items) => items.iter().collect(),
    serde_json::Value::Object(map) => map.values().collect(),
    _ => return None,
  };
  if entries.is_empty() {
    return None;
  }
  Some(
    entries
      .iter()
      .filter_map(|entry| entry.get(field).and_then(serde_json::Value::as_u64))
      .sum(),
  )
}

fn json_entry_count(value: &serde_json::Value) -> usize {
  match value {
    serde_json::Value::Array(items) => items.len(),
    serde_json::Value::Object(map) => map.len(),
    _ => 0,
  }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ClosureStats {
  /// Total closure size, in bytes.
  pub total_size_bytes: u64,
  /// Number of store paths in the closure.
  pub path_count:       usize,
}

impl ClosureStats {
  /// Render the size as a human-scaled string, e.g. `"21.6 GiB"`.
  #[must_use]
  #[expect(
    clippy::cast_precision_loss,
    reason = "a store closure in the petabyte range, where this would \
              actually lose a digit, is not a real scenario"
  )]
  pub fn human_size(self) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = self.total_size_bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
      size /= 1024.0;
      unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
  }
}

#[derive(Debug, Clone, clap::Args)]
pub struct DeploymentStatusArgs {
  /// Emit the house JSON envelope instead of a human summary.
  #[arg(long)]
  pub json: bool,
}

impl DeploymentStatusArgs {
  /// Print this scope's deployment overview.
  ///
  /// # Errors
  ///
  /// Propagates config-load errors, I/O errors from the state-dir load,
  /// and JSON serialisation errors.
  pub fn run(self, scope: Scope) -> Result<()> {
    let paths = ScopePaths::resolve(scope);
    let resolved = resolve_layers(SourcesConfig::default(), &paths)?;

    // The configured value, not the recorded one: "is chezmoi-style
    // reconciliation set up" should answer from `programs.xi.sources`
    // the moment it is declared, not only after a switch has run once
    // to record it into the state dir.
    let configured_working_dir = resolved.working_dir.clone();
    let remote = resolved.remote.unwrap_or_else(|| {
      super::read_state_file(&paths.state_dir, "remote-url")
        .ok()
        .flatten()
        .unwrap_or_default()
    });
    let applied_via = super::read_state_file(&paths.state_dir, "applied-via")
      .ok()
      .flatten();
    let applied_at = super::read_state_file(&paths.state_dir, "applied-at")
      .ok()
      .flatten();

    let sources_id = SourcesId::new(scope, &remote);
    let context =
      SourcesContext::load(sources_id, paths.sources_dir, paths.state_dir)?;
    let status = context.status(&GitopsHealth::Disabled);

    let deployment_kind = SystemDeploymentKind::detect();
    let gitops_active = gitops_daemon_active(scope);
    let local_path = local_sources_path(&remote).map(str::to_owned);

    let username =
      std::env::var("USER").unwrap_or_else(|_| "<user>".to_owned());
    let embedded_home_manager =
      scope == Scope::User && embedded_home_manager_unit(&username);

    let recorded_working_dir = match &status {
      SourcesStatus::Ready { snapshot }
      | SourcesStatus::Pending { snapshot }
      | SourcesStatus::Degraded { snapshot, .. } => {
        snapshot.working_dir.clone()
      },
      SourcesStatus::Missing { .. } => None,
    };
    let working_dir = configured_working_dir.or(recorded_working_dir);

    if self.json {
      let (generation_path, dirty) = match &status {
        SourcesStatus::Ready { snapshot }
        | SourcesStatus::Pending { snapshot }
        | SourcesStatus::Degraded { snapshot, .. } => {
          snapshot.refs.active_generation.as_deref().map_or(
            (None, false),
            |g| {
              let (path, dirty) = split_dirty_marker(g);
              (Some(path.to_owned()), dirty)
            },
          )
        },
        SourcesStatus::Missing { .. } => (None, false),
      };
      let closure = generation_path.as_deref().and_then(closure_stats);

      let payload = serde_json::json!({
        "xi_version": crate::XI_VERSION,
        "xi_rev": crate::XI_REV,
        "scope": scope.as_str(),
        "system_deployment": deployment_kind,
        "embedded_home_manager": embedded_home_manager,
        "gitops_daemon_active": gitops_active,
        "local_sources_path": local_path,
        "working_dir": working_dir,
        "applied_via": applied_via,
        "applied_at": applied_at,
        "dirty": dirty,
        "closure": closure,
        "sources": status,
      });
      println!("{}", serde_json::to_string_pretty(&payload)?);
      return Ok(());
    }

    print_human(PrintHumanArgs {
      scope,
      deployment_kind,
      gitops_active,
      local_path: local_path.as_deref(),
      working_dir: working_dir.as_deref(),
      embedded_home_manager,
      username: &username,
      applied_via: applied_via.as_deref(),
      applied_at: applied_at.as_deref(),
      status: &status,
    });
    Ok(())
  }
}

/// Bundled arguments for [`print_human`], grouped to stay under clippy's
/// argument-count limit rather than because callers share them.
#[derive(Clone, Copy)]
struct PrintHumanArgs<'a> {
  scope:                 Scope,
  deployment_kind:       SystemDeploymentKind,
  gitops_active:         Option<bool>,
  local_path:            Option<&'a str>,
  working_dir:           Option<&'a Path>,
  embedded_home_manager: bool,
  username:              &'a str,
  applied_via:           Option<&'a str>,
  applied_at:            Option<&'a str>,
  status:                &'a SourcesStatus,
}

fn print_human(args: PrintHumanArgs<'_>) {
  let PrintHumanArgs {
    scope,
    deployment_kind,
    gitops_active,
    local_path,
    working_dir,
    embedded_home_manager,
    username,
    applied_via,
    applied_at,
    status,
  } = args;
  let rev = crate::XI_REV.unwrap_or("unknown");
  println!("{} {} ({rev})", style::bold("xi"), crate::XI_VERSION);
  println!(
    "{} {} ({})",
    style::bold("scope:"),
    scope.as_str(),
    if scope == Scope::System {
      "host-wide"
    } else {
      "per-user"
    }
  );

  println!(
    "{} {}",
    style::bold("system deployment:"),
    match deployment_kind {
      SystemDeploymentKind::None => style::dim(deployment_kind.label()),
      _ => style::colored(deployment_kind.label(), style::color::BLUE),
    }
  );
  if scope == Scope::User && embedded_home_manager {
    println!(
      "  {} home-manager-{username}.service embeds this scope's activation; a \
       standalone `xi home switch` here runs independently of it",
      Icon::Warn.render()
    );
  }

  println!("{} {}", style::bold("sync mode:"), match gitops_active {
    Some(true) =>
      style::colored("pull (gitops daemon is active)", style::color::GREEN),
    Some(false) =>
      style::dim("manual (gitops daemon not running; push or direct switch)"),
    None => style::dim("manual (no systemd to check a gitops daemon against)"),
  });

  if let Some(path) = local_path {
    println!("{} {path}", style::bold("sources origin:"));
  }
  if let Some(dir) = working_dir {
    println!(
      "{} {} {}",
      style::bold("working_dir:"),
      dir.display(),
      style::dim("(chezmoi-style reconciliation against this checkout)")
    );
  }

  println!();
  match status {
    SourcesStatus::Ready { snapshot }
    | SourcesStatus::Pending { snapshot }
    | SourcesStatus::Degraded { snapshot, .. } => {
      if let SourcesStatus::Degraded { reason, .. } = status {
        println!(
          "{}",
          style::labeled_status(
            Icon::Warn,
            "sources",
            &format!("degraded ({reason})")
          )
        );
      } else {
        println!("{}", style::bold("sources:"));
      }
      println!("  remote: {}", snapshot.sources_id.canonical_url);
      if let Some(head) = &snapshot.refs.sources_head {
        println!("  sources_head: {head}");
      }
      match snapshot.refs.active_generation.as_deref() {
        Some(generation) => {
          let (path, dirty) = split_dirty_marker(generation);
          print!("  active_generation: {path}");
          if dirty {
            print!(
              " {}",
              style::colored("(dirty-source)", style::color::YELLOW)
            );
          }
          println!();

          if let Some(stats) = closure_stats(path) {
            println!(
              "  closure: {} across {} path(s)",
              stats.human_size(),
              stats.path_count
            );
          }
        },
        None => println!("  active_generation: none yet (never switched)"),
      }
      if let Some(via) = applied_via {
        print!("  applied_via: {via}");
        if let Some(at) = applied_at {
          print!(" ({at})");
        }
        println!();
      }
      if snapshot.sync_available {
        println!(
          "  sync: available (sources dir is ahead; `xi {} sources sync` to \
           apply)",
          scope.as_str()
        );
      }
    },
    SourcesStatus::Missing { .. } => {
      println!(
        "{}",
        style::labeled_status(
          Icon::Info,
          "sources",
          &format!(
            "not configured for this scope (`xi {} switch <remote-url>` to \
             adopt one)",
            scope.as_str()
          )
        )
      );
    },
  }
}

#[cfg(test)]
mod detect_with_sources_tests {
  use super::SystemDeploymentKind;

  /// A sources closure that fails the test if it is ever invoked, for
  /// asserting that host state short-circuits before any source lookup.
  fn unreachable_attribute_exists(_attribute: &str) -> bool {
    panic!("sources must not be consulted when host state already resolved a mechanism")
  }

  #[test]
  fn host_state_nixos_wins_without_consulting_sources() {
    let result = SystemDeploymentKind::resolve_with_sources(
      SystemDeploymentKind::NixOs,
      "github:example/flake",
      "host1",
      &unreachable_attribute_exists,
    );
    assert_eq!(result, Ok(SystemDeploymentKind::NixOs));
  }

  #[test]
  fn host_state_system_manager_wins_without_consulting_sources() {
    let result = SystemDeploymentKind::resolve_with_sources(
      SystemDeploymentKind::SystemManager,
      "github:example/flake",
      "host1",
      &unreachable_attribute_exists,
    );
    assert_eq!(result, Ok(SystemDeploymentKind::SystemManager));
  }

  #[test]
  fn host_state_darwin_wins_without_consulting_sources() {
    let result = SystemDeploymentKind::resolve_with_sources(
      SystemDeploymentKind::Darwin,
      "github:example/flake",
      "host1",
      &unreachable_attribute_exists,
    );
    assert_eq!(result, Ok(SystemDeploymentKind::Darwin));
  }
}

#[cfg(test)]
mod home_activation_mode_tests {
  use super::{HomeActivationMode, discover_standalone_home_output};

  #[test]
  fn resolves_embedded_only() {
    assert_eq!(
      HomeActivationMode::resolve(true, false),
      HomeActivationMode::Embedded
    );
  }

  #[test]
  fn resolves_standalone_only() {
    assert_eq!(
      HomeActivationMode::resolve(false, true),
      HomeActivationMode::Standalone
    );
  }

  #[test]
  fn resolves_both() {
    assert_eq!(
      HomeActivationMode::resolve(true, true),
      HomeActivationMode::Both
    );
  }

  #[test]
  fn resolves_none() {
    assert_eq!(
      HomeActivationMode::resolve(false, false),
      HomeActivationMode::None
    );
  }

  #[test]
  fn discovers_host_qualified_name_first() {
    let available = ["alice", "alice@host"];
    assert_eq!(
      discover_standalone_home_output("alice", "host", available),
      Some("alice@host".to_owned())
    );
  }

  #[test]
  fn falls_back_to_bare_username() {
    let available = ["alice"];
    assert_eq!(
      discover_standalone_home_output("alice", "host", available),
      Some("alice".to_owned())
    );
  }

  #[test]
  fn discovers_nothing_for_an_unrelated_output_set() {
    let available = ["bob", "bob@host"];
    assert_eq!(
      discover_standalone_home_output("alice", "host", available),
      None
    );
  }
}

/// Drives all four [`HomeActivationMode`] combinations through the real
/// detection functions: a fixture filesystem standing in for
/// `/etc/systemd/system` (the embedded signal) crossed with a fixture
/// `homeConfigurations` output set (the standalone signal), rather than
/// handing `HomeActivationMode::resolve` pre-computed booleans.
#[cfg(test)]
mod activation_mode_fixture_tests {
  use tempfile::TempDir;

  use super::{
    HomeActivationMode,
    discover_standalone_home_output,
    embedded_home_manager_unit_under,
  };

  const USERNAME: &str = "alice";
  const HOSTNAME: &str = "host1";

  fn write_embedded_unit(root: &std::path::Path) {
    let unit_dir = root.join("etc/systemd/system");
    std::fs::create_dir_all(&unit_dir).expect("create fixture systemd dir");
    std::fs::write(
      unit_dir.join(format!("home-manager-{USERNAME}.service")),
      "[Unit]\n",
    )
    .expect("write fixture systemd unit");
  }

  fn resolve_against_fixtures<'a>(
    root: &std::path::Path,
    available: impl IntoIterator<Item = &'a str>,
  ) -> HomeActivationMode {
    let embedded = embedded_home_manager_unit_under(root, USERNAME);
    let standalone =
      discover_standalone_home_output(USERNAME, HOSTNAME, available).is_some();
    HomeActivationMode::resolve(embedded, standalone)
  }

  #[test]
  fn embedded_only_from_fixtures() {
    let tmp = TempDir::new().expect("tempdir");
    write_embedded_unit(tmp.path());

    assert_eq!(
      resolve_against_fixtures(tmp.path(), []),
      HomeActivationMode::Embedded
    );
  }

  #[test]
  fn standalone_only_from_fixtures() {
    let tmp = TempDir::new().expect("tempdir");

    assert_eq!(
      resolve_against_fixtures(tmp.path(), [USERNAME]),
      HomeActivationMode::Standalone
    );
  }

  #[test]
  fn both_from_fixtures() {
    let tmp = TempDir::new().expect("tempdir");
    write_embedded_unit(tmp.path());
    let qualified = format!("{USERNAME}@{HOSTNAME}");

    assert_eq!(
      resolve_against_fixtures(tmp.path(), [qualified.as_str()]),
      HomeActivationMode::Both
    );
  }

  #[test]
  fn none_from_fixtures() {
    let tmp = TempDir::new().expect("tempdir");

    assert_eq!(
      resolve_against_fixtures(tmp.path(), []),
      HomeActivationMode::None
    );
  }
}
