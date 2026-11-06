//! `.xi-managed`  -  the file that says "xi owns this directory".
//!
//! The daemon's write paths are `git reset --hard` and friends, which
//! erase whatever is in their way. One mistyped `sources_dir` in a Nix
//! module is therefore enough to eat an operator's `$HOME`. krops solves
//! this with a `.populate` marker; the same trick applies here, with the
//! `sources_id` recorded inside so a sentinel left behind by a *different*
//! source cannot authorise a write either.
//!
//! Every write path calls [`assert_writable`] first. It is cheap (one
//! `read`), it runs on a path the daemon already has to stat, and it is
//! the only thing standing between a config typo and data loss.

use std::{
  fs,
  io,
  path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use xi_core::sources::SourcesId;

use crate::health::DegradedReason;

/// The sentinel's filename, at the root of the sources dir.
pub const SENTINEL_NAME: &str = ".xi-managed";

/// Schema version of the sentinel's JSON header.
pub const SENTINEL_VERSION: u32 = 1;

/// What wrote the sentinel. Fixed, so a future tool can be told apart.
pub const CREATED_BY: &str = "xi";

/// The JSON header stored in `.xi-managed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sentinel {
  /// Identity of the source this directory is the clone of.
  pub sources_id: SourcesId,
  /// What created it.
  pub created_by: String,
  /// Header schema version.
  pub version:    u32,
}

impl Sentinel {
  /// A sentinel for `sources_id`, at the current schema version.
  #[must_use]
  pub fn new(sources_id: SourcesId) -> Self {
    Self {
      sources_id,
      created_by: CREATED_BY.to_owned(),
      version: SENTINEL_VERSION,
    }
  }

  /// Where the sentinel lives for a given sources dir.
  #[must_use]
  pub fn path_in(sources_dir: &Path) -> PathBuf {
    sources_dir.join(SENTINEL_NAME)
  }

  /// Write the sentinel into `sources_dir`, creating the dir if needed.
  ///
  /// Called by the clone path, which is the only place a sources dir
  /// comes into existence.
  ///
  /// # Errors
  ///
  /// Propagates I/O and serialisation failures.
  pub fn write(&self, sources_dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(sources_dir)?;
    let path = Self::path_in(sources_dir);
    let body = serde_json::to_string_pretty(self)
      .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&path, format!("{body}\n"))?;
    Ok(path)
  }

  /// Read the sentinel out of `sources_dir`.
  ///
  /// # Errors
  ///
  /// Returns `NotFound` when there is no sentinel, and `InvalidData`
  /// when there is one but it is not a sentinel this version understands.
  pub fn read(sources_dir: &Path) -> io::Result<Self> {
    let raw = fs::read_to_string(Self::path_in(sources_dir))?;
    serde_json::from_str(&raw)
      .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
  }
}

/// Refuse to write into `sources_dir` unless xi owns it, for `sources_id`.
///
/// Three distinct refusals, all reported as `missing-sentinel` because
/// the operator's fix is the same in each case: look at the configured
/// path and at the file that is or is not there.
///
/// # Errors
///
/// Returns [`DegradedReason::MissingSentinel`] naming which of the three
/// checks failed.
pub fn assert_writable(
  sources_dir: &Path,
  sources_id: &SourcesId,
) -> Result<Sentinel, DegradedReason> {
  let refuse = |detail: String| DegradedReason::MissingSentinel { detail };

  let sentinel = Sentinel::read(sources_dir).map_err(|e| {
    if e.kind() == io::ErrorKind::NotFound {
      refuse(format!(
        "{} has no {SENTINEL_NAME}; refusing to write to a directory xi does \
         not own",
        sources_dir.display()
      ))
    } else {
      refuse(format!(
        "{}: {SENTINEL_NAME} is unreadable or malformed: {e}",
        sources_dir.display()
      ))
    }
  })?;

  if sentinel.sources_id != *sources_id {
    return Err(refuse(format!(
      "{} is the clone of {}, not of {sources_id}; refusing to write",
      sources_dir.display(),
      sentinel.sources_id
    )));
  }

  if sentinel.version > SENTINEL_VERSION {
    return Err(refuse(format!(
      "{} was written by a newer xi ({SENTINEL_NAME} version {}, this xi \
       understands {SENTINEL_VERSION}); refusing to write",
      sources_dir.display(),
      sentinel.version
    )));
  }

  Ok(sentinel)
}

/// Refuse a write target that is not inside the sources dir.
///
/// The `xi-sources-conflict` capability requires the daemon to assert
/// this on every write: the working dir, `$HOME` and the non-metadata
/// parts of the state dir are all out of bounds, and a bug that aims
/// there must abort the tick rather than succeed.
///
/// Both paths are canonicalised first, so `..` segments and symlinks
/// cannot be used to walk out of a prefix that textually looks fine.
///
/// # Errors
///
/// Returns [`DegradedReason::WriteOutsideSourcesDir`] naming the target.
pub fn assert_inside(
  sources_dir: &Path,
  target: &Path,
) -> Result<(), DegradedReason> {
  let refuse = || {
    DegradedReason::WriteOutsideSourcesDir {
      path: target.display().to_string(),
    }
  };

  // An unresolvable root is itself a refusal: without a canonical
  // sources dir there is nothing to compare against, and guessing is
  // what this function exists to prevent.
  let root = sources_dir.canonicalize().map_err(|_e| refuse())?;
  // The target may not exist yet (it is about to be written), so
  // canonicalise the nearest existing ancestor instead.
  let mut probe = target.to_path_buf();
  loop {
    if let Ok(resolved) = probe.canonicalize() {
      return if resolved.starts_with(&root) {
        Ok(())
      } else {
        Err(refuse())
      };
    }
    if !probe.pop() {
      return Err(refuse());
    }
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use tempfile::TempDir;
  use xi_core::sources::Scope;

  use super::*;

  fn id() -> SourcesId {
    SourcesId::new(Scope::System, "https://git.example/cfg")
  }

  fn other_id() -> SourcesId {
    SourcesId::new(Scope::System, "https://git.example/other")
  }

  #[test]
  fn sentinel_round_trips_through_its_json_header() {
    let tmp = TempDir::new().expect("tempdir");
    let written = Sentinel::new(id());
    written.write(tmp.path()).expect("write");

    let read = Sentinel::read(tmp.path()).expect("read");
    assert_eq!(read, written);
    assert_eq!(read.created_by, CREATED_BY);
    assert_eq!(read.version, SENTINEL_VERSION);
  }

  /// The header shape is a contract with operators who read the file,
  /// so the three documented keys are pinned.
  #[test]
  fn header_carries_the_three_documented_keys() {
    let tmp = TempDir::new().expect("tempdir");
    Sentinel::new(id()).write(tmp.path()).expect("write");

    let raw = fs::read_to_string(Sentinel::path_in(tmp.path())).expect("read");
    let json: serde_json::Value =
      serde_json::from_str(&raw).expect("valid JSON");
    assert!(json.get("sources_id").is_some(), "got: {raw}");
    assert_eq!(json["created_by"], "xi");
    assert_eq!(json["version"], SENTINEL_VERSION);
  }

  #[test]
  fn absent_sentinel_refuses_the_write() {
    let tmp = TempDir::new().expect("tempdir");
    let err = assert_writable(tmp.path(), &id())
      .expect_err("a directory xi does not own must not be writable");
    assert_eq!(err.token(), "missing-sentinel");
    assert!(err.to_string().contains(SENTINEL_NAME), "got: {err}");
  }

  #[test]
  fn sentinel_from_another_source_refuses_the_write() {
    let tmp = TempDir::new().expect("tempdir");
    Sentinel::new(other_id()).write(tmp.path()).expect("write");

    let err = assert_writable(tmp.path(), &id())
      .expect_err("a sentinel for another source must not authorise a write");
    assert_eq!(err.token(), "missing-sentinel");
  }

  #[test]
  fn malformed_sentinel_refuses_the_write() {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(Sentinel::path_in(tmp.path()), "not json").expect("write");

    let err = assert_writable(tmp.path(), &id())
      .expect_err("an unparsable sentinel must not authorise a write");
    assert_eq!(err.token(), "missing-sentinel");
  }

  #[test]
  fn a_newer_sentinel_version_refuses_the_write() {
    let tmp = TempDir::new().expect("tempdir");
    let mut sentinel = Sentinel::new(id());
    sentinel.version = SENTINEL_VERSION + 1;
    sentinel.write(tmp.path()).expect("write");

    let err = assert_writable(tmp.path(), &id())
      .expect_err("a format from the future must not be written through");
    assert!(err.to_string().contains("newer xi"), "got: {err}");
  }

  #[test]
  fn matching_sentinel_authorises_the_write() {
    let tmp = TempDir::new().expect("tempdir");
    Sentinel::new(id()).write(tmp.path()).expect("write");
    assert!(assert_writable(tmp.path(), &id()).is_ok());
  }

  #[test]
  fn a_path_inside_the_sources_dir_is_allowed() {
    let tmp = TempDir::new().expect("tempdir");
    let target = tmp.path().join("flake.nix");
    assert!(assert_inside(tmp.path(), &target).is_ok());
  }

  #[test]
  fn a_sibling_path_is_refused() {
    let tmp = TempDir::new().expect("tempdir");
    let sources = tmp.path().join("sources");
    let working = tmp.path().join("working");
    fs::create_dir_all(&sources).expect("mkdir");
    fs::create_dir_all(&working).expect("mkdir");

    let err = assert_inside(&sources, &working.join("flake.nix"))
      .expect_err("the working dir is out of bounds for the daemon");
    assert_eq!(err.token(), "write-outside-sources-dir");
  }

  /// A textual prefix check would pass `sources/../working`; the
  /// canonicalising one must not.
  #[test]
  fn a_dot_dot_escape_is_refused() {
    let tmp = TempDir::new().expect("tempdir");
    let sources = tmp.path().join("sources");
    fs::create_dir_all(&sources).expect("mkdir");
    fs::create_dir_all(tmp.path().join("working")).expect("mkdir");

    let escape = sources.join("..").join("working");
    let err = assert_inside(&sources, &escape)
      .expect_err("`..` must not walk out of the sources dir");
    assert_eq!(err.token(), "write-outside-sources-dir");
  }
}
