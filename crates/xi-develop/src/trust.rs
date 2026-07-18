use std::{fs, path::Path};

use color_eyre::Result;

use crate::{dirs, ui};

/// Check if a flake is trusted.
pub fn is_trusted(flake_root: &Path) -> bool {
  let Ok(canonical) = fs::canonicalize(flake_root) else {
    return false;
  };
  let fid = dirs::flake_id(&canonical);
  dirs::trust_dir().join(&fid).exists()
}

/// Trust a flake for automatic devshell activation.
///
/// # Errors
///
/// Returns an error if the trust marker cannot be written.
pub fn trust_flake(flake_ref: &str, _target: &str) -> Result<()> {
  let canonical = fs::canonicalize(flake_ref).map_err(|e| {
    color_eyre::eyre::eyre!("Cannot resolve flake path '{}': {}", flake_ref, e)
  })?;
  let fid = dirs::flake_id(&canonical);
  let trust_dir = dirs::trust_dir();
  fs::create_dir_all(&trust_dir)?;
  let trust_path = trust_dir.join(&fid);
  fs::write(&trust_path, "")?;

  ui::success(format!("trusted {}", canonical.display()));

  // Only show shell setup instructions if the hook is not loaded.
  // The activation script sets __XI_ORIG_PATH on first load.
  if std::env::var_os("__XI_ORIG_PATH").is_none() {
    ui::info("to activate automatically, add to your shell config:");
    eprintln!("    bash (~/.bashrc):  eval \"$(xi develop activate bash)\"");
    eprintln!("    zsh (~/.zshrc):    eval \"$(xi develop activate zsh)\"");
    eprintln!(
      "    fish (~/.config/fish/config.fish):  xi develop activate fish | source"
    );
  }

  Ok(())
}

/// Revoke trust for a flake.
///
/// # Errors
///
/// Returns an error if the trust marker cannot be removed.
pub fn untrust_flake(flake_ref: &str, _target: &str) -> Result<()> {
  let canonical = fs::canonicalize(flake_ref).map_err(|e| {
    color_eyre::eyre::eyre!("Cannot resolve flake path '{}': {}", flake_ref, e)
  })?;
  let fid = dirs::flake_id(&canonical);
  let trust_path = dirs::trust_dir().join(&fid);
  match fs::remove_file(&trust_path) {
    Ok(()) => ui::success(format!("untrusted {}", canonical.display())),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      ui::warn(format!("already untrusted: {}", canonical.display()));
    },
    Err(e) => return Err(e.into()),
  }

  Ok(())
}
