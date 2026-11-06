//! The two side-effecting seams a push runs through.
//!
//! Everything `xi push` does to the world goes through [`Nix`] (local
//! evaluation and building) or [`Transport`] (anything that touches a
//! target). They are traits rather than free functions for one reason:
//! the spec's scenarios are about *fleet orchestration* - preflight
//! reporting per target, concurrency caps, rollback windows - and none of
//! that is testable against real hosts. A test supplies its own pair and
//! asserts on the calls.
//!
//! The real implementations are thin: they shell out to `nix`, `ssh`,
//! `scp`, `rsync`, and `nix-copy-closure` exactly as the research notes
//! describe the comparable tools doing.

use std::{
  path::Path,
  process::{Command, Stdio},
};

use color_eyre::{
  Result,
  eyre::{bail, eyre},
};
use nix_command::{CommandKind, NixCommand};
use tracing::debug;
use xi_core::{
  retry::{retry, RetryPolicy},
  sources::secrets::SecretSpec,
};

use super::{args::CopyVia, retry::classify_ssh_exit};

/// A target as the transport needs to address it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAddr {
  /// Flake attribute / node name, used in messages and in `--flake #<n>`.
  pub name:     String,
  /// `[user@]hostname` for `ssh`.
  pub ssh_host: String,
  /// Extra `ssh` options this target needs (e.g. `["-p", "2222"]`).
  pub ssh_opts: Vec<String>,
}

/// How a remote command wants to be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommand {
  /// The shell script to run on the target.
  pub script:    String,
  /// Allocate a PTY (`ssh -t`). Required for an interactive sudo prompt.
  pub pty:       bool,
  /// Wrap the script in elevation.
  pub elevate:   Elevate,
  /// Stream the remote output to this process's stdout/stderr.
  ///
  /// Off for probes, whose output is parsed; on for activation, whose
  /// output is the thing the operator is watching.
  pub streaming: bool,
  /// Retry attempts for this command's underlying `ssh` spawn.
  ///
  /// Only consulted on the `streaming` path - a probe never retries
  /// regardless of this value (see [`SshTransport::run`]), so
  /// [`Self::probe`] sets it to a meaningless `1`.
  pub retry_attempts: u32,
}

impl RemoteCommand {
  /// A non-elevated, non-interactive probe whose output is captured.
  #[must_use]
  pub fn probe(script: impl Into<String>) -> Self {
    Self {
      script:    script.into(),
      pty:       false,
      elevate:   Elevate::No,
      streaming: false,
      retry_attempts: 1,
    }
  }

  /// An elevated command, with the sudo mode the flags imply.
  #[must_use]
  pub fn elevated(
    script: impl Into<String>,
    interactive_sudo: bool,
    retry_attempts: u32,
  ) -> Self {
    Self {
      script:    script.into(),
      pty:       interactive_sudo,
      elevate:   if interactive_sudo {
        Elevate::Interactive
      } else {
        Elevate::NonInteractive
      },
      streaming: true,
      retry_attempts,
    }
  }
}

/// How to acquire root on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevate {
  /// Run as the SSH user.
  No,
  /// `sudo -n`: fail cleanly rather than block on a prompt.
  NonInteractive,
  /// `sudo`: a prompt is expected and a PTY is allocated for it.
  Interactive,
}

impl Elevate {
  /// Wrap `script` in the elevation this variant implies.
  #[must_use]
  pub fn wrap(self, script: &str) -> String {
    match self {
      Self::No => script.to_owned(),
      // `sudo -n` is the default on purpose: a push that silently waits
      // on an invisible password prompt looks like a hung network.
      Self::NonInteractive => format!("sudo -n sh -c {}", shell_quote(script)),
      Self::Interactive => format!("sudo sh -c {}", shell_quote(script)),
    }
  }
}

/// What a remote command did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
  /// Whether the remote command exited zero.
  pub success: bool,
  /// Captured stdout, empty for a streaming command.
  pub stdout:  String,
  /// Captured stderr, empty for a streaming command.
  pub stderr:  String,
}

impl Outcome {
  /// A successful outcome carrying `stdout`.
  #[must_use]
  pub fn ok(stdout: impl Into<String>) -> Self {
    Self {
      success: true,
      stdout:  stdout.into(),
      stderr:  String::new(),
    }
  }

  /// A failed outcome carrying `stderr`.
  #[must_use]
  pub fn failed(stderr: impl Into<String>) -> Self {
    Self {
      success: false,
      stdout:  String::new(),
      stderr:  stderr.into(),
    }
  }

  /// The first non-empty of stderr and stdout, for error messages.
  #[must_use]
  pub fn message(&self) -> &str {
    let stderr = self.stderr.trim();
    if stderr.is_empty() {
      self.stdout.trim()
    } else {
      stderr
    }
  }
}

/// Local Nix evaluation and building.
pub trait Nix: Send + Sync {
  /// Evaluate `installable` to JSON, optionally through `--apply`.
  ///
  /// `Ok(None)` means the attribute does not exist, which is how target
  /// discovery probes the flake without treating absence as an error.
  ///
  /// # Errors
  ///
  /// Returns an error if `nix` could not be spawned or its output is not
  /// the JSON the caller asked for.
  fn eval_json(
    &self,
    installable: &str,
    apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>>;

  /// Build `installable` and return its store path.
  ///
  /// # Errors
  ///
  /// Returns an error if the build fails.
  fn build(&self, installable: &str) -> Result<String>;
}

/// Everything that touches a target.
pub trait Transport: Send + Sync {
  /// Run a command on the target.
  ///
  /// A non-zero remote exit is an [`Outcome`] with `success: false`, not
  /// an `Err`: preflight has to report four failures per target rather
  /// than abort on the first.
  ///
  /// # Errors
  ///
  /// Returns an error only when the local `ssh` could not be spawned.
  fn run(&self, target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome>;

  /// Put a store closure on the target.
  ///
  /// # Errors
  ///
  /// Returns an error if the copy fails.
  fn copy_closure(
    &self,
    target: &TargetAddr,
    store_path: &str,
    via: CopyVia,
    retry_attempts: u32,
  ) -> Result<()>;

  /// Upload one secret to its destination with its owner, group and mode.
  ///
  /// # Errors
  ///
  /// Returns an error if the upload or the permission fix-up fails.
  fn upload_secret(
    &self,
    target: &TargetAddr,
    local: &Path,
    spec: &SecretSpec,
    interactive_sudo: bool,
    retry_attempts: u32,
  ) -> Result<()>;

  /// Mirror a directory onto the target.
  ///
  /// # Errors
  ///
  /// Returns an error if the sync fails.
  fn sync_dir(
    &self,
    target: &TargetAddr,
    local: &Path,
    remote: &str,
    excludes: &[&str],
    interactive_sudo: bool,
    retry_attempts: u32,
  ) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Real implementations
// ---------------------------------------------------------------------------

/// [`Nix`] backed by the `nix` CLI.
#[derive(Debug, Clone, Copy, Default)]
pub struct CliNix;

impl Nix for CliNix {
  fn eval_json(
    &self,
    installable: &str,
    apply: Option<&str>,
  ) -> Result<Option<serde_json::Value>> {
    let mut cmd = NixCommand::new(CommandKind::Eval).arg(installable);
    if let Some(apply) = apply {
      cmd = cmd.arg("--apply").arg(apply);
    }
    let output = cmd
      .arg("--json")
      .output()
      .map_err(|e| eyre!("failed to evaluate {installable}: {e}"))?;

    if !output.status.success() {
      debug!(
        installable,
        stderr = %String::from_utf8_lossy(&output.stderr),
        "attribute absent or not evaluable"
      );
      return Ok(None);
    }

    serde_json::from_slice(&output.stdout)
      .map(Some)
      .map_err(|e| eyre!("{installable} did not evaluate to JSON: {e}"))
  }

  fn build(&self, installable: &str) -> Result<String> {
    let output = NixCommand::new(CommandKind::Build)
      .arg(installable)
      .arg("--no-link")
      .arg("--print-out-paths")
      .output()
      .map_err(|e| eyre!("failed to build {installable}: {e}"))?;

    if !output.status.success() {
      bail!(
        "build failed for {installable}:\n{}",
        String::from_utf8_lossy(&output.stderr)
      );
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if path.is_empty() {
      bail!("{installable} built but produced no output path");
    }
    Ok(path)
  }
}

/// [`Transport`] backed by `ssh`, `scp`, `rsync` and `nix copy`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SshTransport;

impl SshTransport {
  /// Base options every SSH-family invocation gets.
  ///
  /// `BatchMode=yes` is the default because a push is usually scripted;
  /// the interactive-sudo path turns it off, since that mode exists
  /// precisely to let a prompt through.
  fn base_opts(target: &TargetAddr, interactive: bool) -> Vec<String> {
    let mut opts = vec![
      "-o".to_owned(),
      format!("BatchMode={}", if interactive { "no" } else { "yes" }),
      "-o".to_owned(),
      "ConnectTimeout=10".to_owned(),
    ];
    opts.extend(target.ssh_opts.iter().cloned());
    opts
  }
}

impl Transport for SshTransport {
  fn run(&self, target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
    let mut ssh = Command::new("ssh");
    ssh.args(Self::base_opts(target, cmd.pty));
    // `-t` forces a PTY so a sudo prompt is visible; `-T` suppresses one
    // so a non-interactive run cannot be derailed by terminal echo.
    ssh.arg(if cmd.pty { "-t" } else { "-T" });
    ssh.arg(&target.ssh_host);
    ssh.arg("--");
    ssh.arg(cmd.elevate.wrap(&cmd.script));

    if cmd.streaming {
      ssh
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
      // Only this branch retries: it is the elevated/activation path, never
      // a probe. A probe (the `!cmd.streaming` branch below) wants an
      // honest, immediate answer, not one that papers over the condition
      // it is testing for.
      let policy = RetryPolicy {
        max_attempts: cmd.retry_attempts,
        ..RetryPolicy::default()
      };
      let status = retry(&policy, classify_ssh_exit, || {
        ssh
          .status()
          .map_err(|e| eyre!("failed to run ssh for {}: {e}", target.name))
      })?;
      return Ok(Outcome {
        success: status.success(),
        stdout:  String::new(),
        stderr:  String::new(),
      });
    }

    let output = ssh
      .output()
      .map_err(|e| eyre!("failed to run ssh for {}: {e}", target.name))?;
    Ok(Outcome {
      success: output.status.success(),
      stdout:  String::from_utf8_lossy(&output.stdout).into_owned(),
      stderr:  String::from_utf8_lossy(&output.stderr).into_owned(),
    })
  }

  fn copy_closure(
    &self,
    target: &TargetAddr,
    store_path: &str,
    via: CopyVia,
    retry_attempts: u32,
  ) -> Result<()> {
    let policy = RetryPolicy {
      max_attempts: retry_attempts,
      ..RetryPolicy::default()
    };
    match via {
      // `nix-copy-closure --use-substitutes` makes the *target* fetch
      // what it can from its own caches, which is the deploy-rs
      // `substituteOnDestination` behaviour and the only sensible choice
      // when the operator's link is the bottleneck.
      CopyVia::Target => {
        let status = retry(&policy, classify_ssh_exit, || {
          Command::new("nix-copy-closure")
            .args(nix_copy_closure_args(target, store_path))
            .status()
            .map_err(|e| eyre!("failed to run nix-copy-closure: {e}"))
        })?;
        if !status.success() {
          bail!("nix-copy-closure to {} failed", target.name);
        }
        Ok(())
      },
      CopyVia::Local => {
        let last_stderr = std::cell::RefCell::new(String::new());
        let status = retry(&policy, classify_ssh_exit, || {
          let output = NixCommand::new(CommandKind::Copy)
            .arg("--to")
            .arg(format!("ssh://{}", target.ssh_host))
            .arg(store_path)
            .output()
            .map_err(|e| eyre!("failed to run nix copy: {e}"))?;
          *last_stderr.borrow_mut() = String::from_utf8_lossy(&output.stderr).into_owned();
          Ok(output.status)
        })?;
        if !status.success() {
          bail!(
            "nix copy to {} failed:\n{}",
            target.name,
            last_stderr.into_inner()
          );
        }
        Ok(())
      },
    }
  }

  fn upload_secret(
    &self,
    target: &TargetAddr,
    local: &Path,
    spec: &SecretSpec,
    interactive_sudo: bool,
    retry_attempts: u32,
  ) -> Result<()> {
    // Staged through a 0600 temp path owned by the SSH user, then moved
    // into place with `install`. Uploading straight to the destination
    // would leave the file world-readable for the width of the chmod,
    // and `install` sets owner, group and mode in one syscall.
    let staged = format!("/tmp/.xi-push-secret-{}", spec.name);

    let status = Command::new("scp")
      .args(Self::base_opts(target, false))
      .arg("--")
      .arg(local)
      .arg(format!("{}:{staged}", target.ssh_host))
      .status()
      .map_err(|e| eyre!("failed to run scp: {e}"))?;
    if !status.success() {
      bail!("scp of secret '{}' to {} failed", spec.name, target.name);
    }

    let script = install_secret_script(spec, &staged);
    let destination = spec.destination.display().to_string();

    let outcome = self.run(
      target,
      &RemoteCommand::elevated(script, interactive_sudo, retry_attempts),
    )?;
    if !outcome.success {
      bail!(
        "installing secret '{}' at {destination} on {} failed: {}",
        spec.name,
        target.name,
        outcome.message()
      );
    }
    Ok(())
  }

  fn sync_dir(
    &self,
    target: &TargetAddr,
    local: &Path,
    remote: &str,
    excludes: &[&str],
    interactive_sudo: bool,
    retry_attempts: u32,
  ) -> Result<()> {
    // The destination is usually root-owned, so rsync needs elevation on
    // the far side. `--rsync-path` is how that is spelled; the local
    // `sudo` would be the wrong machine.
    let rsync_path = if interactive_sudo {
      "sudo rsync"
    } else {
      "sudo -n rsync"
    };

    let policy = RetryPolicy {
      max_attempts: retry_attempts,
      ..RetryPolicy::default()
    };
    let status = retry(&policy, classify_ssh_exit, || {
      let mut rsync = Command::new("rsync");
      rsync.arg("--archive").arg("--delete").arg("--compress");
      for exclude in excludes {
        rsync.arg(format!("--exclude={exclude}"));
      }
      rsync.arg(format!("--rsync-path={rsync_path}"));
      rsync.arg("-e");
      rsync.arg(format!(
        "ssh {}",
        Self::base_opts(target, interactive_sudo).join(" ")
      ));
      // The trailing slash is load-bearing: without it rsync nests the
      // source directory inside the destination.
      rsync.arg(format!("{}/", local.display()));
      rsync.arg(format!("{}:{remote}", target.ssh_host));

      rsync
        .status()
        .map_err(|e| eyre!("failed to run rsync: {e}"))
    })?;
    if !status.success() {
      bail!("rsync of {} to {} failed", local.display(), target.name);
    }
    Ok(())
  }
}

/// Quote a string for a remote POSIX shell.
pub(crate) fn shell_quote(raw: &str) -> String {
  format!("'{}'", raw.replace('\'', r"'\''"))
}

/// The remote script [`SshTransport::upload_secret`] runs to move a
/// staged secret into place with its configured owner, group and mode.
///
/// `install` sets all three in one syscall, so there is no window where
/// the file exists at `staged` with other than `0600` (set by `scp`'s
/// own default) or at `destination` with other than its final mode  -
/// the whole reason the upload goes through a staged temp path instead
/// of writing `destination` directly.
///
/// Pulled out of [`SshTransport::upload_secret`] so the exact script,
/// and in particular that `-m {mode}` carries [`SecretSpec::mode`]
/// through untouched, is assertable without a [`Transport`].
pub(crate) fn install_secret_script(spec: &SecretSpec, staged: &str) -> String {
  let group = spec
    .group
    .as_deref()
    .map_or_else(String::new, |g| format!("-g {}", shell_quote(g)));
  let destination = spec.destination.display().to_string();
  format!(
    "set -e; mkdir -p {parent}; install -o {owner} {group} -m {mode} {staged} \
     {dest}; rm -f {staged}",
    parent = shell_quote(
      spec
        .destination
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .to_string_lossy()
        .as_ref()
    ),
    owner = shell_quote(&spec.owner),
    mode = shell_quote(&spec.mode),
    staged = shell_quote(staged),
    dest = shell_quote(&destination),
  )
}

/// The argv `--copy-via target` runs `nix-copy-closure` with.
///
/// Pulled out of [`SshTransport::copy_closure`] so the exact spelling
/// the `xi-sources-push` spec names -
/// `nix-copy-closure --to <host> --use-substitutes` - is assertable
/// without spawning the real binary, the same way [`SshTransport::base_opts`]
/// keeps the `ssh` argv testable.
pub(crate) fn nix_copy_closure_args(
  target: &TargetAddr,
  store_path: &str,
) -> Vec<String> {
  vec![
    "--to".to_owned(),
    target.ssh_host.clone(),
    "--use-substitutes".to_owned(),
    store_path.to_owned(),
  ]
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn non_interactive_elevation_uses_sudo_n() {
    let wrapped = Elevate::NonInteractive.wrap("true");
    assert!(wrapped.starts_with("sudo -n "), "got: {wrapped}");
  }

  #[test]
  fn interactive_elevation_drops_the_n_flag() {
    let wrapped = Elevate::Interactive.wrap("true");
    assert!(wrapped.starts_with("sudo sh"), "got: {wrapped}");
  }

  #[test]
  fn an_interactive_command_asks_for_a_pty() {
    let cmd = RemoteCommand::elevated("true", true, 3);
    assert!(cmd.pty);
    assert_eq!(cmd.elevate, Elevate::Interactive);
  }

  #[test]
  fn a_default_command_asks_for_no_pty() {
    let cmd = RemoteCommand::elevated("true", false, 3);
    assert!(!cmd.pty);
    assert_eq!(cmd.elevate, Elevate::NonInteractive);
  }

  #[test]
  fn an_elevated_command_carries_its_retry_attempts() {
    let cmd = RemoteCommand::elevated("true", false, 7);
    assert_eq!(cmd.retry_attempts, 7);
  }

  #[test]
  fn a_probe_never_asks_for_more_than_one_attempt() {
    let cmd = RemoteCommand::probe("true");
    assert_eq!(cmd.retry_attempts, 1);
  }

  #[test]
  fn quoting_survives_an_embedded_quote() {
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
  }

  #[test]
  fn copy_via_target_invokes_nix_copy_closure_with_use_substitutes() {
    let target = TargetAddr {
      name:     "web1".to_owned(),
      ssh_host: "deploy@web1.example".to_owned(),
      ssh_opts: Vec::new(),
    };
    let args = nix_copy_closure_args(&target, "/nix/store/abc-system");
    assert_eq!(
      args,
      vec![
        "--to".to_owned(),
        "deploy@web1.example".to_owned(),
        "--use-substitutes".to_owned(),
        "/nix/store/abc-system".to_owned(),
      ],
      "must match the xi-sources-push spec's exact invocation"
    );
  }

  fn secret_spec(group: Option<&str>) -> SecretSpec {
    SecretSpec {
      name:        "ssh-key".to_owned(),
      source:      std::path::PathBuf::from("./ssh-key"),
      destination: std::path::PathBuf::from("/run/keys/ssh"),
      owner:       "root".to_owned(),
      group:       group.map(str::to_owned),
      mode:        "0600".to_owned(),
      upload_at:   xi_core::sources::secrets::UploadAt::PreActivate,
    }
  }

  #[test]
  fn the_install_script_carries_the_configured_mode_and_owner() {
    let spec = secret_spec(None);
    let script = install_secret_script(&spec, "/tmp/.staged");
    assert!(script.contains("-m '0600'"), "got: {script}");
    assert!(script.contains("-o 'root'"), "got: {script}");
    assert!(script.contains("install"), "got: {script}");
  }

  #[test]
  fn the_install_script_omits_the_group_flag_when_none_is_configured() {
    let spec = secret_spec(None);
    let script = install_secret_script(&spec, "/tmp/.staged");
    assert!(!script.contains("-g "), "got: {script}");
  }

  #[test]
  fn the_install_script_passes_the_configured_group() {
    let spec = secret_spec(Some("wheel"));
    let script = install_secret_script(&spec, "/tmp/.staged");
    assert!(script.contains("-g 'wheel'"), "got: {script}");
  }

  #[test]
  fn a_custom_mode_is_honoured_verbatim() {
    let mut spec = secret_spec(None);
    spec.mode = "0440".to_owned();
    let script = install_secret_script(&spec, "/tmp/.staged");
    assert!(script.contains("-m '0440'"), "got: {script}");
    assert!(!script.contains("-m '0600'"), "got: {script}");
  }

  #[test]
  fn the_install_script_stages_then_installs_then_removes_the_staged_copy() {
    let spec = secret_spec(None);
    let script = install_secret_script(&spec, "/tmp/.staged-secret");
    let staged_pos = script.find("/tmp/.staged-secret").expect("staged path");
    let install_pos = script.find("install").expect("install call");
    let rm_pos = script.rfind("rm -f").expect("cleanup");
    assert!(
      install_pos < rm_pos,
      "install must run before the staged copy is removed: {script}"
    );
    assert!(
      staged_pos < rm_pos,
      "the staged path must appear before cleanup: {script}"
    );
  }

  #[test]
  fn the_install_script_creates_the_destination_parent_directory() {
    let spec = secret_spec(None);
    let script = install_secret_script(&spec, "/tmp/.staged");
    assert!(script.contains("mkdir -p '/run/keys'"), "got: {script}");
  }

  #[test]
  fn batch_mode_is_off_only_when_interactive() {
    let target = TargetAddr {
      name:     "a".to_owned(),
      ssh_host: "a".to_owned(),
      ssh_opts: Vec::new(),
    };
    assert!(
      SshTransport::base_opts(&target, false)
        .contains(&"BatchMode=yes".to_owned())
    );
    assert!(
      SshTransport::base_opts(&target, true)
        .contains(&"BatchMode=no".to_owned())
    );
  }
}
