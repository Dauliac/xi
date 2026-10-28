//! The `xi sources` command surface.
//!
//! Two verbs today: the read-only `status` defined by the
//! `xi-sources-model` capability, and `init` from `xi-sources-config`.
//! Both print [`xi_core::sources::SourcesStatusResponse`] on stdout and
//! nothing else, preserving xi's stdout/stderr split: diagnostics go to
//! stderr via `tracing`, machine output goes to stdout.
//!
//! Path resolution is no longer provisional  -  [`ScopePaths`] is the
//! `xi-sources-config` table, and [`resolve_layers`] is the five-rung
//! precedence ladder every value here comes through.

pub mod args;

use std::{
  path::{Path, PathBuf},
  process::Command,
};

use color_eyre::{Result, eyre::eyre};
use tracing::{debug, warn};
use xi_core::sources::{
  GitopsHealth,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatus,
  SourcesStatusResponse,
  config::{
    BlockWrite,
    Layers,
    ResolvedSources,
    ScopePaths,
    SourcesConfig,
    WriteError,
  },
  verbless_switch_error,
};

use self::args::{
  InitArgs,
  InitScopeArg,
  SourcesCommand,
  SourcesProxy,
  StatusArgs,
};

impl SourcesProxy {
  /// Dispatch the selected `xi sources` verb.
  ///
  /// # Errors
  ///
  /// Propagates config-load, I/O and serialisation errors from the verbs.
  pub fn run(self) -> Result<()> {
    match self.command {
      SourcesCommand::Status(args) => args.run(),
      SourcesCommand::Init(args) => args.run(),
    }
  }
}

impl InitScopeArg {
  /// Resolve `auto` against the effective uid.
  ///
  /// Root configures the host, anyone else configures themselves. The
  /// check is on the *effective* uid so `sudo xi sources init` lands on
  /// the system scope, which is what the operator meant.
  #[must_use]
  pub fn resolve(self) -> Scope {
    match self {
      Self::System => Scope::System,
      Self::User => Scope::User,
      Self::Auto => {
        if nix::unistd::geteuid().is_root() {
          Scope::System
        } else {
          Scope::User
        }
      },
    }
  }
}

/// Build the precedence ladder for this invocation.
///
/// Rungs, highest first: the CLI flags the caller already parsed, the
/// `XI_SOURCES_*` environment, the project `.xi.toml [sources]`, and the
/// Nix-module-written per-scope `sources.toml`. The fifth rung (built-in
/// defaults) is supplied by [`Layers::resolve`].
///
/// # Errors
///
/// Propagates the load-time validation error from whichever surface is
/// malformed, so a bad value is reported with its file+line or variable
/// name rather than at first fetch.
fn resolve_layers(
  cli: SourcesConfig,
  paths: &ScopePaths,
) -> Result<ResolvedSources> {
  let layers = Layers {
    cli,
    env: SourcesConfig::from_env()?,
    toml: xi_flake::load_sources_config(None)?,
    nix_module: SourcesConfig::from_toml_file(&paths.config_file)?,
  };

  let resolved = layers.resolve();
  resolved.check_consistency().map_err(|e| eyre!(e))?;
  Ok(resolved)
}

impl StatusArgs {
  /// Print the `xi sources status` envelope on stdout.
  ///
  /// # Errors
  ///
  /// Propagates config-load errors, I/O errors from the state-dir load,
  /// and JSON serialisation errors.
  pub fn run(self) -> Result<()> {
    let scope = Scope::from(self.scope);
    let paths = ScopePaths::resolve(scope);

    let cli = SourcesConfig {
      remote: self.remote,
      ..SourcesConfig::default()
    };
    let resolved = resolve_layers(cli, &paths)?;

    // With no remote configured anywhere there is no identity to resolve,
    // so the answer is `Missing` by definition. Synthesising an empty id
    // keeps that path on the same code as every other status.
    let remote = resolved.remote.clone().unwrap_or_default();
    let sources_id = SourcesId::new(scope, &remote);

    let context =
      SourcesContext::load(sources_id, paths.sources_dir, paths.state_dir)?;

    // Gitops health is owned by `xi-sources-gitops`; until that daemon
    // exists there is nothing tracking this source.
    let status = context.status(&GitopsHealth::Disabled);

    let mut response = SourcesStatusResponse::new(status);
    if self.explain {
      response = response.with_provenance(resolved.provenance);
    }
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
  }
}

impl InitArgs {
  /// Configure a config repo for one scope and perform the first clone.
  ///
  /// The order is the spec's and matters: the refusal check runs before
  /// anything is created, so `init` against an already-configured
  /// `.xi.toml` exits non-zero having touched no file at all.
  ///
  /// # Errors
  ///
  /// Returns an error when the remote is invalid, when `.xi.toml` already
  /// carries a `[sources]` block and `--force` was not given, or when a
  /// directory cannot be created.
  pub fn run(self) -> Result<()> {
    let scope = self.scope.resolve();
    let paths = ScopePaths::resolve(scope);

    // Validate before touching the filesystem: a typo'd URL must not
    // leave a half-written config behind.
    xi_core::sources::config::validate_remote_url(&self.remote)
      .map_err(|e| eyre!(e))?;

    let cli = SourcesConfig {
      remote: Some(self.remote.clone()),
      branch: self.branch.clone(),
      working_dir: self.working_dir.clone(),
      ..SourcesConfig::default()
    };

    // The block we persist carries only what the operator supplied, so
    // today's defaults are not frozen into their file.
    let block = cli.clone();

    let config_path = Path::new(".xi.toml");
    match block.write_block(config_path, self.force) {
      Ok(BlockWrite::Created) => {
        debug!("created {} with a [sources] block", config_path.display());
      },
      Ok(outcome) => {
        debug!(?outcome, "updated {}", config_path.display());
      },
      Err(e @ WriteError::AlreadyConfigured { .. }) => {
        return Err(eyre!(e));
      },
      Err(e) => return Err(eyre!(e)),
    }

    let resolved = resolve_layers(cli, &paths)?;

    for dir in [
      &paths.sources_dir,
      &paths.state_dir,
      &config_parent(&paths.config_file),
    ] {
      std::fs::create_dir_all(dir)?;
    }

    let sources_id = SourcesId::new(scope, &self.remote);
    let outcome = if self.no_clone {
      CloneOutcome::Skipped
    } else {
      clone_into(&self.remote, &resolved.branch, &paths.sources_dir)
    };

    let context = SourcesContext::load(
      sources_id,
      paths.sources_dir.clone(),
      paths.state_dir.clone(),
    )?;
    let snapshot = context.snapshot();

    // `init`'s envelope reports the outcome of the init run, which is not
    // the same question `status` answers. A fresh clone has no active
    // generation, so `SourcesContext::status` would call it `Pending`
    // ("nothing switched yet") even though init itself fully succeeded.
    let status = match outcome {
      CloneOutcome::NeedsCredentials(reason) => {
        warn!("clone needs credentials: {reason}");
        SourcesStatus::Pending { snapshot }
      },
      CloneOutcome::Failed(reason) => {
        return Err(eyre!("clone failed: {reason}"));
      },
      CloneOutcome::Skipped | CloneOutcome::Done => {
        if resolved.require_signed
          && !head_is_signed(&paths.sources_dir).unwrap_or(false)
        {
          SourcesStatus::Degraded {
            reason: "require-signed is set but HEAD carries no usable \
                     signature"
              .to_owned(),
            snapshot,
          }
        } else {
          SourcesStatus::Ready { snapshot }
        }
      },
    };

    let response =
      SourcesStatusResponse::new(status).with_provenance(resolved.provenance);
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
  }
}

/// The directory holding a per-scope `sources.toml`.
fn config_parent(config_file: &Path) -> PathBuf {
  config_file
    .parent()
    .map_or_else(|| PathBuf::from("/"), Path::to_path_buf)
}

/// Why a clone did not end in a usable working tree.
enum CloneOutcome {
  /// The clone produced a tree.
  Done,
  /// `--no-clone` was passed.
  Skipped,
  /// Git could not authenticate; the operator must supply credentials.
  NeedsCredentials(String),
  /// Anything else.
  Failed(String),
}

/// Clone `remote` at `branch` into `dest`.
///
/// `GIT_TERMINAL_PROMPT=0` is deliberate: without it a missing credential
/// makes git block on a prompt, which turns a scripted `init` into a hang
/// instead of the `Pending` the spec asks for.
fn clone_into(remote: &str, branch: &str, dest: &Path) -> CloneOutcome {
  if dest.join(".git").is_dir() {
    debug!("{} is already a clone; leaving it alone", dest.display());
    return CloneOutcome::Done;
  }

  let output = Command::new("git")
    .env("GIT_TERMINAL_PROMPT", "0")
    .arg("clone")
    .arg("--branch")
    .arg(branch)
    .arg("--")
    .arg(remote)
    .arg(dest)
    .output();

  let output = match output {
    Ok(output) => output,
    Err(e) => return CloneOutcome::Failed(format!("could not run git: {e}")),
  };

  if output.status.success() {
    return CloneOutcome::Done;
  }

  let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
  if is_auth_failure(&stderr) {
    CloneOutcome::NeedsCredentials(stderr)
  } else {
    CloneOutcome::Failed(stderr)
  }
}

/// Recognise the ways git reports "I could not authenticate".
///
/// Matched on stderr text because git exits 128 for essentially every
/// failure, so the exit code alone cannot distinguish "wrong password"
/// from "no such branch".
fn is_auth_failure(stderr: &str) -> bool {
  const NEEDLES: &[&str] = &[
    "authentication failed",
    "could not read username",
    "could not read password",
    "permission denied (publickey",
    "terminal prompts disabled",
    "host key verification failed",
  ];
  let haystack = stderr.to_ascii_lowercase();
  NEEDLES.iter().any(|needle| haystack.contains(needle))
}

/// Whether HEAD carries a signature git is willing to verify.
///
/// `%G?` is `G` for a good signature, `U` for good-but-untrusted, and
/// `B`/`N`/`E` otherwise. Only `G` and `U` count as "signed" here; the
/// *trust* question belongs to `xi-sources-gitops`, which owns
/// `allowed-signers`.
fn head_is_signed(repo: &Path) -> Option<bool> {
  let output = Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["log", "-1", "--format=%G?"])
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  let marker = String::from_utf8_lossy(&output.stdout).trim().to_owned();
  Some(matches!(marker.as_str(), "G" | "U"))
}

/// Handler for the verb-less `xi switch`.
///
/// `switch` is accepted by the parser purely so this message can name the
/// four concrete verbs; clap's own "unrecognized subcommand" error would
/// not. No sources state is touched on this path.
///
/// # Errors
///
/// Always returns an error  -  that is the entire contract.
pub fn verbless_switch() -> Result<()> {
  Err(color_eyre::eyre::eyre!(verbless_switch_error()))
}
