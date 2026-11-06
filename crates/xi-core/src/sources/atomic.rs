//! Crash-safe file replacement: the one write primitive the state dir uses.
//!
//! Part of the `xi-sources-crash-recovery` capability
//! (`openspec/specs/xi-sources-crash-recovery/spec.md`, requirement
//! "Atomic state-dir metadata writes").
//!
//! A plain `fs::write` truncates the target and then streams bytes into
//! it, so a reader that opens the file in between sees a half-written
//! value and a crash in between leaves one on disk permanently. Both
//! matter here: the state dir is read by `xi sources status` and by the
//! gitops daemon while a switch is writing it, and the whole point of
//! this capability is that a SIGKILL must never leave a lie behind.
//!
//! [`write_atomic`] closes both holes with the standard four-step dance:
//! write to a sibling temp file, `fsync` it, `rename` over the target,
//! `fsync` the parent directory. `rename(2)` within one directory is
//! atomic, so a reader sees either the old whole file or the new whole
//! file, and the final directory `fsync` is what makes the rename itself
//! survive power loss rather than merely a process kill.

use std::{
  fs,
  io,
  path::{Path, PathBuf},
};

/// Infix that marks a half-written file this module owns.
///
/// Named here rather than inlined at the format site because
/// [`super::repair`] has to recognise the leftovers of a crashed write
/// in order to sweep them.
pub const TEMP_INFIX: &str = ".tmp.";

/// The temp path [`write_atomic`] would use for `path` in this process.
///
/// The pid in the name is what keeps two concurrent writers from
/// scribbling over each other's half-written file. It is also what lets
/// repair tell a leftover apart from a write that is still running.
#[must_use]
pub fn temp_path_for(path: &Path) -> PathBuf {
  let mut name = path.file_name().unwrap_or_default().to_os_string();
  name.push(format!("{TEMP_INFIX}{}", std::process::id()));
  path.with_file_name(name)
}

/// True when `name` is a leftover temp file of some [`write_atomic`] call.
#[must_use]
pub fn is_temp_name(name: &str) -> bool {
  temp_owner_pid(name).is_some()
}

/// The pid encoded in a temp file name, if `name` is one.
#[must_use]
pub fn temp_owner_pid(name: &str) -> Option<u32> {
  name
    .rsplit_once(TEMP_INFIX)
    .filter(|(stem, _)| !stem.is_empty())
    .and_then(|(_, pid)| pid.parse().ok())
}

/// Replace `path` with `contents` atomically, or leave it untouched.
///
/// `mode` is applied to the temp file *before* the rename on unix, so
/// the target is never briefly world-readable: the file arrives at its
/// final name already hardened. Pass `None` to inherit the umask.
///
/// # Errors
///
/// Propagates every I/O error from the create, write, sync and rename.
/// On any failure the temp file is removed and `path` still holds its
/// previous content, so a failed write is never a partial write.
pub fn write_atomic(
  path: &Path,
  contents: &[u8],
  mode: Option<u32>,
) -> io::Result<()> {
  let parent = path.parent().ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("{} has no parent directory", path.display()),
    )
  })?;

  let temp = temp_path_for(path);
  // A single helper so every early exit removes the temp file. Without
  // this, an error between create and rename would leave exactly the
  // litter this module exists to prevent.
  let result = write_temp_then_rename(&temp, path, contents, mode);
  if result.is_err() {
    drop(fs::remove_file(&temp));
  }
  result?;

  // Durability of the *rename*, not of the bytes: without this the
  // directory entry can be lost on power failure even though the file
  // contents were synced.
  sync_dir(parent)
}

fn write_temp_then_rename(
  temp: &Path,
  path: &Path,
  contents: &[u8],
  mode: Option<u32>,
) -> io::Result<()> {
  use std::io::Write as _;

  let mut file = fs::File::create(temp)?;
  #[cfg(unix)]
  if let Some(mode) = mode {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
  }
  #[cfg(not(unix))]
  let _ = mode;

  file.write_all(contents)?;
  file.sync_all()?;
  drop(file);

  fs::rename(temp, path)
}

/// `fsync` a directory, tolerating filesystems that refuse to.
///
/// Opening a directory read-only and syncing it is the portable way to
/// make a rename durable, but some filesystems (and every non-unix
/// target) return an error instead. Treating that as a failure would
/// turn a successful write into a reported error, so an unsupported
/// sync is swallowed: the rename already happened and is already
/// atomic, only its durability across power loss is weaker.
fn sync_dir(dir: &Path) -> io::Result<()> {
  match fs::File::open(dir) {
    Ok(handle) => {
      match handle.sync_all() {
        Ok(()) => Ok(()),
        Err(err)
          if matches!(
            err.kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
          ) =>
        {
          Ok(())
        },
        Err(err) => Err(err),
      }
    },
    Err(err) if err.kind() == io::ErrorKind::PermissionDenied => Ok(()),
    Err(err) => Err(err),
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn temp_name_carries_this_pid() {
    let temp = temp_path_for(Path::new("/state/sources-head"));
    let name = temp
      .file_name()
      .and_then(|n| n.to_str())
      .expect("temp name is utf-8");
    assert!(is_temp_name(name), "{name} should look like a temp file");
    assert_eq!(temp_owner_pid(name), Some(std::process::id()));
  }

  #[test]
  fn sanctioned_names_are_not_mistaken_for_temps() {
    for name in ["sources-head", "transaction.json", "remote-url"] {
      assert!(!is_temp_name(name), "{name} is not a temp file");
    }
  }

  #[test]
  fn write_replaces_whole_content() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("sources-head");
    write_atomic(&path, b"old", None).expect("first write");
    write_atomic(&path, b"new-and-longer", None).expect("second write");
    assert_eq!(
      fs::read_to_string(&path).expect("read back"),
      "new-and-longer"
    );
  }

  #[test]
  fn failed_write_leaves_no_temp_behind() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // A directory where the target name is itself a directory: the
    // rename cannot succeed, which is the cheapest reliable failure.
    let path = tmp.path().join("occupied");
    fs::create_dir(&path).expect("occupy the name");
    drop(fs::create_dir(path.join("child")));
    fs::write(path.join("child/keep"), b"x").expect("non-empty dir");

    assert!(write_atomic(&path, b"payload", None).is_err());
    let leftovers: Vec<String> = fs::read_dir(tmp.path())
      .expect("list")
      .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
      .filter(|n| is_temp_name(n))
      .collect();
    assert!(leftovers.is_empty(), "left temp files: {leftovers:?}");
  }
}
