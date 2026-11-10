use std::{
  env,
  fs,
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
#[must_use]
pub fn state_base() -> PathBuf {
  xi_state_root().join("develop")
}

/// Xi's state root — one level above `state_base()`.
/// `$XDG_STATE_HOME/xi/`
///
/// Hook sidecars (`hook-queue/`, `hook-cache/`, `hook.log`) live here beside
/// the per-flake `develop/<fid>` subtree.
pub fn xi_state_root() -> PathBuf {
  let base = env::var("XDG_STATE_HOME")
    .map_or_else(|_| home_dir().join(".local/state"), PathBuf::from);
  base.join("xi")
}

/// Trust directory.
/// `$XDG_CONFIG_HOME/xi/develop/trusted/`
pub fn trust_dir() -> PathBuf {
  let base = env::var("XDG_CONFIG_HOME")
    .map_or_else(|_| home_dir().join(".config"), PathBuf::from);
  base.join("xi/develop/trusted")
}

/// Environment variable that overrides the runtime directory.
///
/// Set this to relocate the daemon socket and pid file (for example into a
/// test's temporary directory). An empty value is treated as unset.
pub const RUNTIME_DIR_VAR: &str = "XI_RUNTIME_DIR";

/// Maximum usable length of a `sockaddr_un` path, in bytes.
///
/// `sun_path` is 108 bytes on Linux and 104 on macOS/BSD; one byte is
/// reserved for the NUL terminator. We use the smaller of the two so a path
/// that passes this check on Linux is also bindable on macOS.
const SUN_PATH_MAX: usize = 103;

/// Runtime directory.
///
/// Honours an explicit [`RUNTIME_DIR_VAR`] override before any platform
/// default, so tests can place the daemon socket and pid file inside a
/// temporary directory instead of the process-global `/tmp/xi-{uid}`.
/// Falls back to `/tmp/xi-{uid}` on unix and `$XDG_RUNTIME_DIR/xi`
/// elsewhere.
#[must_use]
pub fn runtime_dir() -> PathBuf {
  runtime_dir_from(env::var(RUNTIME_DIR_VAR).ok().as_deref())
}

/// [`runtime_dir`] with the override supplied explicitly.
///
/// Split out so tests can exercise every branch without mutating the process
/// environment, which is both `unsafe` in edition 2024 and racy under the
/// multithreaded test harness. `Some("")` is treated as unset.
#[must_use]
pub fn runtime_dir_from(override_value: Option<&str>) -> PathBuf {
  if let Some(explicit) = override_value
    && !explicit.is_empty()
  {
    return PathBuf::from(explicit);
  }
  platform_runtime_dir()
}

/// The platform default runtime directory, ignoring any override.
#[must_use]
pub fn platform_runtime_dir() -> PathBuf {
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

/// Validate that a unix socket path fits in `sockaddr_un::sun_path`.
///
/// `UnixListener::bind` fails with an opaque `invalid argument` when the
/// path is too long, which is especially confusing under a
/// [`RUNTIME_DIR_VAR`] override pointing at a deep temporary directory.
/// This names the actual length and the limit instead.
///
/// # Errors
///
/// Returns an error if the path exceeds the platform `sun_path` limit.
pub fn check_socket_path_len(socket_path: &Path) -> color_eyre::Result<()> {
  let len = socket_path.as_os_str().as_encoded_bytes().len();
  if len > SUN_PATH_MAX {
    color_eyre::eyre::bail!(
      "daemon socket path is {len} bytes, which exceeds the {SUN_PATH_MAX} \
       byte limit for a unix socket: {}\nSet a shorter {RUNTIME_DIR_VAR}.",
      socket_path.display()
    );
  }
  Ok(())
}

/// Per-flake daemon runtime directory.
/// `{runtime_dir}/{flake_id}/`, i.e. `/tmp/xi-{uid}/{flake_id}/` unless
/// overridden (see [`runtime_dir`]).
#[must_use]
pub fn daemon_runtime_dir(flake_id: &str) -> PathBuf {
  runtime_dir().join(flake_id)
}

/// Daemon socket path.
/// `{runtime_dir}/{flake_id}/daemon.sock` (see [`runtime_dir`]).
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

/// Whether this target's env and hook output files actually exist on
/// disk for this process's state dir.
///
/// A daemon's "nothing changed, skip the write" fast paths (eval-cache
/// hit, content-hash dedup) are only safe to take if these already
/// exist  -  otherwise a consumer gets told `should_source_env = true`
/// pointing at a `.current.sh` symlink that was never created, and
/// `source` fails on every single prompt tick.
#[must_use]
pub fn env_hook_files_exist(state_dir: &Path, target: &str) -> bool {
  current_link(state_dir, &format!("env-{target}"), "sh").exists()
    && current_link(state_dir, &format!("hook-{target}"), "sh").exists()
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

/// Ensure the env/hook generation counters exist, creating them at
/// generation 1 if missing.
///
/// A counter file that was never created reads back as generation 0  -
/// indistinguishable from a brand-new consumer's own default last-seen
/// generation  -  so a code path that skips the normal bump-on-change (e.g.
/// a warm-started daemon that never runs a real eval) must call this
/// first, or `should_source_env`/`should_source_hook` stay false for
/// every consumer forever, even ones that have never actually sourced
/// anything.
pub fn ensure_generation_files_exist(state_dir: &Path) {
  ensure_generation_file_exists(state_dir, "env-generation");
  ensure_generation_file_exists(state_dir, "hook-generation");
}

fn ensure_generation_file_exists(state_dir: &Path, filename: &str) {
  let gen_path = state_dir.join(filename);
  if !gen_path.exists() {
    let _ = std::fs::create_dir_all(state_dir);
    let _ = std::fs::write(&gen_path, "1");
  }
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
    assert_eq!(hex_encode(&[0xAB, 0xCD, 0x12]), "abcd12");
  }
}
