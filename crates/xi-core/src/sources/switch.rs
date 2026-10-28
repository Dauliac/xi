//! The switch entrypoint: installer pre-check, then source adoption.
//!
//! `xi <scope> switch [<remote-url>]` is the single way a machine is
//! pointed at a config repo, on a bare machine and on a managed one
//! alike. [`prepare`] is the shared pre-step every scope's `switch`
//! runs before it resolves an installable, so all four verbs behave
//! identically. See `openspec/changes/xi-sources-config/design.md`.

use std::{
  path::{Path, PathBuf},
  process::Command,
};

use clap::Args;
use color_eyre::{Result, eyre::eyre};
use tracing::{debug, info, warn};

use super::{
  Scope,
  config::{
    DEFAULT_BRANCH,
    Installer,
    PositionalKind,
    ScopePaths,
    SourcesConfig,
    classify_positional,
  },
  read_state_file,
  write_state_file,
};
use crate::installable::{Installable, InstallableArgs};

/// Source-related flags shared by every scope's `switch`.
#[derive(Debug, Clone, Args)]
pub struct SwitchSourcesArgs {
  /// Branch to track on the adopted remote.
  ///
  /// Top rung of the ladder for this key. Needed on the command line
  /// because a machine being adopted for the first time has no
  /// `.xi.toml` yet, and the bootstrap one-liner has nowhere else to
  /// put it.
  #[arg(long)]
  pub branch: Option<String>,

  /// Read the positional as a config repo to adopt, whatever its shape.
  #[arg(long, conflicts_with = "no_adopt")]
  pub adopt: bool,

  /// Read the positional as a flake installable, whatever its shape.
  #[arg(long)]
  pub no_adopt: bool,

  /// Allow re-pointing this scope at a different remote.
  #[arg(long)]
  pub force: bool,
}

impl SwitchSourcesArgs {
  /// How to read `raw`, honouring an explicit override.
  fn kind_of(&self, raw: &str) -> PositionalKind {
    if self.adopt {
      return PositionalKind::ConfigRemote;
    }
    if self.no_adopt {
      return PositionalKind::Installable;
    }
    classify_positional(raw)
  }
}

/// Run the switch pre-steps and return the installable to build from.
///
/// Three cases, in order:
///
/// 1. The positional is a config-repo remote: adopt it, then build from the
///    sources dir.
/// 2. No positional, but this scope has a recorded source: build from the
///    sources dir.
/// 3. Anything else: hand back the installable untouched, so `xi <scope> switch
///    .` and `xi <scope> switch github:you/cfg` behave exactly as they did
///    before this capability existed.
///
/// The installer pre-check runs first in every case, including case 3,
/// because reaching a switch without Nix is the same problem however
/// the user spelled the argument.
///
/// # Errors
///
/// Propagates config-load errors, the installer pre-check failure, and
/// clone or state-write failures.
pub fn prepare(
  scope: Scope,
  installable: InstallableArgs,
  opts: &SwitchSourcesArgs,
) -> Result<InstallableArgs> {
  let paths = ScopePaths::resolve(scope);
  // `--branch` is the top rung for that key. It has to be reachable from
  // the command line: a machine being adopted for the first time has no
  // `.xi.toml`, and the bootstrap one-liner has nowhere else to put it.
  let cli = SourcesConfig {
    branch: opts.branch.clone(),
    ..SourcesConfig::default()
  };
  let resolved = super::args::resolve_layers(cli, &paths)?;

  ensure_nix_available(resolved.installer)?;

  // Case 1: an explicit remote on the command line.
  if let InstallableArgs::Specified(Installable::Flake {
    ref reference,
    ref attribute,
  }) = installable
    && opts.kind_of(reference) == PositionalKind::ConfigRemote
  {
    adopt(scope, reference, &resolved.branch, &paths, opts.force)?;
    return Ok(sources_installable(&paths.sources_dir, attribute.clone()));
  }

  // Case 2: nothing on the command line, but this scope is adopted.
  if matches!(installable, InstallableArgs::Unspecified)
    && let Some(recorded) = recorded_remote(&paths.state_dir)?
  {
    debug!("using recorded source for {scope}: {recorded}");
    if paths.sources_dir.join(".git").is_dir() {
      return Ok(sources_installable(&paths.sources_dir, Vec::new()));
    }
    warn!(
      "{scope} records {recorded} but {} is not a clone; run `xi {} switch \
       {recorded}` to restore it",
      paths.sources_dir.display(),
      scope.as_str()
    );
  }

  // Case 3: unchanged.
  Ok(installable)
}

/// A flake installable rooted at the managed sources dir.
fn sources_installable(
  sources_dir: &Path,
  attribute: Vec<String>,
) -> InstallableArgs {
  InstallableArgs::Specified(Installable::Flake {
    reference: sources_dir.display().to_string(),
    attribute,
  })
}

/// The remote this scope is adopted to, if any.
fn recorded_remote(state_dir: &Path) -> Result<Option<String>> {
  Ok(read_state_file(state_dir, "remote-url")?)
}

/// Adopt `remote` as this scope's source and make the clone current.
///
/// Idempotent for the remote already recorded: that case fast-forwards
/// and returns, which is what lets one command be correct on a fresh
/// machine and on a managed one. A *different* remote is refused
/// without `force`, because silently repointing what a host deploys
/// from is not something to do on a typo.
fn adopt(
  scope: Scope,
  remote: &str,
  branch: &str,
  paths: &ScopePaths,
  force: bool,
) -> Result<()> {
  super::config::validate_remote_url(remote).map_err(|e| eyre!(e))?;

  let recorded = recorded_remote(&paths.state_dir)?;
  let repointing = recorded.as_deref().is_some_and(|r| r != remote);

  if repointing && !force {
    let recorded = recorded.unwrap_or_default();
    return Err(eyre!(
      "{scope} scope is already adopted to {recorded}; refusing to re-point \
       it at {remote} without --force"
    ));
  }

  for dir in [&paths.sources_dir, &paths.state_dir] {
    create_private_dir(dir)?;
  }

  if paths.sources_dir.join(".git").is_dir() {
    // Under --force the existing clone still points at the old origin.
    // Recording the new URL without moving origin would leave the two
    // silently disagreeing, and every later fetch would quietly keep
    // pulling the remote the operator just replaced.
    if repointing {
      info!("re-pointing origin at {remote}");
      set_origin(&paths.sources_dir, remote)?;
    }
    info!("updating {}", paths.sources_dir.display());
    fetch_and_reset(&paths.sources_dir, branch)?;
  } else {
    info!("cloning {remote} into {}", paths.sources_dir.display());
    clone(remote, branch, &paths.sources_dir)?;
  }

  write_state_file(&paths.state_dir, "remote-url", remote)?;
  Ok(())
}

/// Create a directory owner-only, not whatever the umask allows.
///
/// `create_dir_all` honours the process umask, which on a typical
/// desktop is 002 and yields 0775. A config repo clone is private by
/// contract (the path table requires 0700), and adoption is the path
/// that creates it first on a fresh machine, before any Nix module
/// activation gets a chance to chmod it.
fn create_private_dir(dir: &Path) -> Result<()> {
  std::fs::create_dir_all(dir)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

/// `git clone` into a directory that may already exist but is empty.
fn clone(remote: &str, branch: &str, dest: &Path) -> Result<()> {
  // `GIT_TERMINAL_PROMPT=0` is deliberate: without it a missing
  // credential makes git block on a prompt, turning a scripted switch
  // into a hang instead of a clean failure.
  let out = Command::new("git")
    .env("GIT_TERMINAL_PROMPT", "0")
    .args(["clone", "--branch", branch, "--"])
    .arg(remote)
    .arg(dest)
    .output()?;
  if out.status.success() {
    return Ok(());
  }

  let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
  // A missing branch is the single most likely adoption failure, because
  // xi defaults to `main` and plenty of repos are still on `master`.
  // Git's own message never says where the branch name came from.
  if stderr.contains("Remote branch") && stderr.contains("not found") {
    return Err(eyre!(
      "the remote has no branch `{branch}`.\n\n{stderr}\n\nxi tracks \
       `{DEFAULT_BRANCH}` unless told otherwise. Pick the branch with \
       `--branch <name>`, `XI_SOURCES_BRANCH`, or `[sources] branch` in \
       .xi.toml."
    ));
  }
  Err(eyre!("git clone failed: {stderr}"))
}

/// Point an existing clone at a different origin.
fn set_origin(repo: &Path, remote: &str) -> Result<()> {
  let out = Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["remote", "set-url", "origin"])
    .arg(remote)
    .output()?;
  if out.status.success() {
    return Ok(());
  }
  Err(eyre!(
    "git remote set-url failed: {}",
    String::from_utf8_lossy(&out.stderr).trim()
  ))
}

/// Fast-forward an existing clone to the tracked branch.
fn fetch_and_reset(repo: &Path, branch: &str) -> Result<()> {
  for args in [
    vec!["fetch", "origin", branch],
    vec!["checkout", branch],
    vec!["reset", "--hard", &format!("origin/{branch}")],
  ] {
    let out = Command::new("git")
      .env("GIT_TERMINAL_PROMPT", "0")
      .arg("-C")
      .arg(repo)
      .args(&args)
      .output()?;
    if !out.status.success() {
      return Err(eyre!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
      ));
    }
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Installer pre-check
// ---------------------------------------------------------------------------

/// Resolve this scope's `installer` setting and run the pre-check.
///
/// # Errors
///
/// Propagates config-load errors and the pre-check failure.
pub fn ensure_nix_available_for(scope: Scope) -> Result<()> {
  // Resolving config needs no Nix, so this is safe to do first.
  let paths = ScopePaths::resolve(scope);
  let resolved = super::args::resolve_layers(SourcesConfig::default(), &paths)?;
  ensure_nix_available(resolved.installer)
}

/// Ensure a usable `nix` exists before any evaluation work starts.
///
/// This runs on *every* switch, not only the bootstrap path: arriving
/// at a switch without Nix is the same problem whether the operator
/// passed a remote, a local flake, or nothing, and a bare
/// `nix: command not found` is a worse answer than naming the fix.
///
/// When Nix is present this is a pure PATH lookup: no network, no
/// prompt, no measurable cost on the everyday switch.
///
/// # Errors
///
/// Returns an error naming the exact installer command when Nix is
/// absent, or naming the `installer` config key when it is `none`.
pub fn ensure_nix_available(installer: Option<Installer>) -> Result<()> {
  if which::which("nix").is_ok() {
    debug!("nix found on PATH; installer pre-check is a no-op");
    return Ok(());
  }

  match installer {
    Some(Installer::None) => {
      Err(eyre!(
        "nix is not on PATH and `installer = \"none\"` opts out of installing \
         it.\nSet `installer` to `detsys` or `lix` in [sources], or install \
         Nix yourself and re-run."
      ))
    },
    // Receipt-backed auto-install is owned by xi-sources-bootstrap and
    // is not implemented yet. Printing the exact command the operator
    // can paste is honest; running an OS-level installer with no
    // receipt to reverse it would not be.
    chosen => {
      let backend = chosen.unwrap_or(Installer::Detsys);
      Err(eyre!(
        "nix is not on PATH.\n\nxi can install it for you once \
         xi-sources-bootstrap lands. Until then, run:\n\n  {}\n\nthen re-run \
         this command.",
        install_command(backend)
      ))
    },
  }
}

/// The documented one-liner for each installer backend.
#[must_use]
pub const fn install_command(backend: Installer) -> &'static str {
  match backend {
    Installer::Lix => {
      "curl -sSf -L https://install.lix.systems/lix | sh -s -- install \
       --no-confirm"
    },
    Installer::Detsys | Installer::None => {
      "curl -LsSf https://install.determinate.systems/nix | sh -s -- install \
       --no-confirm --determinate=false"
    },
  }
}

/// Where the managed clone for `scope` lives.
#[must_use]
pub fn sources_dir_for(scope: Scope) -> PathBuf {
  ScopePaths::resolve(scope).sources_dir
}
