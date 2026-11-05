//! `cwd`-to-scope disambiguation for the unified `xi deploy` verb.
//!
//! Per `xi-deploy-unified-verb/spec.md`'s "`cwd` disambiguates scope, not
//! destination" requirement: on a host with more than one scope adopted,
//! `cwd` matching a scope's configured `working_dir` resolves `--scope`
//! when the flag itself was not given. This is a separate, earlier step
//! feeding scope resolution, not a branch inside the decision tree
//! itself (see [`crate::decision`]) - it has no opinion on whether the
//! matched scope's own tree then activates locally or rolls out to a
//! fleet, which stays entirely the decision tree's call.

use std::path::{Path, PathBuf};

use xi_core::sources::{Scope, args::resolve_working_dir};

/// One candidate scope's working dir, already resolved off its config
/// ladder and state dir.
///
/// A scope with no working dir configured - including one with nothing
/// adopted at all - simply never matches `cwd`; callers building this
/// list from live host state only need to include scopes that have one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeWorkingDir {
  /// The scope this working dir belongs to.
  pub scope:       Scope,
  /// Its configured working dir, assumed already canonicalized.
  pub working_dir: PathBuf,
}

/// Resolve which scope `cwd` picks out, from already-resolved facts.
///
/// Pure and filesystem-free, so it is unit-testable on its own: every
/// path here is assumed already canonicalized, making this a plain
/// equality search rather than a branch inside the decision tree.
/// Returns `None` when `cwd` matches none of `candidates`, leaving
/// `--scope` resolution to whatever the caller falls back to
/// (`SystemDeploymentKind::detect()` and recorded-source presence).
#[must_use]
pub fn scope_from_cwd(
  cwd: &Path,
  candidates: &[ScopeWorkingDir],
) -> Option<Scope> {
  candidates
    .iter()
    .find(|candidate| candidate.working_dir == cwd)
    .map(|candidate| candidate.scope)
}

/// [`scope_from_cwd`] against live host state: both scopes' configured
/// working dirs, canonicalized alongside `cwd` so a symlinked checkout or
/// a relative `cwd` still matches.
///
/// # Errors
///
/// Propagates config-load and state-dir read errors from
/// [`resolve_working_dir`].
pub fn disambiguate_scope(cwd: &Path) -> color_eyre::Result<Option<Scope>> {
  let cwd = canonicalize_or(cwd);
  let mut candidates = Vec::new();
  for scope in [Scope::System, Scope::User] {
    if let Some(working_dir) = resolve_working_dir(scope)? {
      candidates.push(ScopeWorkingDir {
        scope,
        working_dir: canonicalize_or(&working_dir),
      });
    }
  }
  Ok(scope_from_cwd(&cwd, &candidates))
}

/// Canonicalize, falling back to the original path when it cannot be
/// resolved (gone, unreadable). An unresolvable candidate is simply one
/// `scope_from_cwd` will not match, not a reason to fail scope
/// resolution outright.
fn canonicalize_or(path: &Path) -> PathBuf {
  path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn candidate(scope: Scope, dir: &str) -> ScopeWorkingDir {
    ScopeWorkingDir {
      scope,
      working_dir: PathBuf::from(dir),
    }
  }

  /// The dual-adoption fixture from the spec: a system scope with no
  /// working dir configured (only a sources dir, which this step never
  /// looks at) and a user scope with one at `/home/me/dotfiles`.
  fn dual_adoption() -> Vec<ScopeWorkingDir> {
    vec![candidate(Scope::User, "/home/me/dotfiles")]
  }

  #[test]
  fn cwd_matching_the_users_working_dir_resolves_user_scope() {
    assert_eq!(
      scope_from_cwd(Path::new("/home/me/dotfiles"), &dual_adoption()),
      Some(Scope::User)
    );
  }

  #[test]
  fn cwd_matching_neither_candidate_resolves_nothing() {
    assert_eq!(
      scope_from_cwd(Path::new("/tmp/unrelated"), &dual_adoption()),
      None
    );
  }

  #[test]
  fn an_empty_candidate_list_resolves_nothing() {
    assert_eq!(scope_from_cwd(Path::new("/home/me/dotfiles"), &[]), None);
  }

  #[test]
  fn two_configured_working_dirs_each_resolve_their_own_scope() {
    let candidates = vec![
      candidate(Scope::System, "/etc/xi/dotfiles-system"),
      candidate(Scope::User, "/home/me/dotfiles"),
    ];
    assert_eq!(
      scope_from_cwd(Path::new("/etc/xi/dotfiles-system"), &candidates),
      Some(Scope::System)
    );
    assert_eq!(
      scope_from_cwd(Path::new("/home/me/dotfiles"), &candidates),
      Some(Scope::User)
    );
  }
}
