//! Locate-and-run: comma-style execution via nix-index.
//!
//! Resolves a bare command name (e.g. `cowsay`) to a nixpkgs derivation
//! using `nix-locate`, builds it with nom progress, then executes the
//! binary directly from the store path.

use std::io::{BufRead, Write as _};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use color_eyre::eyre::bail;
use color_eyre::Result;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Cache level for locate results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CacheLevel {
  /// No caching — always re-query nix-locate and rebuild.
  Disabled,
  /// Cache the chosen derivation name (re-build each time).
  Choice,
  /// Cache both the derivation name and the resolved store path.
  #[default]
  Full,
}

impl CacheLevel {
  #[must_use]
  pub const fn from_u8(v: u8) -> Self {
    match v {
      0 => Self::Disabled,
      1 => Self::Choice,
      _ => Self::Full,
    }
  }
}


/// Simple JSON key-value cache stored in `~/.cache/xi/locate/`.
pub struct LocateCache {
  dir: PathBuf,
  level: CacheLevel,
}

/// Cached choice entry: command → derivation attribute.
#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct ChoicesMap(std::collections::HashMap<String, String>);

/// Cached path entry: derivation → store path.
#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct PathsMap(std::collections::HashMap<String, String>);

impl LocateCache {
  #[must_use]
  pub fn new(level: CacheLevel) -> Self {
    let dir = xi_core::dirs::xdg_cache_dir().join("locate");
    Self { dir, level }
  }

  /// Look up a cached store path for a command (level 2 hit).
  pub fn get_path(&self, command: &str) -> Option<PathBuf> {
    if self.level != CacheLevel::Full {
      return None;
    }
    let choices = self.load_choices();
    let derivation = choices.0.get(command)?;
    let paths = self.load_paths();
    paths.0.get(derivation).map(PathBuf::from)
  }

  /// Look up a cached derivation choice for a command (level 1 hit).
  pub fn get_choice(&self, command: &str) -> Option<String> {
    if self.level == CacheLevel::Disabled {
      return None;
    }
    self.load_choices().0.get(command).cloned()
  }

  /// Save a command → derivation choice.
  pub fn save_choice(&self, command: &str, derivation: &str) {
    if self.level == CacheLevel::Disabled {
      return;
    }
    let mut choices = self.load_choices();
    choices
      .0
      .insert(command.to_string(), derivation.to_string());
    self.write_json("choices.json", &choices);
  }

  /// Save a derivation → store path mapping.
  pub fn save_path(&self, derivation: &str, store_path: &Path) {
    if self.level != CacheLevel::Full {
      return;
    }
    let mut paths = self.load_paths();
    paths.0.insert(
      derivation.to_string(),
      store_path.to_string_lossy().to_string(),
    );
    self.write_json("paths.json", &paths);
  }

  fn load_choices(&self) -> ChoicesMap {
    self.read_json("choices.json").unwrap_or_default()
  }

  fn load_paths(&self) -> PathsMap {
    self.read_json("paths.json").unwrap_or_default()
  }

  fn read_json<T: serde::de::DeserializeOwned>(
    &self,
    filename: &str,
  ) -> Option<T> {
    let path = self.dir.join(filename);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
  }

  fn write_json<T: serde::Serialize>(&self, filename: &str, value: &T) {
    if let Err(e) = std::fs::create_dir_all(&self.dir) {
      debug!("[locate] failed to create cache dir: {e}");
      return;
    }
    let path = self.dir.join(filename);
    match serde_json::to_string_pretty(value) {
      Ok(json) => {
        if let Err(e) = std::fs::write(&path, json) {
          debug!("[locate] failed to write {}: {e}", path.display());
        }
      },
      Err(e) => debug!("[locate] failed to serialize cache: {e}"),
    }
  }
}

// ---------------------------------------------------------------------------
// nix-locate
// ---------------------------------------------------------------------------

/// Query `nix-locate` for packages providing `/bin/<command>`.
///
/// Returns a list of derivation attribute names (e.g. `cowsay`,
/// `neo-cowsay`).
pub fn nix_locate(command: &str) -> Result<Vec<String>> {
  let bin_pattern = format!("/bin/{command}");

  let output = Command::new("nix-locate")
    .args(["--minimal", "--at-root", "--whole-name", &bin_pattern])
    .stderr(Stdio::piped())
    .stdout(Stdio::piped())
    .output()
    .map_err(|e| {
      color_eyre::eyre::eyre!(
        "Failed to run nix-locate (is nix-index installed?): {e}\n\
         Install it with: nix profile install nixpkgs#nix-index\n\
         Then run: nix-index  (or use nix-index-database for pre-built indexes)"
      )
    })?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no database") || stderr.contains("No such file") {
      bail!(
        "nix-index database not found.\n\
         Run `nix-index` to build it, or use nix-index-database for \
         pre-built indexes:\n\
         https://github.com/nix-community/nix-index-database"
      );
    }
    bail!("nix-locate failed: {stderr}");
  }

  let stdout = String::from_utf8_lossy(&output.stdout);

  // Deduplicate: nix-locate can return the same attr multiple times
  // for different outputs of the same derivation.
  let mut seen = std::collections::HashSet::new();
  let results: Vec<String> = stdout
    .lines()
    .filter(|line| !line.is_empty())
    .map(|line| {
      // nix-locate output: "nixpkgs.cowsay" or "cowsay" — strip
      // leading "nixpkgs." if present
      line
        .strip_prefix("nixpkgs.")
        .unwrap_or(line)
        .to_string()
    })
    .filter(|r| seen.insert(r.clone()))
    .collect();

  Ok(results)
}

// ---------------------------------------------------------------------------
// Interactive picker
// ---------------------------------------------------------------------------

/// Pick one derivation from a list of candidates using an interactive picker.
///
/// Tries `fzy` first, then `fzf`, then falls back to a simple numbered menu.
pub fn pick(candidates: &[String], command: &str) -> Result<String> {
  if candidates.is_empty() {
    bail!(
      "command '{command}' not found in nix-index database.\n\
       Make sure your nix-index database is up to date."
    );
  }

  if candidates.len() == 1 {
    return Ok(candidates[0].clone());
  }

  // Try fzy, then fzf
  for picker_bin in &["fzy", "fzf"] {
    if which::which(picker_bin).is_ok()
      && let Ok(choice) = run_picker(picker_bin, candidates, command)
    {
      return Ok(choice);
    }
  }

  // Fallback: numbered menu on stderr
  pick_numbered_menu(candidates, command)
}

fn run_picker(
  picker: &str,
  candidates: &[String],
  command: &str,
) -> Result<String> {
  let prompt = format!("Pick package for '{command}': ");

  let mut args: Vec<&str> = Vec::new();
  if picker == "fzy" {
    args.extend(["--prompt", &prompt]);
  } else if picker == "fzf" {
    args.extend(["--prompt", &prompt, "--height", "~40%", "--reverse"]);
  }

  let mut child = Command::new(picker)
    .args(&args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .map_err(|e| color_eyre::eyre::eyre!("Failed to start {picker}: {e}"))?;

  if let Some(ref mut stdin) = child.stdin {
    for candidate in candidates {
      let _ = writeln!(stdin, "{candidate}");
    }
  }
  drop(child.stdin.take());

  let output = child.wait_with_output()?;
  if !output.status.success() {
    bail!("picker cancelled");
  }

  let choice = String::from_utf8_lossy(&output.stdout).trim().to_string();
  if choice.is_empty() {
    bail!("no package selected");
  }
  Ok(choice)
}

fn pick_numbered_menu(
  candidates: &[String],
  command: &str,
) -> Result<String> {
  eprintln!(
    "Multiple packages provide '{command}':"
  );
  for (i, candidate) in candidates.iter().enumerate() {
    eprintln!("  [{}] {}", i + 1, candidate);
  }
  eprint!("Pick [1-{}]: ", candidates.len());
  std::io::stderr().flush()?;

  let mut input = String::new();
  std::io::stdin().lock().read_line(&mut input)?;
  let input = input.trim();

  let index: usize = input
    .parse::<usize>()
    .map_err(|_| color_eyre::eyre::eyre!("invalid selection: {input}"))?;

  if index == 0 || index > candidates.len() {
    bail!("selection out of range: {index}");
  }

  Ok(candidates[index - 1].clone())
}

// ---------------------------------------------------------------------------
// Build + resolve store path
// ---------------------------------------------------------------------------

/// Build a nixpkgs derivation and return its store path.
///
/// Phase 1: build with nom for pretty progress.
/// Phase 2: query `--print-out-paths` (instant, already cached in store).
pub fn build_and_resolve_path(
  derivation: &str,
  passthrough_args: &[String],
  no_nom: bool,
) -> Result<PathBuf> {
  let installable = format!("nixpkgs#{derivation}");

  info!("Building {installable}");

  // Phase 1: build (with nom progress)
  let build_cmd = nix_command::NixCommand::new(nix_command::CommandKind::Build)
    .print_build_logs(false)
    .arg(&installable)
    .arg("--no-link")
    .args(passthrough_args);

  crate::execute_build(&build_cmd, no_nom, false)?;

  // Phase 2: resolve store path (instant — derivation is cached)
  let path_cmd = nix_command::NixCommand::new(nix_command::CommandKind::Build)
    .print_build_logs(false)
    .arg(&installable)
    .arg("--no-link")
    .arg("--print-out-paths");

  let output = path_cmd.output().map_err(|e| {
    color_eyre::eyre::eyre!("Failed to resolve store path: {e}")
  })?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("nix build --print-out-paths failed:\n{stderr}");
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  let store_path = stdout
    .lines()
    .next()
    .ok_or_else(|| color_eyre::eyre::eyre!("nix build produced no output"))?
    .trim();

  if store_path.is_empty() {
    bail!("nix build --print-out-paths returned empty output");
  }

  Ok(PathBuf::from(store_path))
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// Locate a command via nix-index and run it.
///
/// This is the main entry point for `xi run --locate`.
pub fn locate_and_run(
  command: &str,
  extra_args: &[String],
  passthrough_args: &[String],
  no_nom: bool,
  cache_level: CacheLevel,
  shell_mode: bool,
  install_mode: bool,
) -> Result<()> {
  use xi_core::style;

  let cache = LocateCache::new(cache_level);

  // --- Cache level 2: direct store path exec ---
  if let Some(cached_path) = cache.get_path(command) {
    let bin_path = cached_path.join("bin").join(command);
    if bin_path.exists() {
      debug!("[locate] cache hit (path): {}", cached_path.display());
      if shell_mode {
        return open_shell_from_cache(&cached_path, passthrough_args);
      }
      if install_mode {
        let derivation =
          cache.get_choice(command).unwrap_or_else(|| command.to_string());
        return install_package(&derivation);
      }
      return exec_from_store(&bin_path, command, extra_args);
    }
    // Cached path is stale (GC'd) — fall through to rebuild
    debug!("[locate] cached path is stale, rebuilding");
  }

  // --- Resolve derivation (cache level 1 or nix-locate) ---
  let derivation = if let Some(cached_choice) = cache.get_choice(command) {
    debug!("[locate] cache hit (choice): {cached_choice}");
    cached_choice
  } else {
    eprintln!(
      "{}",
      style::labeled_status(
        style::Icon::Loading,
        "locate",
        &format!("searching for '{command}'"),
      )
    );

    let candidates = nix_locate(command)?;

    if candidates.is_empty() {
      bail!(
        "command '{command}' not found in nix-index database.\n\
         Make sure your nix-index database is up to date."
      );
    }

    let chosen = pick(&candidates, command)?;
    cache.save_choice(command, &chosen);
    eprintln!(
      "{}",
      style::labeled_status(
        style::Icon::Success,
        "locate",
        &format!("found nixpkgs#{chosen}"),
      )
    );
    chosen
  };

  // --- Install mode: nix profile install ---
  if install_mode {
    return install_package(&derivation);
  }

  // --- Build + resolve store path ---
  let store_path =
    build_and_resolve_path(&derivation, passthrough_args, no_nom)?;
  cache.save_path(&derivation, &store_path);

  // --- Shell mode: open nix shell ---
  if shell_mode {
    let installable = format!("nixpkgs#{derivation}");
    info!("Opening shell with {installable}");
    let cmd =
      nix_command::NixCommand::new(nix_command::CommandKind::Shell)
        .arg(&installable)
        .args(passthrough_args);
    return crate::run_interactive(&cmd);
  }

  // --- Execute ---
  let bin_path = store_path.join("bin").join(command);
  if !bin_path.exists() {
    // The package might provide the binary under a different name.
    // Try to find it.
    if let Some(found) = find_binary_in_store(&store_path, command) {
      return exec_from_store(&found, command, extra_args);
    }
    bail!(
      "Package nixpkgs#{derivation} was built but /bin/{command} not \
       found in {}.\nThe package may provide a different binary name.",
      store_path.display()
    );
  }

  exec_from_store(&bin_path, command, extra_args)
}

/// Execute a binary directly from the nix store.
fn exec_from_store(
  bin_path: &Path,
  command: &str,
  extra_args: &[String],
) -> Result<()> {
  debug!(
    "[locate] exec {} {}",
    bin_path.display(),
    extra_args.join(" ")
  );

  // Use std::process::Command with exec() for direct replacement on Unix
  let err = Command::new(bin_path)
    .args(extra_args)
    .exec();

  // exec() only returns on error
  bail!("failed to exec '{command}': {err}");
}

/// Open a nix shell with a cached store path in `$PATH`.
fn open_shell_from_cache(
  store_path: &Path,
  passthrough_args: &[String],
) -> Result<()> {
  // Use nix shell with the store path directly
  let cmd = nix_command::NixCommand::new(nix_command::CommandKind::Shell)
    .arg(store_path.to_string_lossy().as_ref())
    .args(passthrough_args);
  crate::run_interactive(&cmd)
}

/// Install a package via `nix profile install`.
fn install_package(derivation: &str) -> Result<()> {
  let installable = format!("nixpkgs#{derivation}");
  info!("Installing {installable}");

  let status = Command::new(nix_command::find_real_nix_binary())
    .args(["profile", "install", &installable])
    .stdin(Stdio::inherit())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .status()
    .map_err(|e| {
      color_eyre::eyre::eyre!("Failed to run nix profile install: {e}")
    })?;

  if !status.success() {
    bail!("nix profile install exited with status {status}");
  }

  eprintln!(
    "{}",
    xi_core::style::labeled_status(
      xi_core::style::Icon::Success,
      "locate",
      &format!("installed nixpkgs#{derivation}"),
    )
  );

  Ok(())
}

/// Try to find a binary in a store path's bin directory.
fn find_binary_in_store(store_path: &Path, _command: &str) -> Option<PathBuf> {
  let bin_dir = store_path.join("bin");
  if !bin_dir.is_dir() {
    return None;
  }

  // Return the first executable found
  let entries = std::fs::read_dir(&bin_dir).ok()?;
  for entry in entries.flatten() {
    let path = entry.path();
    if path.is_file() {
      return Some(path);
    }
  }

  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cache_level_from_u8() {
    assert_eq!(CacheLevel::from_u8(0), CacheLevel::Disabled);
    assert_eq!(CacheLevel::from_u8(1), CacheLevel::Choice);
    assert_eq!(CacheLevel::from_u8(2), CacheLevel::Full);
    assert_eq!(CacheLevel::from_u8(42), CacheLevel::Full);
  }

  #[test]
  fn cache_level_default_is_full() {
    assert_eq!(CacheLevel::default(), CacheLevel::Full);
  }

  #[test]
  fn cache_roundtrip_in_tempdir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = LocateCache {
      dir: dir.path().to_path_buf(),
      level: CacheLevel::Full,
    };

    // Initially empty
    assert!(cache.get_choice("cowsay").is_none());
    assert!(cache.get_path("cowsay").is_none());

    // Save choice
    cache.save_choice("cowsay", "cowsay");
    assert_eq!(cache.get_choice("cowsay").as_deref(), Some("cowsay"));

    // Save path
    let fake_path = PathBuf::from("/nix/store/abc-cowsay-3.04");
    cache.save_path("cowsay", &fake_path);
    assert_eq!(cache.get_path("cowsay"), Some(fake_path));
  }

  #[test]
  fn cache_disabled_returns_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = LocateCache {
      dir: dir.path().to_path_buf(),
      level: CacheLevel::Disabled,
    };

    cache.save_choice("cowsay", "cowsay");
    assert!(cache.get_choice("cowsay").is_none());
  }

  #[test]
  fn cache_choice_level_does_not_cache_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = LocateCache {
      dir: dir.path().to_path_buf(),
      level: CacheLevel::Choice,
    };

    cache.save_choice("cowsay", "cowsay");
    assert_eq!(cache.get_choice("cowsay").as_deref(), Some("cowsay"));

    cache.save_path("cowsay", Path::new("/nix/store/abc"));
    assert!(cache.get_path("cowsay").is_none());
  }
}
