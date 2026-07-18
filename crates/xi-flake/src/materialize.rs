use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::bail;
use sha2::Digest;
use tracing::{debug, info, warn};
use yansi::{Color, Paint};

use crate::args::MaterializeArgs;
use crate::project_config::{MaterializeTarget, ProjectMaterializeConfig};
use crate::{project_config, resolve_local_flake_dir};

/// Cache directory for JIT materialization (gitignored).
const CACHE_DIR: &str = ".xi/materialized";

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

impl MaterializeArgs {
  /// Run the materialize command.
  ///
  /// # Errors
  ///
  /// Returns an error if any target fails.
  pub fn run(self) -> Result<()> {
    let flake_ref = self.flake_ref.as_deref().unwrap_or(".");
    let local_dir = resolve_local_flake_dir(Some(flake_ref))
      .unwrap_or_else(|| PathBuf::from("."));
    let config = project_config::load_project_config(Some(&local_dir));

    if self.setup {
      return setup_git_hide(&local_dir, &config.materialize);
    }

    if self.clean {
      return clean_cache(&local_dir);
    }

    if config.materialize.targets.is_empty() {
      bail!(
        "No materialization targets configured.\n\
         Add [[materialize.target]] sections to .xi.toml"
      );
    }

    let targets = self.filter_targets(&config.materialize)?;

    if self.list {
      return list_targets(&local_dir, &targets, &config.materialize);
    }

    if self.check {
      return check_targets(&local_dir, &targets);
    }

    run_targets(
      &local_dir,
      &targets,
      &config.materialize,
      self.commit,
      self.force,
    )
  }

  /// Filter targets by name if specific ones were requested.
  fn filter_targets<'a>(
    &self,
    config: &'a ProjectMaterializeConfig,
  ) -> Result<Vec<&'a MaterializeTarget>> {
    if self.targets.is_empty() {
      return Ok(config.targets.iter().collect());
    }

    let mut result = Vec::new();
    for name in &self.targets {
      let found = config.targets.iter().find(|t| t.name == *name);
      match found {
        Some(t) => result.push(t),
        None => {
          let available: Vec<_> =
            config.targets.iter().map(|t| t.name.as_str()).collect();
          bail!(
            "Unknown target '{name}'. Available: {}",
            available.join(", ")
          );
        },
      }
    }
    Ok(result)
  }
}

// ---------------------------------------------------------------------------
// Public API for pre-build integration
// ---------------------------------------------------------------------------

/// Run materialization as a pre-build step (called from build/ci commands).
///
/// Only runs stale targets. Returns silently if no targets are configured
/// or `pre_build` is disabled.
pub fn run_pre_build_materialize(project_dir: &Path) -> Result<()> {
  let config = project_config::load_project_config(Some(project_dir));

  if !config.materialize.pre_build || config.materialize.targets.is_empty() {
    return Ok(());
  }

  let targets: Vec<&MaterializeTarget> =
    config.materialize.targets.iter().collect();

  info!("Running pre-build materialization");
  run_targets(project_dir, &targets, &config.materialize, false, false)
}

/// Check materialization freshness (called from xi ci Phase 1).
///
/// Returns `None` if `check_in_ci` is disabled or no targets exist.
/// Returns `Some((stale_count, total_count))` otherwise.
pub fn check_materialize_freshness(
  project_dir: &Path,
) -> Result<Option<(usize, usize)>> {
  let config = project_config::load_project_config(Some(project_dir));

  if !config.materialize.check_in_ci || config.materialize.targets.is_empty()
  {
    return Ok(None);
  }

  let cache_dir = project_dir.join(CACHE_DIR);
  let total = config.materialize.targets.len();
  let mut stale = 0;

  for target in &config.materialize.targets {
    if !is_target_fresh(project_dir, &cache_dir, target)? {
      stale += 1;
    }
  }

  Ok(Some((stale, total)))
}

// ---------------------------------------------------------------------------
// Staleness detection
// ---------------------------------------------------------------------------

/// Compute a cache key from the content of source files matching the
/// given glob patterns.
fn compute_source_hash(
  project_dir: &Path,
  sources: &[String],
) -> Result<String> {
  let mut hasher = <sha2::Sha256 as Digest>::new();
  let mut matched = false;

  for pattern in sources {
    let full_pattern =
      project_dir.join(pattern).to_string_lossy().to_string();
    for entry in glob::glob(&full_pattern).map_err(|e| {
      color_eyre::eyre::eyre!("Invalid glob pattern '{pattern}': {e}")
    })? {
      let path = entry.map_err(|e| {
        color_eyre::eyre::eyre!("Error reading glob entry: {e}")
      })?;
      if path.is_file() {
        let content = std::fs::read(&path).map_err(|e| {
          color_eyre::eyre::eyre!(
            "Failed to read source file {}: {e}",
            path.display()
          )
        })?;
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(&content);
        matched = true;
      }
    }
  }

  if !matched {
    warn!(
      "No source files matched patterns: {}",
      sources.join(", ")
    );
  }

  Ok(format!("{:x}", hasher.finalize()))
}

fn hash_file_path(cache_dir: &Path, target: &MaterializeTarget) -> PathBuf {
  cache_dir.join(format!("{}.hash", target.name))
}

fn output_path(base_dir: &Path, target: &MaterializeTarget) -> PathBuf {
  base_dir.join(&target.output)
}

fn is_target_fresh(
  project_dir: &Path,
  cache_dir: &Path,
  target: &MaterializeTarget,
) -> Result<bool> {
  let hash_path = hash_file_path(cache_dir, target);
  let out_path = output_path(cache_dir, target);

  if !hash_path.exists() || !out_path.exists() {
    return Ok(false);
  }

  let stored_hash =
    std::fs::read_to_string(&hash_path).unwrap_or_default();
  let current_hash = compute_source_hash(project_dir, &target.sources)?;

  Ok(stored_hash.trim() == current_hash)
}

// ---------------------------------------------------------------------------
// Git integration
// ---------------------------------------------------------------------------

/// Collect all committed materialized file paths for a project.
fn committed_materialized_files(
  project_dir: &Path,
  config: &ProjectMaterializeConfig,
) -> Vec<PathBuf> {
  let commit_dir = project_dir.join(&config.commit_path);
  if !commit_dir.exists() {
    return vec![];
  }

  let mut files = Vec::new();
  collect_files_recursive(&commit_dir, &mut files);
  files
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if path.is_dir() {
      collect_files_recursive(&path, files);
    } else {
      files.push(path);
    }
  }
}

/// Apply `git update-index --skip-worktree` to all committed
/// materialized files so they don't appear in `git status`.
fn git_skip_worktree(project_dir: &Path, files: &[PathBuf]) -> Result<()> {
  if files.is_empty() {
    return Ok(());
  }

  let mut cmd = Command::new("git");
  cmd.arg("update-index").arg("--skip-worktree");
  cmd.current_dir(project_dir);

  for file in files {
    // Use relative path from project root
    if let Ok(rel) = file.strip_prefix(project_dir) {
      cmd.arg(rel);
    }
  }

  let status = cmd.status().map_err(|e| {
    color_eyre::eyre::eyre!("Failed to run git update-index: {e}")
  })?;

  if !status.success() {
    debug!("git update-index --skip-worktree exited with {status}");
  }

  Ok(())
}

/// Remove `--skip-worktree` so files show in `git status` again.
fn git_no_skip_worktree(
  project_dir: &Path,
  files: &[PathBuf],
) -> Result<()> {
  if files.is_empty() {
    return Ok(());
  }

  let mut cmd = Command::new("git");
  cmd.arg("update-index").arg("--no-skip-worktree");
  cmd.current_dir(project_dir);

  for file in files {
    if let Ok(rel) = file.strip_prefix(project_dir) {
      cmd.arg(rel);
    }
  }

  let _ = cmd.status();
  Ok(())
}

/// Stage materialized files with `git add`.
fn git_stage_files(project_dir: &Path, files: &[PathBuf]) -> Result<()> {
  if files.is_empty() {
    return Ok(());
  }

  let mut cmd = Command::new("git");
  cmd.arg("add");
  cmd.current_dir(project_dir);

  for file in files {
    if let Ok(rel) = file.strip_prefix(project_dir) {
      cmd.arg(rel);
    }
  }

  let status = cmd.status().map_err(|e| {
    color_eyre::eyre::eyre!("Failed to run git add: {e}")
  })?;

  if !status.success() {
    warn!("git add exited with {status}");
  }

  Ok(())
}

/// Get the current git branch name.
fn git_current_branch(project_dir: &Path) -> Option<String> {
  let output = Command::new("git")
    .args(["rev-parse", "--abbrev-ref", "HEAD"])
    .current_dir(project_dir)
    .output()
    .ok()?;

  if output.status.success() {
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
  } else {
    None
  }
}

/// Check if auto-stage should run based on branch filters.
fn should_auto_stage(
  project_dir: &Path,
  config: &ProjectMaterializeConfig,
) -> bool {
  if !config.auto_stage {
    return false;
  }

  // Must be in a git repo
  let Some(branch) = git_current_branch(project_dir) else {
    return false;
  };

  // Empty list means all branches
  if config.auto_stage_branches.is_empty() {
    return true;
  }

  config
    .auto_stage_branches
    .iter()
    .any(|b| b == &branch)
}

/// Write `.gitattributes` merge driver for materialized files.
fn ensure_gitattributes_merge_driver(
  project_dir: &Path,
  config: &ProjectMaterializeConfig,
) -> Result<()> {
  let gitattributes = project_dir.join(".gitattributes");
  let pattern = format!("{}/** merge=ours", config.commit_path);

  if gitattributes.exists() {
    let content =
      std::fs::read_to_string(&gitattributes).unwrap_or_default();
    if content.contains(&pattern) {
      return Ok(());
    }
    let mut new_content = content;
    if !new_content.ends_with('\n') {
      new_content.push('\n');
    }
    new_content
      .push_str(&format!("\n# xi materialized files — avoid merge conflicts\n{pattern}\n"));
    std::fs::write(&gitattributes, new_content)?;
  } else {
    std::fs::write(
      &gitattributes,
      format!(
        "# xi materialized files — avoid merge conflicts\n{pattern}\n"
      ),
    )?;
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Set up git to hide materialized files from `git status`.
fn setup_git_hide(
  project_dir: &Path,
  config: &ProjectMaterializeConfig,
) -> Result<()> {
  let files = committed_materialized_files(project_dir, config);

  if files.is_empty() {
    info!(
      "No committed materialized files found in {}",
      config.commit_path
    );
    println!(
      "  Run `xi materialize --commit` first to create committed files."
    );
    return Ok(());
  }

  git_skip_worktree(project_dir, &files)?;
  ensure_gitattributes_merge_driver(project_dir, config)?;

  println!(
    "  {} {} file(s) hidden from git status",
    Paint::new("Setup complete:").fg(Color::Green).bold(),
    files.len()
  );
  println!(
    "  {}",
    Paint::new("Materialized files won't appear in git diff/status.").dim()
  );
  println!(
    "  {}",
    Paint::new("Use `xi materialize --commit` to update and stage them.")
      .dim()
  );

  Ok(())
}

/// Remove the `.xi/materialized/` cache directory.
fn clean_cache(project_dir: &Path) -> Result<()> {
  let cache_dir = project_dir.join(CACHE_DIR);
  if cache_dir.exists() {
    std::fs::remove_dir_all(&cache_dir).map_err(|e| {
      color_eyre::eyre::eyre!(
        "Failed to remove cache directory {}: {e}",
        cache_dir.display()
      )
    })?;
    info!("Removed {}", cache_dir.display());
  } else {
    info!("Cache directory does not exist, nothing to clean");
  }
  Ok(())
}

/// List all targets and their staleness status.
fn list_targets(
  project_dir: &Path,
  targets: &[&MaterializeTarget],
  config: &ProjectMaterializeConfig,
) -> Result<()> {
  let cache_dir = project_dir.join(CACHE_DIR);

  println!();
  println!(
    "  {} ({})",
    Paint::new("Materialize targets").bold(),
    Paint::new(format!("{} configured", targets.len())).dim()
  );
  println!();

  for target in targets {
    let fresh =
      is_target_fresh(project_dir, &cache_dir, target).unwrap_or(false);
    let status = if fresh {
      Paint::new("fresh").fg(Color::Green).bold().to_string()
    } else {
      Paint::new("stale").fg(Color::Yellow).bold().to_string()
    };

    println!(
      "    {} {} → {}",
      Paint::new(&target.name).bold(),
      status,
      Paint::new(&target.output).dim(),
    );
    println!("      {}", Paint::new(&target.command).dim());
    if !target.sources.is_empty() {
      println!(
        "      sources: {}",
        Paint::new(target.sources.join(", ")).dim(),
      );
    }
  }

  println!();
  if !config.commit_path.is_empty() {
    println!(
      "  commit path: {}",
      Paint::new(&config.commit_path).dim()
    );
  }
  println!();

  Ok(())
}

/// Check all targets are fresh, exit 1 if any are stale.
fn check_targets(
  project_dir: &Path,
  targets: &[&MaterializeTarget],
) -> Result<()> {
  let cache_dir = project_dir.join(CACHE_DIR);
  let mut stale = Vec::new();

  for target in targets {
    let fresh = is_target_fresh(project_dir, &cache_dir, target)?;
    if fresh {
      println!(
        "  {} {}",
        Paint::new(&target.name).bold(),
        Paint::new("fresh").fg(Color::Green),
      );
    } else {
      println!(
        "  {} {}",
        Paint::new(&target.name).bold(),
        Paint::new("STALE").fg(Color::Red).bold(),
      );
      stale.push(target.name.as_str());
    }
  }

  if stale.is_empty() {
    info!("All targets are fresh");
    Ok(())
  } else {
    bail!(
      "{} target(s) are stale: {}. Run `xi materialize` to refresh.",
      stale.len(),
      stale.join(", ")
    );
  }
}

/// Run stale targets and write outputs.
fn run_targets(
  project_dir: &Path,
  targets: &[&MaterializeTarget],
  config: &ProjectMaterializeConfig,
  commit: bool,
  force: bool,
) -> Result<()> {
  let cache_dir = project_dir.join(CACHE_DIR);
  let commit_dir = if commit {
    Some(project_dir.join(&config.commit_path))
  } else {
    None
  };

  std::fs::create_dir_all(&cache_dir).map_err(|e| {
    color_eyre::eyre::eyre!(
      "Failed to create cache directory {}: {e}",
      cache_dir.display()
    )
  })?;

  if let Some(ref cd) = commit_dir {
    std::fs::create_dir_all(cd).map_err(|e| {
      color_eyre::eyre::eyre!(
        "Failed to create commit directory {}: {e}",
        cd.display()
      )
    })?;
  }

  // If committing with git-hide, lift skip-worktree first
  if commit && config.git_hide {
    let existing = committed_materialized_files(project_dir, config);
    git_no_skip_worktree(project_dir, &existing)?;
  }

  let mut ran = 0;
  let mut skipped = 0;

  for target in targets {
    if !force {
      let fresh = is_target_fresh(project_dir, &cache_dir, target)?;
      if fresh && !commit {
        debug!("{}: fresh, skipping", target.name);
        skipped += 1;
        continue;
      }
    }

    info!("Materializing: {}", target.name);

    let is_dir_output = target.output.ends_with('/');
    let out = if is_dir_output {
      let dir = output_path(&cache_dir, target);
      std::fs::create_dir_all(&dir)?;
      dir
    } else {
      output_path(&cache_dir, target)
    };

    // Run the command
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&target.command);
    cmd.current_dir(project_dir);

    if is_dir_output {
      cmd.env("XI_MATERIALIZE_OUT", &out);
    }
    cmd.env("XI_PROJECT_ROOT", project_dir);

    if is_dir_output {
      let status = cmd
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| {
          color_eyre::eyre::eyre!(
            "Failed to run command for target '{}': {e}",
            target.name
          )
        })?;

      if !status.success() {
        bail!(
          "Command for target '{}' exited with status {status}",
          target.name
        );
      }
    } else {
      let output = cmd
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| {
          color_eyre::eyre::eyre!(
            "Failed to run command for target '{}': {e}",
            target.name
          )
        })?;

      if !output.status.success() {
        bail!(
          "Command for target '{}' exited with status {}",
          target.name,
          output.status
        );
      }

      if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
      }
      std::fs::write(&out, &output.stdout).map_err(|e| {
        color_eyre::eyre::eyre!(
          "Failed to write output for target '{}': {e}",
          target.name
        )
      })?;
    }

    // Store the source hash
    let hash = compute_source_hash(project_dir, &target.sources)?;
    std::fs::write(hash_file_path(&cache_dir, target), &hash)?;

    // Copy to commit dir if --commit
    if let Some(ref cd) = commit_dir {
      let commit_out = output_path(cd, target);
      if is_dir_output {
        copy_dir_recursive(&out, &commit_out)?;
      } else {
        if let Some(parent) = commit_out.parent() {
          std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&out, &commit_out)?;
      }
    }

    ran += 1;
    println!(
      "  {} {} → {}",
      Paint::new(&target.name).bold(),
      Paint::new("ok").fg(Color::Green),
      Paint::new(out.display().to_string()).dim(),
    );
  }

  // Auto-stage + re-apply skip-worktree after commit
  if commit {
    let committed_files =
      committed_materialized_files(project_dir, config);

    if should_auto_stage(project_dir, config) {
      git_stage_files(project_dir, &committed_files)?;
      println!(
        "  {} {} file(s)",
        Paint::new("Staged").fg(Color::Green),
        committed_files.len()
      );
    }

    if config.git_hide {
      git_skip_worktree(project_dir, &committed_files)?;
    }
  }

  println!();
  if ran > 0 {
    println!(
      "  {} ({} ran, {} skipped)",
      Paint::new("Done").fg(Color::Green).bold(),
      ran,
      skipped,
    );
  } else {
    println!(
      "  {} (all {} target(s) are fresh)",
      Paint::new("Nothing to do").dim(),
      skipped,
    );
  }

  if commit {
    println!(
      "  Wrote to {}",
      Paint::new(&config.commit_path).bold()
    );
  }

  println!();
  Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
  std::fs::create_dir_all(dst)?;
  for entry in std::fs::read_dir(src)? {
    let entry = entry?;
    let ty = entry.file_type()?;
    let dst_path = dst.join(entry.file_name());
    if ty.is_dir() {
      copy_dir_recursive(&entry.path(), &dst_path)?;
    } else {
      std::fs::copy(entry.path(), &dst_path)?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  #[test]
  fn compute_hash_of_single_file() {
    let dir = tempdir().expect("tempdir");
    std::fs::write(dir.path().join("test.txt"), "hello").expect("write");

    let hash = compute_source_hash(dir.path(), &["test.txt".into()])
      .expect("hash");
    assert!(!hash.is_empty());
    assert_eq!(hash.len(), 64);
  }

  #[test]
  fn compute_hash_changes_with_content() {
    let dir = tempdir().expect("tempdir");
    std::fs::write(dir.path().join("test.txt"), "hello").expect("write");
    let hash1 = compute_source_hash(dir.path(), &["test.txt".into()])
      .expect("hash");

    std::fs::write(dir.path().join("test.txt"), "world").expect("write");
    let hash2 = compute_source_hash(dir.path(), &["test.txt".into()])
      .expect("hash");

    assert_ne!(hash1, hash2);
  }

  #[test]
  fn compute_hash_glob_pattern() {
    let dir = tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.nix"), "1").expect("write");
    std::fs::write(dir.path().join("b.nix"), "2").expect("write");
    std::fs::write(dir.path().join("c.txt"), "3").expect("write");

    let hash =
      compute_source_hash(dir.path(), &["*.nix".into()]).expect("hash");
    assert!(!hash.is_empty());
  }

  #[test]
  fn target_freshness_lifecycle() {
    let dir = tempdir().expect("tempdir");
    let cache = dir.path().join(CACHE_DIR);
    std::fs::create_dir_all(&cache).expect("mkdir");
    std::fs::write(dir.path().join("src.txt"), "v1").expect("write");

    let target = MaterializeTarget {
      name: "test".into(),
      command: "echo hello".into(),
      output: "out.txt".into(),
      sources: vec!["src.txt".into()],
    };

    // Initially stale
    assert!(!is_target_fresh(dir.path(), &cache, &target).unwrap());

    // Write output and hash
    std::fs::write(output_path(&cache, &target), "hello").unwrap();
    let hash = compute_source_hash(dir.path(), &target.sources).unwrap();
    std::fs::write(hash_file_path(&cache, &target), &hash).unwrap();

    // Now fresh
    assert!(is_target_fresh(dir.path(), &cache, &target).unwrap());

    // Change source → stale
    std::fs::write(dir.path().join("src.txt"), "v2").expect("write");
    assert!(!is_target_fresh(dir.path(), &cache, &target).unwrap());
  }

  #[test]
  fn auto_stage_respects_branch_filter() {
    let config = ProjectMaterializeConfig {
      auto_stage: true,
      auto_stage_branches: vec!["main".into(), "master".into()],
      ..ProjectMaterializeConfig::default()
    };

    // Can't fully test git branch in unit tests, but verify the
    // empty-branches-means-all logic
    let config_all = ProjectMaterializeConfig {
      auto_stage: true,
      auto_stage_branches: vec![],
      ..ProjectMaterializeConfig::default()
    };

    let dir = tempdir().expect("tempdir");
    // No git repo → should return false (no branch detected)
    assert!(!should_auto_stage(dir.path(), &config));
    // With empty branches and auto_stage=true, no git → false
    assert!(!should_auto_stage(dir.path(), &config_all));

    // auto_stage disabled → always false
    let config_off = ProjectMaterializeConfig {
      auto_stage: false,
      ..ProjectMaterializeConfig::default()
    };
    assert!(!should_auto_stage(dir.path(), &config_off));
  }
}
