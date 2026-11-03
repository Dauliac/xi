//! The named Nix profile the daemon registers generations on.
//!
//! Every successful build (`xi-sources-gitops` section 6) is meant to land
//! as a generation on a profile dedicated to the daemon, so
//! `nix-env --list-generations` and `nix-collect-garbage` see it the same
//! way they see any other profile. This module owns the two path spellings
//! and the `nix-env` invocation that registers a generation on them; it
//! does not decide *when* to register one, because that is the daemon's
//! build step ([[xi-sources-gitops]] section 6.2), still to be written.
//!
//! The leaf name, `xi-sources`, is fixed so an operator who has read the
//! proposal can find the profile without reading code: `/nix/var/nix/
//! profiles/system-profiles/xi-sources` at system scope, `${XDG_STATE_HOME}/
//! nix/profiles/xi-sources` at user scope.

use std::path::{Path, PathBuf};

use xi_core::sources::{Scope, config::xdg_state_home};

use crate::exec::{Exec, Invocation};

/// Leaf name of the daemon's named profile, at either scope.
pub const PROFILE_NAME: &str = "xi-sources";

/// Where the named profile lives for `scope`.
///
/// System scope is a fixed path, matching every other `/nix/var/nix/
/// profiles/system-profiles/<name>` profile on the machine. User scope
/// follows XDG: `${XDG_STATE_HOME}/nix/profiles/xi-sources`, which is the
/// same base dir the sources state dir uses, without being inside it  -
/// the state dir holds xi's own bookkeeping, and a Nix profile is a
/// different kind of thing even when it happens to sit alongside it.
#[must_use]
pub fn profile_path(scope: Scope) -> PathBuf {
  profile_path_with(scope, &|key| std::env::var(key).ok())
}

/// [`profile_path`] against an arbitrary environment, for tests.
#[must_use]
pub fn profile_path_with(
  scope: Scope,
  lookup: &dyn Fn(&str) -> Option<String>,
) -> PathBuf {
  match scope {
    Scope::System => {
      PathBuf::from("/nix/var/nix/profiles/system-profiles").join(PROFILE_NAME)
    },
    Scope::User => {
      xdg_state_home(lookup)
        .join("nix/profiles")
        .join(PROFILE_NAME)
    },
  }
}

/// `nix-env` failed to register a generation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("nix-env --profile {} --set {store_path} failed: {detail}", profile.display())]
pub struct RegisterError {
  /// The profile `nix-env` was asked to update.
  pub profile:    PathBuf,
  /// The store path that could not be registered.
  pub store_path: String,
  /// What `nix-env` said.
  pub detail:     String,
}

/// Register `store_path` as the newest generation of the named profile.
///
/// Runs `nix-env --profile <path> --set <store_path>`, which is how Nix
/// itself adds a generation to any profile: the same primitive
/// `nix-env --list-generations` and `nix-collect-garbage` already
/// understand. Routed through [`Exec`] so a test can assert the exact
/// argv without actually writing under `/nix/var/nix/profiles`.
///
/// # Errors
///
/// Returns [`RegisterError`] when the invocation fails to spawn or
/// exits non-zero.
pub fn register_generation(
  exec: &dyn Exec,
  profile: &Path,
  store_path: &str,
) -> Result<(), RegisterError> {
  let invocation = Invocation::new("nix-env", [
    "--profile",
    &profile.to_string_lossy(),
    "--set",
    store_path,
  ]);

  let outcome = exec.run(&invocation).map_err(|source| {
    RegisterError {
      profile:    profile.to_path_buf(),
      store_path: store_path.to_owned(),
      detail:     source.to_string(),
    }
  })?;

  if outcome.ok() {
    Ok(())
  } else {
    Err(RegisterError {
      profile:    profile.to_path_buf(),
      store_path: store_path.to_owned(),
      detail:     outcome.failure_text(),
    })
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;
  use crate::exec::recorder::RecordingExec;

  fn lookup(home: &str) -> impl Fn(&str) -> Option<String> + '_ {
    move |key| {
      if key == "XDG_STATE_HOME" {
        None
      } else if key == "HOME" {
        Some(home.to_owned())
      } else {
        None
      }
    }
  }

  #[test]
  fn system_scope_uses_the_fixed_system_profiles_path() {
    let path = profile_path_with(Scope::System, &lookup("/home/op"));
    assert_eq!(
      path,
      PathBuf::from("/nix/var/nix/profiles/system-profiles/xi-sources")
    );
  }

  #[test]
  fn user_scope_follows_xdg_state_home() {
    let path = profile_path_with(Scope::User, &|key| {
      if key == "XDG_STATE_HOME" {
        Some("/home/op/.local/state".to_owned())
      } else {
        None
      }
    });
    assert_eq!(
      path,
      PathBuf::from("/home/op/.local/state/nix/profiles/xi-sources")
    );
  }

  #[test]
  fn user_scope_falls_back_to_home_relative_xdg_default() {
    let path = profile_path_with(Scope::User, &lookup("/home/op"));
    assert_eq!(
      path,
      PathBuf::from("/home/op/.local/state/nix/profiles/xi-sources")
    );
  }

  #[test]
  fn register_generation_runs_the_exact_nix_env_invocation() {
    let exec = RecordingExec::always_ok();
    let profile =
      PathBuf::from("/home/op/.local/state/nix/profiles/xi-sources");
    register_generation(&exec, &profile, "/nix/store/abc123-config")
      .expect("registration should succeed");

    assert_eq!(exec.argv_log(), vec![format!(
      "nix-env --profile {} --set /nix/store/abc123-config",
      profile.display()
    )]);
  }

  #[test]
  fn register_generation_surfaces_nix_env_s_failure_text() {
    let exec = RecordingExec::always_ok().with_reply(
      1,
      "",
      "error: build of '/nix/store/abc123-config' failed",
    );
    let profile =
      PathBuf::from("/home/op/.local/state/nix/profiles/xi-sources");
    let err = register_generation(&exec, &profile, "/nix/store/abc123-config")
      .expect_err("a non-zero nix-env exit must be an error");
    assert!(err.to_string().contains("build of"), "got: {err}");
  }
}
