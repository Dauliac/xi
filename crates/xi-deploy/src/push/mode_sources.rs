//! `--mode sources`: rsync the operator's sources dir to the target and
//! let it rebuild from its own copy, instead of shipping a built
//! closure.
//!
//! This is the `xi-sources-push` spec's "`--mode sources`" requirement:
//! rsync the sources dir to the target, excluding `.git/` and
//! operator-specific state, then SSH in and run the appropriate rebuild
//! tool with `--flake <remote path>#<host>`. This module only does the
//! transfer, through the existing [`Transport::sync_dir`]; the remote
//! rebuild invocation is a separate bead (4.2) layered on top.

use std::path::Path;

use color_eyre::Result;
use xi_core::sources::Scope;

use super::engine::{TargetAddr, Transport};

/// Files and directories the sync never ships to a target.
///
/// `.git/` is the spec's own example: the target rebuilds the working
/// tree, not the history behind it, and a full `.git/` is often the
/// single largest thing under the sources dir. The remaining two cover
/// operator-local build/dev artifacts that have no meaning on the
/// target's own filesystem: `.direnv/` caches a *local* devshell, and
/// `result*` is Nix's build-output symlink convention (`nix build`'s
/// default `-o result`), which usually dangles once copied anywhere
/// else and should never be confused for a tracked source.
pub const DEFAULT_EXCLUDES: &[&str] = &[".git/", ".direnv/", "result*"];

/// Where `--mode sources` lands the synced sources dir on the target.
///
/// The user-scope path is a literal `~/...`, not locally expanded:
/// rsync's remote end runs through the target's login shell, which is
/// what expands it there. Expanding it here would substitute the
/// *operator's* home directory instead of the target's.
#[must_use]
pub const fn remote_path(scope: Scope) -> &'static str {
  match scope {
    Scope::System => "/etc/xi/sources",
    Scope::User => "~/.local/share/xi/sources",
  }
}

/// Sync `local_sources_dir` to `target`'s [`remote_path`] for `scope`.
///
/// `extra_excludes` is appended after [`DEFAULT_EXCLUDES`], so a caller
/// (the `.xi.toml`-aware orchestrator, once it exists) can add a
/// repo-specific pattern without losing the baseline.
///
/// # Errors
///
/// Propagates a [`Transport::sync_dir`] error: a non-zero `rsync`, or a
/// local spawn failure.
pub fn sync(
  transport: &dyn Transport,
  target: &TargetAddr,
  local_sources_dir: &Path,
  scope: Scope,
  extra_excludes: &[&str],
  interactive_sudo: bool,
) -> Result<()> {
  let mut excludes: Vec<&str> = DEFAULT_EXCLUDES.to_vec();
  excludes.extend_from_slice(extra_excludes);
  transport.sync_dir(
    target,
    local_sources_dir,
    remote_path(scope),
    &excludes,
    interactive_sudo,
  )
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! calls are unimplemented \
            Transport methods this module's tests never exercise"
)]
mod tests {
  use std::sync::Mutex;

  use color_eyre::eyre::bail;

  use super::*;
  use crate::push::{args::CopyVia, engine::RemoteCommand};

  /// Records the args of every `sync_dir` call and answers with a
  /// scripted outcome, one per call (falling back to success once the
  /// script is exhausted).
  struct RecordingTransport {
    outcomes: Mutex<Vec<bool>>,
    calls:    Mutex<Vec<SyncCall>>,
  }

  #[derive(Debug, Clone, PartialEq, Eq)]
  struct SyncCall {
    local:             String,
    remote:            String,
    excludes:          Vec<String>,
    interactive_sudo:  bool,
  }

  impl RecordingTransport {
    fn new(outcomes: Vec<bool>) -> Self {
      Self {
        outcomes: Mutex::new(outcomes),
        calls:    Mutex::new(Vec::new()),
      }
    }

    fn calls(&self) -> Vec<SyncCall> {
      self.calls.lock().expect("calls lock").clone()
    }
  }

  impl Transport for RecordingTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      _cmd: &RemoteCommand,
    ) -> Result<crate::push::engine::Outcome> {
      unreachable!("not exercised by --mode sources tests")
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: CopyVia,
    ) -> Result<()> {
      unreachable!("not exercised by --mode sources tests")
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &Path,
      _spec: &xi_core::sources::secrets::SecretSpec,
      _interactive_sudo: bool,
    ) -> Result<()> {
      unreachable!("not exercised by --mode sources tests")
    }

    fn sync_dir(
      &self,
      _target: &TargetAddr,
      local: &Path,
      remote: &str,
      excludes: &[&str],
      interactive_sudo: bool,
    ) -> Result<()> {
      self.calls.lock().expect("calls lock").push(SyncCall {
        local: local.display().to_string(),
        remote: remote.to_owned(),
        excludes: excludes.iter().map(|s| (*s).to_owned()).collect(),
        interactive_sudo,
      });
      let mut outcomes = self.outcomes.lock().expect("outcomes lock");
      let success = if outcomes.is_empty() { true } else { outcomes.remove(0) };
      if success {
        Ok(())
      } else {
        bail!("rsync failed")
      }
    }
  }

  fn target() -> TargetAddr {
    TargetAddr {
      name:     "web1".to_owned(),
      ssh_host: "web1".to_owned(),
      ssh_opts: Vec::new(),
    }
  }

  #[test]
  fn system_scope_lands_under_etc_xi() {
    assert_eq!(remote_path(Scope::System), "/etc/xi/sources");
  }

  #[test]
  fn user_scope_lands_under_the_target_home() {
    assert_eq!(remote_path(Scope::User), "~/.local/share/xi/sources");
  }

  #[test]
  fn the_default_excludes_cover_git_and_operator_local_artifacts() {
    assert!(DEFAULT_EXCLUDES.contains(&".git/"));
    assert!(DEFAULT_EXCLUDES.contains(&".direnv/"));
    assert!(DEFAULT_EXCLUDES.contains(&"result*"));
  }

  #[test]
  fn a_sync_passes_the_scope_resolved_remote_path() {
    let transport = RecordingTransport::new(vec![true]);
    sync(
      &transport,
      &target(),
      Path::new("/srv/cfg"),
      Scope::System,
      &[],
      false,
    )
    .expect("syncs");
    let calls = transport.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].local, "/srv/cfg");
    assert_eq!(calls[0].remote, "/etc/xi/sources");
  }

  #[test]
  fn extra_excludes_are_appended_after_the_defaults() {
    let transport = RecordingTransport::new(vec![true]);
    sync(
      &transport,
      &target(),
      Path::new("/srv/cfg"),
      Scope::System,
      &[".envrc.local"],
      false,
    )
    .expect("syncs");
    let calls = transport.calls();
    assert_eq!(
      calls[0].excludes,
      vec![".git/", ".direnv/", "result*", ".envrc.local"]
    );
  }

  #[test]
  fn interactive_sudo_propagates_to_the_transport() {
    let transport = RecordingTransport::new(vec![true]);
    sync(
      &transport,
      &target(),
      Path::new("/srv/cfg"),
      Scope::User,
      &[],
      true,
    )
    .expect("syncs");
    assert!(transport.calls()[0].interactive_sudo);
  }

  #[test]
  fn a_failed_rsync_propagates_as_an_error() {
    let transport = RecordingTransport::new(vec![false]);
    let err = sync(
      &transport,
      &target(),
      Path::new("/srv/cfg"),
      Scope::System,
      &[],
      false,
    )
    .expect_err("fails");
    assert!(format!("{err}").contains("rsync failed"), "got: {err}");
  }
}
