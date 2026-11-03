//! Shadow-write for the sources dir: fetch beside it, then rename.
//!
//! Part of the `xi-sources-crash-recovery` capability
//! (`openspec/specs/xi-sources-crash-recovery/spec.md`, requirement
//! "Shadow-write for the sources dir").
//!
//! The sources dir is what the machine deploys from, so it must never be
//! observed half-updated. A `git fetch` directly into it fails that:
//! killed partway through, it leaves a clone whose working tree and
//! index disagree, and the next `xi switch` builds from whatever that is.
//!
//! So a fetch lands in `<sources_dir>.incoming` instead. Nothing reads
//! that path, so a crash at any point before the rename is harmless:
//! the sources dir is untouched and the leftover is inert.
//!
//! Making the new clone visible is then three renames
//! ([`promote_dir`]), because `rename(2)` refuses to replace a non-empty
//! directory:
//!
//! 1. `<sources_dir>` to `<sources_dir>.previous`
//! 2. `<sources_dir>.incoming` to `<sources_dir>`
//! 3. remove `<sources_dir>.previous`
//!
//! Only step 3 is non-atomic, and by then readers already see the new
//! sources dir. A crash between 1 and 2 is the one window where the
//! sources dir is absent, and it is exactly what the `.previous`
//! leftover tells [`super::repair`] to undo.

use std::{
  ffi::OsString,
  fs,
  io,
  path::{Path, PathBuf},
  process::Command,
};

use tracing::debug;

/// Suffix of the directory a fetch is staged into.
pub const INCOMING_SUFFIX: &str = ".incoming";

/// Suffix of the directory the previous sources dir is moved aside to.
pub const PREVIOUS_SUFFIX: &str = ".previous";

/// `<sources_dir>.incoming`, a sibling of the sources dir.
///
/// A sibling rather than a child so the final step is a plain rename
/// within one directory, which is the only form `rename(2)` guarantees
/// to be atomic.
#[must_use]
pub fn incoming_path(sources_dir: &Path) -> PathBuf {
  with_suffix(sources_dir, INCOMING_SUFFIX)
}

/// `<sources_dir>.previous`, where the outgoing clone is parked.
#[must_use]
pub fn previous_path(sources_dir: &Path) -> PathBuf {
  with_suffix(sources_dir, PREVIOUS_SUFFIX)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
  let mut name = OsString::from(path.as_os_str());
  name.push(suffix);
  PathBuf::from(name)
}

/// Make `from` visible at `to`, preserving the old `to` until it is.
///
/// # Errors
///
/// Propagates the rename failures. A failure at step 1 leaves both
/// paths where they were; a failure at step 2 leaves `to` parked at its
/// `.previous` path, which [`super::repair`] restores.
pub fn promote_dir(from: &Path, to: &Path) -> io::Result<()> {
  let previous = previous_path(to);

  let parked = if to.exists() {
    // A leftover `.previous` from an earlier crash would make this
    // rename fail, and keeping it is pointless: it is older than the
    // sources dir we are about to park, which is itself about to be
    // discarded.
    remove_if_present(&previous)?;
    fs::rename(to, &previous)?;
    true
  } else {
    false
  };

  fs::rename(from, to)?;

  if parked {
    remove_if_present(&previous)?;
  }
  Ok(())
}

/// Remove a path if it is there, whatever kind of path it is.
///
/// # Errors
///
/// Propagates every I/O error except `NotFound`, which is the expected
/// outcome and not a problem.
pub fn remove_if_present(path: &Path) -> io::Result<()> {
  let meta = match fs::symlink_metadata(path) {
    Ok(meta) => meta,
    Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
    Err(err) => return Err(err),
  };
  if meta.is_dir() {
    fs::remove_dir_all(path)
  } else {
    fs::remove_file(path)
  }
}

/// A fetch staged beside the sources dir.
///
/// Built by [`ShadowFetch::begin`], populated by
/// [`ShadowFetch::fetch`], and made visible by registering it with
/// [`super::transaction::Transaction::promote`] so the rename happens at
/// commit. Dropping it does *not* clean up: the transaction owns that,
/// and repair owns whatever the transaction could not reach.
#[derive(Debug)]
pub struct ShadowFetch {
  sources_dir: PathBuf,
  incoming:    PathBuf,
}

impl ShadowFetch {
  /// Create an empty `<sources_dir>.incoming`, owner-only.
  ///
  /// Any leftover from an earlier crashed fetch is removed first: it
  /// cannot be resumed (we do not know how far it got) and leaving it
  /// would make this fetch start from a mystery state.
  ///
  /// # Errors
  ///
  /// Propagates the removal and directory-creation failures.
  pub fn begin(sources_dir: &Path) -> io::Result<Self> {
    let incoming = incoming_path(sources_dir);
    remove_if_present(&incoming)?;
    fs::create_dir_all(&incoming)?;
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(&incoming, fs::Permissions::from_mode(0o700))?;
    }
    Ok(Self {
      sources_dir: sources_dir.to_path_buf(),
      incoming,
    })
  }

  /// The staging path, for registering with a transaction.
  #[must_use]
  pub fn incoming(&self) -> &Path {
    &self.incoming
  }

  /// Where this fetch will end up once promoted.
  #[must_use]
  pub fn target(&self) -> &Path {
    &self.sources_dir
  }

  /// Populate the shadow with `branch` from `remote`.
  ///
  /// When a sources dir already exists, the shadow is seeded from it
  /// with `git clone --local`, which hardlinks the object store instead
  /// of re-downloading it. Hardlinks are what make that safe here:
  /// after the promote deletes the old directory entry, the objects are
  /// still referenced by the new clone. The alternative, `--shared`,
  /// would leave the new clone pointing at an `objects/info/alternates`
  /// path we are about to delete.
  ///
  /// Every git invocation names `--git-dir` and `--work-tree`
  /// explicitly rather than relying on discovery from the cwd, so no
  /// step can accidentally operate on the live sources dir.
  ///
  /// # Errors
  ///
  /// Returns the git failure, with the missing-branch case rewritten to
  /// name where the branch came from  -  that is far and away the most
  /// common adoption failure, since xi tracks `main` by default and
  /// plenty of repos are still on `master`.
  pub fn fetch(&self, remote: &str, branch: &str) -> io::Result<()> {
    if self.sources_dir.join(".git").is_dir() {
      debug!(
        "seeding {} from the existing clone",
        self.incoming.display()
      );
      // `--local` needs a non-existent or empty target; `begin` made it
      // empty, and git tolerates that.
      git_bare(&[
        "clone",
        "--quiet",
        "--local",
        "--no-checkout",
        "--",
        &self.sources_dir.display().to_string(),
        &self.incoming.display().to_string(),
      ])?;
      self.git(&["remote", "set-url", "origin", remote])?;
    } else {
      git_bare(&[
        "init",
        "--quiet",
        "--",
        &self.incoming.display().to_string(),
      ])?;
      self.git(&["remote", "add", "origin", remote])?;
    }

    self.git(&["fetch", "--quiet", "--no-tags", "origin", branch])?;
    self.git(&["checkout", "--quiet", "--force", "-B", branch, "FETCH_HEAD"])
  }

  /// The sha the shadow's `HEAD` resolves to.
  ///
  /// # Errors
  ///
  /// Propagates the `git rev-parse` failure.
  pub fn head_sha(&self) -> io::Result<String> {
    head_sha(&self.incoming)
  }

  /// Run git against the shadow's git dir and work tree.
  fn git(&self, args: &[&str]) -> io::Result<()> {
    let out = Command::new("git")
      .env("GIT_TERMINAL_PROMPT", "0")
      .arg("--git-dir")
      .arg(self.incoming.join(".git"))
      .arg("--work-tree")
      .arg(&self.incoming)
      .args(args)
      .output()?;
    check(args, &out)
  }
}

/// Run git with no repository context, for `clone` and `init`.
///
/// Free rather than a method: with no `--git-dir` to supply, there is
/// nothing on the shadow for it to read.
fn git_bare(args: &[&str]) -> io::Result<()> {
  let out = Command::new("git")
    .env("GIT_TERMINAL_PROMPT", "0")
    .args(args)
    .output()?;
  check(args, &out)
}

fn check(args: &[&str], out: &std::process::Output) -> io::Result<()> {
  if out.status.success() {
    return Ok(());
  }
  let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
  Err(io::Error::other(describe_git_failure(args, &stderr)))
}

/// Turn a git failure into a message that names the fix where we can.
///
/// Only one case is worth special-casing: a branch the remote does not
/// have. Git's own wording never says where the branch name came from,
/// and xi supplies a default (`main`), so the bare git error sends the
/// operator looking in the wrong place.
fn describe_git_failure(args: &[&str], stderr: &str) -> String {
  let missing_branch = stderr.contains("couldn't find remote ref")
    || (stderr.contains("Remote branch") && stderr.contains("not found"));
  if missing_branch {
    let branch = args.last().copied().unwrap_or("the requested branch");
    return format!(
      "the remote has no branch `{branch}`.\n\n{stderr}\n\nxi tracks `{}` \
       unless told otherwise. Pick the branch with `--branch <name>`, \
       `XI_SOURCES_BRANCH`, or `[sources] branch` in .xi.toml.",
      super::config::DEFAULT_BRANCH
    );
  }
  format!("git {} failed: {stderr}", args.join(" "))
}

/// The sha `HEAD` resolves to in the git repository at `repo`.
///
/// Returns `NotFound` when `repo` is not a repository or has no commit
/// yet, so callers can treat "no head" as a legitimate state.
///
/// # Errors
///
/// Propagates the failure to spawn git, and reports a non-zero exit as
/// `NotFound`.
pub fn head_sha(repo: &Path) -> io::Result<String> {
  let out = Command::new("git")
    .arg("--git-dir")
    .arg(repo.join(".git"))
    .arg("--work-tree")
    .arg(repo)
    .args(["rev-parse", "HEAD"])
    .output()?;
  if !out.status.success() {
    return Err(io::Error::new(
      io::ErrorKind::NotFound,
      format!(
        "{}: no resolvable HEAD: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr).trim()
      ),
    ));
  }
  Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn shadow_paths_are_siblings_of_the_sources_dir() {
    let dir = Path::new("/var/lib/xi/sources");
    assert_eq!(
      incoming_path(dir),
      PathBuf::from("/var/lib/xi/sources.incoming")
    );
    assert_eq!(
      previous_path(dir),
      PathBuf::from("/var/lib/xi/sources.previous")
    );
    assert_eq!(incoming_path(dir).parent(), dir.parent());
  }

  #[test]
  fn promote_replaces_a_non_empty_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("sources");
    let shadow = incoming_path(&target);

    fs::create_dir_all(&target).expect("target");
    fs::write(target.join("old"), b"old").expect("old file");
    fs::create_dir_all(&shadow).expect("shadow");
    fs::write(shadow.join("new"), b"new").expect("new file");

    promote_dir(&shadow, &target).expect("promote");

    assert!(target.join("new").exists());
    assert!(!target.join("old").exists());
    assert!(!shadow.exists());
    assert!(!previous_path(&target).exists());
  }

  #[test]
  fn promote_onto_a_missing_target_is_a_plain_rename() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("sources");
    let shadow = incoming_path(&target);
    fs::create_dir_all(&shadow).expect("shadow");

    promote_dir(&shadow, &target).expect("promote");
    assert!(target.is_dir());
    assert!(!previous_path(&target).exists());
  }

  #[test]
  fn promote_discards_a_stale_previous_first() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("sources");
    let shadow = incoming_path(&target);
    let previous = previous_path(&target);

    fs::create_dir_all(&target).expect("target");
    fs::create_dir_all(&shadow).expect("shadow");
    fs::create_dir_all(&previous).expect("stale previous");
    fs::write(previous.join("stale"), b"x").expect("stale file");

    promote_dir(&shadow, &target).expect("promote");
    assert!(!previous.exists());
  }
}
