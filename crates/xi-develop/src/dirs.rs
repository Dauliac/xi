use std::{
  env, fs,
  path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

/// Cache version — bump when on-disk format changes.
/// Triggers automatic cache invalidation on upgrade.
pub const CACHE_VERSION: u32 = 1;

const VERSION_FILE: &str = "VERSION";

/// Compute a flake ID from a canonical path.
/// Returns the first 16 hex chars of `sha256(canonical_path)`.
#[must_use]
pub fn flake_id(canonical_path: &Path) -> String {
  let mut hasher = Sha256::new();
  hasher.update(canonical_path.as_os_str().as_encoded_bytes());
  let hash = hasher.finalize();
  hex_encode(&hash[..8])
}

/// Compute flake ID from a flake reference (canonicalizes the path first).
///
/// # Errors
///
/// Returns an error if the path cannot be canonicalized.
pub fn flake_id_from_ref(flake_ref: &str) -> color_eyre::Result<String> {
  let path = fs::canonicalize(flake_ref).map_err(|e| {
    color_eyre::eyre::eyre!("Cannot resolve flake path '{}': {}", flake_ref, e)
  })?;
  Ok(flake_id(&path))
}

/// Per-flake state directory.
/// `$XDG_STATE_HOME/xi/develop/{flake_id}/`
#[must_use]
pub fn state_dir(fid: &str) -> PathBuf {
  state_base().join(fid)
}

/// Nix profile path for GC root protection.
/// `{state_dir}/profile-{target}`
///
/// The `--profile` flag on `nix print-dev-env` creates a profile symlink at
/// this path and automatically registers it as a GC root, preventing
/// `nix-collect-garbage` from deleting the devshell closure.
#[must_use]
pub fn profile_path(state_dir: &Path, target: &str) -> PathBuf {
  state_dir.join(format!("profile-{target}"))
}

/// Base state directory.
/// `$XDG_STATE_HOME/xi/develop/`
pub fn state_base() -> PathBuf {
  let base = env::var("XDG_STATE_HOME")
    .map_or_else(|_| home_dir().join(".local/state"), PathBuf::from);
  base.join("xi/develop")
}

/// Trust directory.
/// `$XDG_CONFIG_HOME/xi/develop/trusted/`
pub fn trust_dir() -> PathBuf {
  let base = env::var("XDG_CONFIG_HOME")
    .map_or_else(|_| home_dir().join(".config"), PathBuf::from);
  base.join("xi/develop/trusted")
}

/// Runtime directory.
/// `/tmp/xi-{uid}/`
#[must_use]
pub fn runtime_dir() -> PathBuf {
  #[cfg(unix)]
  {
    let uid = nix_uid();
    PathBuf::from(format!("/tmp/xi-{uid}"))
  }
  #[cfg(not(unix))]
  {
    let base = env::var("XDG_RUNTIME_DIR")
      .map(PathBuf::from)
      .unwrap_or_else(|_| env::temp_dir());
    base.join("xi")
  }
}

/// Per-flake daemon runtime directory.
/// `/tmp/xi-{uid}/{flake_id}/`
#[must_use]
pub fn daemon_runtime_dir(flake_id: &str) -> PathBuf {
  runtime_dir().join(flake_id)
}

/// Daemon socket path.
/// `/tmp/xi-{uid}/{flake_id}/daemon.sock`
#[must_use]
pub fn daemon_socket_path(flake_id: &str) -> PathBuf {
  daemon_runtime_dir(flake_id).join("daemon.sock")
}

// ── A/B slot file helpers ──

/// The two slots for atomic file switching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
  A,
  B,
}

impl Slot {
  #[must_use]
  pub const fn other(self) -> Self {
    match self {
      Self::A => Self::B,
      Self::B => Self::A,
    }
  }

  #[must_use]
  pub const fn suffix(self) -> &'static str {
    match self {
      Self::A => "a",
      Self::B => "b",
    }
  }
}

/// Get the path for a slot file: `{state_dir}/{prefix}.{slot}.{ext}`
#[must_use]
pub fn slot_path(
  state_dir: &Path,
  prefix: &str,
  slot: Slot,
  ext: &str,
) -> PathBuf {
  state_dir.join(format!("{prefix}.{}.{ext}", slot.suffix()))
}

/// Get the current symlink path: `{state_dir}/{prefix}.current.{ext}`
#[must_use]
pub fn current_link(state_dir: &Path, prefix: &str, ext: &str) -> PathBuf {
  state_dir.join(format!("{prefix}.current.{ext}"))
}

/// Read which slot a current symlink points to.
#[must_use]
pub fn active_slot(state_dir: &Path, prefix: &str, ext: &str) -> Option<Slot> {
  let link = current_link(state_dir, prefix, ext);
  let target = std::fs::read_link(&link).ok()?;
  let name = target.file_name()?.to_str()?;
  if name.contains(".a.") {
    Some(Slot::A)
  } else if name.contains(".b.") {
    Some(Slot::B)
  } else {
    None
  }
}

/// Write content to the inactive slot and atomically switch the symlink.
///
/// # Errors
/// Returns an error if file I/O fails.
pub fn write_and_switch(
  state_dir: &Path,
  prefix: &str,
  ext: &str,
  content: &str,
) -> color_eyre::Result<()> {
  std::fs::create_dir_all(state_dir)?;

  // Write to the inactive slot
  let active = active_slot(state_dir, prefix, ext).unwrap_or(Slot::A);
  let target_slot = active.other();
  let target_path = slot_path(state_dir, prefix, target_slot, ext);

  // Atomic write to slot file
  let tmp_path = target_path.with_extension("tmp");
  {
    let mut file = std::fs::File::create(&tmp_path)?;
    std::io::Write::write_all(&mut file, content.as_bytes())?;
    file.sync_all()?;
  }
  std::fs::rename(&tmp_path, &target_path)?;

  // Atomically repoint symlink
  let link = current_link(state_dir, prefix, ext);
  let tmp_link = link.with_extension("tmp-link");
  let _ = std::fs::remove_file(&tmp_link);
  #[cfg(unix)]
  std::os::unix::fs::symlink(&target_path, &tmp_link)?;
  #[cfg(not(unix))]
  std::fs::copy(&target_path, &tmp_link)?;
  std::fs::rename(&tmp_link, &link)?;

  Ok(())
}

/// Bump env-generation counter (signals shell to re-source env file).
pub fn bump_env_generation(state_dir: &Path) {
  bump_generation_file(state_dir, "env-generation");
}

/// Bump hook-generation counter (signals shell to re-source hook file).
pub fn bump_hook_generation(state_dir: &Path) {
  bump_generation_file(state_dir, "hook-generation");
}

fn bump_generation_file(state_dir: &Path, filename: &str) {
  let gen_path = state_dir.join(filename);
  let current: u64 = std::fs::read_to_string(&gen_path)
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0);
  let _ = std::fs::write(&gen_path, (current + 1).to_string());
}

/// Ensure the cache version matches. Nukes state on mismatch.
///
/// # Errors
///
/// Returns an error if directory operations fail.
pub fn ensure_cache_version(dir: &Path) -> color_eyre::Result<()> {
  let version_file = dir.join(VERSION_FILE);

  if let Ok(content) = fs::read_to_string(&version_file) {
    let stored: u32 = content.trim().parse().unwrap_or(0);
    if stored == CACHE_VERSION {
      return Ok(());
    }
    tracing::info!(
      "Cache version changed ({stored} → {CACHE_VERSION}), rebuilding"
    );
    nuke_dir_contents(dir)?;
  }

  fs::create_dir_all(dir)?;
  fs::write(&version_file, CACHE_VERSION.to_string())?;
  Ok(())
}

fn nuke_dir_contents(dir: &Path) -> color_eyre::Result<()> {
  if !dir.exists() {
    return Ok(());
  }
  for entry in fs::read_dir(dir)? {
    let entry = entry?;
    let name = entry.file_name();
    if name == VERSION_FILE {
      continue;
    }
    if entry.file_type()?.is_dir() {
      fs::remove_dir_all(entry.path())?;
    } else {
      fs::remove_file(entry.path())?;
    }
  }
  Ok(())
}

fn home_dir() -> PathBuf {
  env::var("HOME").map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
}

#[cfg(unix)]
fn nix_uid() -> u32 {
  nix::unistd::getuid().as_raw()
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
  let mut s = String::with_capacity(bytes.len() * 2);
  for b in bytes {
    use std::fmt::Write;
    let _ = write!(s, "{b:02x}");
  }
  s
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn flake_id_is_deterministic() {
    let id1 = flake_id(Path::new("/home/user/project"));
    let id2 = flake_id(Path::new("/home/user/project"));
    assert_eq!(id1, id2);
    assert_eq!(id1.len(), 16);
  }

  #[test]
  fn flake_id_differs_for_different_paths() {
    let id1 = flake_id(Path::new("/home/user/project-a"));
    let id2 = flake_id(Path::new("/home/user/project-b"));
    assert_ne!(id1, id2);
  }

  #[test]
  fn hex_encode_works() {
    assert_eq!(hex_encode(&[0xab, 0xcd, 0x12]), "abcd12");
  }
}
