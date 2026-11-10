//! The xi-managed throwaway SSH keypair `modules/nixos/vm-guest.nix` bakes
//! into the `--dev` build's `authorized_keys`, and `xi vm connect` uses.
//!
//! One keypair per xi state dir, shared by every declared VM: there is no
//! per-VM identity to isolate, and a single pair keeps `vm-guest.nix`'s
//! fixed-path convention (`<stateDir>/ssh/id_ed25519.pub`) simple.

use std::path::{Path, PathBuf};

use color_eyre::{Result, eyre::bail};

/// The keypair's filenames under `<state_dir>/ssh/`.
const KEY_BASENAME: &str = "id_ed25519";

/// Private/public key paths for `state_dir`, regardless of whether they
/// exist yet.
#[must_use]
pub fn key_paths(state_dir: &Path) -> (PathBuf, PathBuf) {
  let dir = state_dir.join("ssh");
  (
    dir.join(KEY_BASENAME),
    dir.join(format!("{KEY_BASENAME}.pub")),
  )
}

/// Generate the keypair under `state_dir` if it does not already exist.
///
/// Idempotent: a second call against an existing keypair is a no-op, so
/// `xi vm build`/`start` can call this unconditionally before every
/// `--dev` build.
///
/// # Errors
///
/// Returns an error if the `ssh/` directory cannot be created or
/// `ssh-keygen` fails.
pub fn ensure_keypair(state_dir: &Path) -> Result<(PathBuf, PathBuf)> {
  let (private_key, public_key) = key_paths(state_dir);
  if public_key.exists() && private_key.exists() {
    return Ok((private_key, public_key));
  }

  let dir = private_key
    .parent()
    .ok_or_else(|| color_eyre::eyre::eyre!("key path has no parent directory"))?;
  std::fs::create_dir_all(dir).map_err(|e| {
    color_eyre::eyre::eyre!("failed to create {}: {e}", dir.display())
  })?;

  let status = std::process::Command::new("ssh-keygen")
    .arg("-t")
    .arg("ed25519")
    .arg("-N")
    .arg("")
    .arg("-C")
    .arg("xi-vm")
    .arg("-f")
    .arg(&private_key)
    .arg("-q")
    .status()
    .map_err(|e| color_eyre::eyre::eyre!("failed to run ssh-keygen: {e}"))?;

  if !status.success() {
    bail!("ssh-keygen exited with {status}");
  }

  Ok((private_key, public_key))
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn ensure_keypair_creates_both_files_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (private_key, public_key) =
      ensure_keypair(dir.path()).expect("first generation");
    assert!(private_key.exists());
    assert!(public_key.exists());

    let modified_before = std::fs::metadata(&public_key)
      .expect("metadata")
      .modified()
      .expect("mtime");

    let (private_key_again, public_key_again) =
      ensure_keypair(dir.path()).expect("second call is a no-op");
    assert_eq!(private_key_again, private_key);
    assert_eq!(public_key_again, public_key);

    let modified_after = std::fs::metadata(&public_key)
      .expect("metadata")
      .modified()
      .expect("mtime");
    assert_eq!(modified_before, modified_after, "must not regenerate");
  }

  #[test]
  fn key_paths_matches_the_nix_module_convention() {
    let (private_key, public_key) = key_paths(Path::new("/tmp/xi"));
    assert_eq!(private_key, Path::new("/tmp/xi/ssh/id_ed25519"));
    assert_eq!(public_key, Path::new("/tmp/xi/ssh/id_ed25519.pub"));
  }
}
