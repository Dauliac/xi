//! `--mode sources`: rsync the operator's sources dir to the target and
//! let it rebuild from its own copy, instead of shipping a built
//! closure.
//!
//! This is the `xi-sources-push` spec's "`--mode sources`" requirement:
//! rsync the sources dir to the target, excluding `.git/` and
//! operator-specific state ([`sync`], through [`Transport::sync_dir`]),
//! then SSH in and run the target's own `xi <scope> switch` entrypoint
//! against the synced copy ([`switch`], through [`Transport::run`]) -
//! the same entrypoint `xi-deploy-unify-activation` gives `--mode
//! closure`'s [`crate::push::orchestrate::activation_script`], so both
//! modes take the target's `SourcesLock` and leave the same state-dir
//! markers no matter which one triggered the activation.

use std::path::Path;

use color_eyre::{Result, eyre::bail};
use xi_core::sources::Scope;

use super::{
  discover::PushTarget,
  engine::{RemoteCommand, TargetAddr, Transport},
};

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

/// The remote activation invocation `--mode sources` runs after
/// [`sync`] lands the sources dir: a remote `xi <scope> switch <remote
/// path>#<target name>` invocation, tagged with
/// `XI_APPLIED_ORIGIN=push:<origin_host>` so `record_activation_at` can
/// tell this activation apart from a local interactive switch or a
/// gitops-daemon tick, e.g.
/// `XI_APPLIED_ORIGIN=push:operator-host xi system switch
/// /etc/xi/sources#web1`.
///
/// The synced sources dir is a flake like any other, so this costs `xi
/// <scope> switch` nothing it was not already going to do: the
/// positional `<remote path>#<target name>` resolves as the same
/// `reference#attribute` installable shape an interactive `xi <scope>
/// switch /etc/xi/sources#web1` would parse. `Scope::sources_verb`
/// picks `system` over naming `nixos`/`darwin` explicitly, which is why
/// this needs no per-kind branch the way `target.rebuild_tool`
/// (`nixos-rebuild`/`darwin-rebuild`/`home-manager`) used to.
///
/// Exposed separately from [`switch`] so the exact script is assertable
/// without a [`Transport`].
#[must_use]
pub fn switch_script(
  target: &PushTarget,
  scope: Scope,
  origin_host: &str,
) -> String {
  format!(
    "XI_APPLIED_ORIGIN=push:{origin_host} xi {} switch {}#{}",
    scope.sources_verb(),
    remote_path(scope),
    target.addr.name
  )
}

/// Wrap `command` in a transient `systemd-run` unit, per the
/// `xi-sources-push` proposal's "Remote execution wrapped in a
/// transient systemd unit": the remote-build path runs its command this
/// way instead of as a bare foreground SSH command.
///
/// The three flags each earn their place:
/// - `--wait` blocks until the unit finishes and propagates its exit code, so
///   [`switch`] still sees a real success/failure.
/// - `--pipe` connects the unit's stdio to this invocation's, so the rebuild's
///   own output still reaches the operator instead of only journald.
/// - `--collect` unloads the unit (success or failure) once it exits, so
///   `--mode sources` never leaves a transient unit file behind.
///
/// Together these survive an SSH hiccup (the unit keeps running on the
/// target even if the SSH session drops), get journald logging on the
/// target for free, and still propagate the real exit code - all three
/// things a bare foreground SSH command cannot offer.
///
/// `command` is inserted after `--`, not shell-quoted: every caller
/// today is [`switch_script`]'s output, a single invocation with no
/// shell metacharacters, and the surrounding shell (this whole string
/// still runs through the remote login shell, the same as every other
/// [`RemoteCommand::script`]) splits it into the argv `systemd-run`
/// wants on the far side of `--`.
#[must_use]
pub fn wrap_in_systemd_run(command: &str) -> String {
  format!("systemd-run --wait --pipe --collect -- {command}")
}

/// Run the remote rebuild `--mode sources` needs after [`sync`] has
/// landed the sources dir on `target`, wrapped in a transient
/// `systemd-run` unit ([`wrap_in_systemd_run`]).
///
/// # Errors
///
/// Propagates a [`Transport::run`] error (the local `ssh` could not be
/// spawned), or reports a non-zero remote exit naming `target` and the
/// remote's own error output.
pub fn switch(
  transport: &dyn Transport,
  target: &PushTarget,
  scope: Scope,
  origin_host: &str,
  interactive_sudo: bool,
) -> Result<()> {
  let script =
    wrap_in_systemd_run(&switch_script(target, scope, origin_host));
  let outcome = transport.run(
    &target.addr,
    &RemoteCommand::elevated(script, interactive_sudo),
  )?;
  if !outcome.success {
    bail!(
      "remote rebuild on {} failed: {}",
      target.addr.name,
      outcome.message()
    );
  }
  Ok(())
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::unreachable,
  reason = "Fine in tests; the unreachable! calls are unimplemented Transport \
            methods this module's tests never exercise"
)]
mod tests {
  use std::sync::Mutex;

  use color_eyre::eyre::bail;

  use super::*;
  use crate::push::{
    args::CopyVia,
    discover::TargetSource,
    engine::{Elevate, RemoteCommand},
  };

  /// Records the args of every `sync_dir` and `run` call and answers
  /// each from its own scripted outcome list, one entry per call
  /// (falling back to success once a script is exhausted).
  struct RecordingTransport {
    outcomes:     Mutex<Vec<bool>>,
    calls:        Mutex<Vec<SyncCall>>,
    run_outcomes: Mutex<Vec<bool>>,
    run_calls:    Mutex<Vec<RemoteCommand>>,
  }

  #[derive(Debug, Clone, PartialEq, Eq)]
  struct SyncCall {
    local:            String,
    remote:           String,
    excludes:         Vec<String>,
    interactive_sudo: bool,
  }

  impl RecordingTransport {
    fn new(outcomes: Vec<bool>) -> Self {
      Self {
        outcomes:     Mutex::new(outcomes),
        calls:        Mutex::new(Vec::new()),
        run_outcomes: Mutex::new(Vec::new()),
        run_calls:    Mutex::new(Vec::new()),
      }
    }

    fn with_run_outcomes(run_outcomes: Vec<bool>) -> Self {
      Self {
        outcomes:     Mutex::new(Vec::new()),
        calls:        Mutex::new(Vec::new()),
        run_outcomes: Mutex::new(run_outcomes),
        run_calls:    Mutex::new(Vec::new()),
      }
    }

    fn calls(&self) -> Vec<SyncCall> {
      self.calls.lock().expect("calls lock").clone()
    }

    fn run_calls(&self) -> Vec<RemoteCommand> {
      self.run_calls.lock().expect("run_calls lock").clone()
    }
  }

  impl Transport for RecordingTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      cmd: &RemoteCommand,
    ) -> Result<crate::push::engine::Outcome> {
      self
        .run_calls
        .lock()
        .expect("run_calls lock")
        .push(cmd.clone());
      let mut outcomes = self.run_outcomes.lock().expect("run_outcomes lock");
      let success = if outcomes.is_empty() {
        true
      } else {
        outcomes.remove(0)
      };
      Ok(if success {
        crate::push::engine::Outcome::ok("")
      } else {
        crate::push::engine::Outcome::failed("switch-to-configuration failed")
      })
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
      let success = if outcomes.is_empty() {
        true
      } else {
        outcomes.remove(0)
      };
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

  fn push_target(name: &str, rebuild_tool: &'static str) -> PushTarget {
    PushTarget {
      addr: TargetAddr {
        name:     name.to_owned(),
        ssh_host: name.to_owned(),
        ssh_opts: Vec::new(),
      },
      source: TargetSource::NixosConfigurations,
      tags: Vec::new(),
      installable: String::new(),
      kind: crate::target::ProfileKind::NixOS,
      profile_path: String::new(),
      rebuild_tool,
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
    assert_eq!(calls[0].excludes, vec![
      ".git/",
      ".direnv/",
      "result*",
      ".envrc.local"
    ]);
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

  #[test]
  fn switch_script_runs_the_remote_switch_entrypoint_against_the_synced_path()
   {
    let target = push_target("web1", "nixos-rebuild");
    assert_eq!(
      switch_script(&target, Scope::System, "operator-host"),
      "XI_APPLIED_ORIGIN=push:operator-host xi system switch \
       /etc/xi/sources#web1"
    );
  }

  #[test]
  fn switch_script_does_not_special_case_the_targets_rebuild_tool() {
    let target = push_target("mac1", "darwin-rebuild");
    assert_eq!(
      switch_script(&target, Scope::System, "operator-host"),
      "XI_APPLIED_ORIGIN=push:operator-host xi system switch \
       /etc/xi/sources#mac1"
    );
  }

  #[test]
  fn switch_script_resolves_the_user_scope_remote_path_and_verb() {
    let target = push_target("me", "home-manager");
    assert_eq!(
      switch_script(&target, Scope::User, "operator-host"),
      "XI_APPLIED_ORIGIN=push:operator-host xi home switch \
       ~/.local/share/xi/sources#me"
    );
  }

  // -- wrap_in_systemd_run: flags and command placement ------------------

  #[test]
  fn the_wrapped_command_carries_every_required_flag_in_order() {
    let wrapped =
      wrap_in_systemd_run("nixos-rebuild switch --flake /etc/xi/sources#web1");
    assert_eq!(
      wrapped,
      "systemd-run --wait --pipe --collect -- nixos-rebuild switch --flake \
       /etc/xi/sources#web1"
    );
  }

  #[test]
  fn the_wait_flag_is_present_so_the_exit_code_propagates() {
    assert!(wrap_in_systemd_run("true").contains("--wait"));
  }

  #[test]
  fn the_pipe_flag_is_present_so_output_reaches_the_operator() {
    assert!(wrap_in_systemd_run("true").contains("--pipe"));
  }

  #[test]
  fn the_collect_flag_is_present_so_no_unit_file_survives() {
    assert!(wrap_in_systemd_run("true").contains("--collect"));
  }

  #[test]
  fn the_command_is_appended_verbatim_after_the_double_dash_separator() {
    let wrapped = wrap_in_systemd_run("echo hello");
    let (flags, command) =
      wrapped.split_once(" -- ").expect("has a -- separator");
    assert_eq!(flags, "systemd-run --wait --pipe --collect");
    assert_eq!(command, "echo hello");
  }

  #[test]
  fn wrapping_the_real_switch_script_produces_the_full_remote_command() {
    let target = push_target("web1", "nixos-rebuild");
    let wrapped = wrap_in_systemd_run(&switch_script(
      &target,
      Scope::System,
      "operator-host",
    ));
    assert_eq!(
      wrapped,
      "systemd-run --wait --pipe --collect -- XI_APPLIED_ORIGIN=push:operator-\
       host xi system switch /etc/xi/sources#web1"
    );
  }

  #[test]
  fn a_successful_switch_runs_the_systemd_wrapped_script_elevated() {
    let transport = RecordingTransport::with_run_outcomes(vec![true]);
    let target = push_target("web1", "nixos-rebuild");
    switch(&transport, &target, Scope::System, "operator-host", false)
      .expect("switches");

    let calls = transport.run_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
      calls[0].script,
      wrap_in_systemd_run(&switch_script(
        &target,
        Scope::System,
        "operator-host"
      ))
    );
    assert_eq!(calls[0].elevate, Elevate::NonInteractive);
  }

  #[test]
  fn interactive_sudo_propagates_to_the_remote_switch() {
    let transport = RecordingTransport::with_run_outcomes(vec![true]);
    let target = push_target("web1", "nixos-rebuild");
    switch(&transport, &target, Scope::System, "operator-host", true)
      .expect("switches");

    let calls = transport.run_calls();
    assert_eq!(calls[0].elevate, Elevate::Interactive);
    assert!(calls[0].pty);
  }

  #[test]
  fn the_switch_script_carries_the_applied_origin_tag() {
    let transport = RecordingTransport::with_run_outcomes(vec![true]);
    let target = push_target("web1", "nixos-rebuild");
    switch(&transport, &target, Scope::System, "operator-host", false)
      .expect("switches");

    let calls = transport.run_calls();
    assert!(
      calls[0].script.contains("XI_APPLIED_ORIGIN=push:operator-host"),
      "got: {}",
      calls[0].script
    );
  }

  #[test]
  fn a_failed_remote_switch_names_the_target_and_the_remote_error() {
    let transport = RecordingTransport::with_run_outcomes(vec![false]);
    let target = push_target("web1", "nixos-rebuild");
    let err = switch(&transport, &target, Scope::System, "operator-host", false)
      .expect_err("remote switch failed");
    let message = format!("{err}");
    assert!(message.contains("web1"), "got: {message}");
    assert!(
      message.contains("switch-to-configuration failed"),
      "got: {message}"
    );
  }
}
