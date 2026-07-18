use color_eyre::Result;

use crate::args::SwitchArgs;
use crate::diff::PackageDiff;
use crate::shell::ShellType;
use crate::{dirs, env_file, meta, ui};

/// Handle the `xi develop switch <TARGET>` command.
/// Outputs shell-eval-able code to switch the active devshell.
///
/// # Errors
///
/// Returns an error if eval or env file writing fails.
pub fn switch(args: &SwitchArgs) -> Result<()> {
  let flake_ref = args.flake.as_deref().unwrap_or(".");
  let target = &args.target;

  let fid = dirs::flake_id_from_ref(flake_ref)?;
  let state_dir = dirs::state_dir(&fid);

  dirs::ensure_cache_version(&state_dir)?;

  // Check if already on this target
  let old_target =
    std::env::var("__XI_TARGET").unwrap_or_else(|_| "default".into());
  if old_target == *target {
    ui::info(format!("already on devshell '{target}'"));
    return Ok(());
  }

  // Load old packages for diff
  let old_packages = meta::load(&state_dir)
    .ok()
    .map(|m| m.packages)
    .unwrap_or_default();

  let profile_path = dirs::profile_path(&state_dir, target);

  // Check if env file already exists for this target
  let env_path = state_dir.join(ShellType::Bash.env_file_name(target));
  let new_packages = if env_path.exists() {
    // Already cached — load its meta
    // (meta.json only stores the last eval'd target, so re-eval to get packages)
    let dev_env =
      env_file::eval_devshell_niced(flake_ref, target, Some(&profile_path))?;
    dev_env.packages
  } else {
    ui::loading(format!("evaluating devshell '{target}'"));
    let dev_env =
      env_file::eval_devshell_niced(flake_ref, target, Some(&profile_path))?;

    // Write env files for all shell types
    for shell in [ShellType::Bash, ShellType::Zsh, ShellType::Fish] {
      env_file::write_env_file(&state_dir, &dev_env, shell, target)?;
    }

    // Save meta
    meta::save(
      &state_dir,
      &meta::DevShellMeta {
        env_hash: dev_env.env_hash.clone(),
        target: target.clone(),
        flake_root: flake_ref.to_string(),
        store_path: None,
        packages: dev_env.packages.clone(),
        timestamp: meta::now_secs(),
        eval_duration_ms: 0,
        lock_hash: None,
        input_hash: None,
      },
    )?;

    dev_env.packages
  };

  // Show diff
  let pkg_diff = PackageDiff::compute(&old_packages, &new_packages);
  if !pkg_diff.is_empty() {
    pkg_diff.print_full();
  }

  // Output export statement for the shell to eval
  println!("export __XI_TARGET={target}");
  println!("unset __XI_HOOK_RAN 2>/dev/null");

  Ok(())
}
