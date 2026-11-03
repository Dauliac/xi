//! The one git-touching step every tick starts with: `git fetch`.
//!
//! Deliberately a subprocess, not `libgit2`: the proposal is explicit
//! that users expect `~/.gitconfig` (credential helpers, proxy settings,
//! `insteadOf` rewrites) to be honoured, and `libgit2` does not read all
//! of it. A real `git` binary does, by construction.
//!
//! Not behind the [`crate::exec::Exec`] seam, for the reason
//! [`crate::signing`] gives: the fast-forward and force-push scenarios
//! this module exists to support need a real git fixture to be honest
//! tests, and mocking `git fetch` would only assert this module's own
//! idea of what git does.

use std::{
  path::Path,
  process::{Command, Output},
};

use xi_core::sources::Sha;

/// `git fetch` (or the checkout after it) failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("git {args} failed in {}: {stderr}", repo.display())]
pub struct FetchError {
  /// The sources dir the command ran in.
  pub repo:   std::path::PathBuf,
  /// The git subcommand, space-joined, for the error text.
  pub args:   String,
  /// Git's stderr, trimmed.
  pub stderr: String,
}

/// Fetch `branch` from `remote_url` into `sources_dir`'s object database,
/// and return the sha it resolved to.
///
/// This does not move the sources dir's `HEAD` - it only makes the new
/// objects available and names them, via `FETCH_HEAD`, so the caller can
/// decide (via [`xi_core::sources::conflict::relate`]) whether advancing
/// to it is a fast-forward before touching the working tree.
///
/// # Errors
///
/// Returns [`FetchError`] when the fetch itself fails (network, auth,
/// unknown branch) or the subsequent `rev-parse FETCH_HEAD` does, which
/// only happens if the fetch reported success without actually updating
/// `FETCH_HEAD` - a git-internal inconsistency this surfaces rather than
/// panics on.
pub fn fetch_tip(
  sources_dir: &Path,
  remote_url: &str,
  branch: &str,
) -> Result<Sha, FetchError> {
  run(sources_dir, &["fetch", "--quiet", remote_url, branch])?;
  let output = run(sources_dir, &["rev-parse", "FETCH_HEAD"])?;
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Reset `sources_dir`'s working tree and `HEAD` to `sha`.
///
/// `--hard` is correct here specifically because the sources dir is a
/// daemon-owned clone no operator edits by hand ([[xi-sources-conflict]]
/// policy); the working dir, which operators do edit, is never touched
/// by this module.
///
/// # Errors
///
/// Returns [`FetchError`] when the reset fails, e.g. `sha` is not an
/// object `sources_dir` actually has (the caller is expected to have
/// fetched it first).
pub fn checkout(sources_dir: &Path, sha: &Sha) -> Result<(), FetchError> {
  run(sources_dir, &["reset", "--quiet", "--hard", sha])?;
  Ok(())
}

fn run(repo: &Path, args: &[&str]) -> Result<Output, FetchError> {
  let output = Command::new("git")
    .env("GIT_TERMINAL_PROMPT", "0")
    .arg("-C")
    .arg(repo)
    .args(args)
    .output()
    .map_err(|source| {
      FetchError {
        repo:   repo.to_path_buf(),
        args:   args.join(" "),
        stderr: source.to_string(),
      }
    })?;

  if output.status.success() {
    Ok(output)
  } else {
    Err(FetchError {
      repo:   repo.to_path_buf(),
      args:   args.join(" "),
      stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::fs;

  use tempfile::TempDir;

  use super::*;

  fn git(at: &Path, args: &[&str]) {
    let status = Command::new("git")
      .env("GIT_TERMINAL_PROMPT", "0")
      .arg("-C")
      .arg(at)
      .args(args)
      .status()
      .expect("git should run");
    assert!(status.success(), "git {args:?} failed");
  }

  fn head(at: &Path) -> Sha {
    let output = Command::new("git")
      .arg("-C")
      .arg(at)
      .args(["rev-parse", "HEAD"])
      .output()
      .expect("rev-parse");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
  }

  fn new_repo_with_commit(message: &str) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    git(tmp.path(), &["config", "user.email", "test@example.com"]);
    git(tmp.path(), &["config", "user.name", "Test"]);
    fs::write(tmp.path().join("file.txt"), message).expect("write");
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-q", "-m", message]);
    tmp
  }

  #[test]
  fn fetch_tip_resolves_to_the_remotes_current_head() {
    let remote = new_repo_with_commit("base");
    let sources_dir = TempDir::new().expect("tempdir");
    git(sources_dir.path(), &["init", "-q", "-b", "main"]);

    let sha = fetch_tip(
      sources_dir.path(),
      remote.path().to_str().expect("utf8 path"),
      "main",
    )
    .expect("fetch should succeed");
    assert_eq!(sha, head(remote.path()));
  }

  #[test]
  fn checkout_moves_the_sources_dir_to_the_fetched_sha() {
    let remote = new_repo_with_commit("base");
    let sources_dir = TempDir::new().expect("tempdir");
    git(sources_dir.path(), &["init", "-q", "-b", "main"]);

    let sha = fetch_tip(
      sources_dir.path(),
      remote.path().to_str().expect("utf8 path"),
      "main",
    )
    .expect("fetch should succeed");
    checkout(sources_dir.path(), &sha).expect("checkout should succeed");

    assert_eq!(head(sources_dir.path()), sha);
    assert_eq!(
      fs::read_to_string(sources_dir.path().join("file.txt")).expect("read"),
      "base"
    );
  }

  #[test]
  fn fetching_an_unknown_branch_is_an_error() {
    let remote = new_repo_with_commit("base");
    let sources_dir = TempDir::new().expect("tempdir");
    git(sources_dir.path(), &["init", "-q", "-b", "main"]);

    let err = fetch_tip(
      sources_dir.path(),
      remote.path().to_str().expect("utf8 path"),
      "does-not-exist",
    )
    .expect_err("an unknown branch must not fetch");
    assert!(err.to_string().contains("fetch"), "got: {err}");
  }
}
