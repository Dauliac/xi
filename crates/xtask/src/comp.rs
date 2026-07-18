use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{CommandFactory, ValueEnum};
use clap_complete::generate_to;

const BINARY_NAME: &str = "xi";

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum CompletionShell {
  Bash,
  Elvish,
  Fish,
  PowerShell,
  Zsh,
  Nushell,
}

impl std::fmt::Display for CompletionShell {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Bash => write!(f, "Bash"),
      Self::Elvish => write!(f, "Elvish"),
      Self::Fish => write!(f, "Fish"),
      Self::PowerShell => write!(f, "PowerShell"),
      Self::Zsh => write!(f, "Zsh"),
      Self::Nushell => write!(f, "Nushell"),
    }
  }
}

const ALL_SHELLS: [CompletionShell; 6] = [
  CompletionShell::Bash,
  CompletionShell::Elvish,
  CompletionShell::Fish,
  CompletionShell::PowerShell,
  CompletionShell::Zsh,
  CompletionShell::Nushell,
];

pub fn generate(
  out_dir: &str,
  shell: Option<CompletionShell>,
) -> Result<(), String> {
  let gen_dir = Path::new(out_dir);
  if !gen_dir.exists() {
    std::fs::create_dir_all(gen_dir).map_err(|e| {
      format!("Failed to create output directory '{out_dir}': {e}")
    })?;
  }

  let mut cmd = xi::interface::Main::command();

  if let Some(shell) = shell {
    generate_single(shell, &mut cmd, gen_dir)?;
    println!("Generated {shell} completion to {out_dir}");
  } else {
    for shell in ALL_SHELLS {
      generate_single(shell, &mut cmd, gen_dir)?;
    }
    println!("Generated all completions to {out_dir}");
  }

  Ok(())
}

/// Find the `xi` binary alongside the running `xtask` binary.
///
/// During `nix build` postInstall, both live in `$out/bin/`.
/// During development, both are in `target/debug/` or `target/release/`.
fn find_xi_binary() -> Option<PathBuf> {
  let xtask_exe = std::env::current_exe().ok()?;
  let bin_dir = xtask_exe.parent()?;
  let xi_bin = bin_dir.join(BINARY_NAME);
  if xi_bin.is_file() { Some(xi_bin) } else { None }
}

/// Generate a dynamic completion script by running `COMPLETE=<shell> xi`.
///
/// Dynamic scripts call back into the `xi` binary at runtime, which means
/// custom `ArgValueCompleter` functions (e.g. flake output completion) work.
///
/// Returns `Ok(path)` on success, or `Err` if the binary couldn't be found/run.
fn generate_dynamic(
  shell_name: &str,
  ext: &str,
  out_dir: &Path,
) -> Result<PathBuf, String> {
  let xi_bin = find_xi_binary().ok_or_else(|| {
    format!("Could not find '{BINARY_NAME}' binary next to xtask")
  })?;

  let output = Command::new(&xi_bin)
    .env("COMPLETE", shell_name)
    .output()
    .map_err(|e| format!("Failed to run {}: {e}", xi_bin.display()))?;

  if !output.status.success() {
    return Err(format!(
      "COMPLETE={shell_name} {} exited with {}",
      xi_bin.display(),
      output.status
    ));
  }

  let script = String::from_utf8(output.stdout)
    .map_err(|e| format!("Non-UTF8 completion output: {e}"))?;

  if script.trim().is_empty() {
    return Err(format!(
      "COMPLETE={shell_name} {} produced empty output",
      xi_bin.display()
    ));
  }

  let path = out_dir.join(format!("{BINARY_NAME}.{ext}"));
  std::fs::write(&path, &script)
    .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;

  Ok(path)
}

fn generate_single(
  shell: CompletionShell,
  cmd: &mut clap::Command,
  out_dir: &Path,
) -> Result<PathBuf, String> {
  match shell {
    // Dynamic generation for shells with ArgValueCompleter support.
    // These scripts call back into `xi` at runtime for completions,
    // enabling custom completers (flake outputs, etc.).
    CompletionShell::Bash => {
      generate_dynamic("bash", "bash", out_dir).or_else(|e| {
        eprintln!(
          "Dynamic bash completion failed ({e}), falling back to static"
        );
        generate_to(clap_complete::Shell::Bash, cmd, BINARY_NAME, out_dir)
          .map_err(|e| format!("Failed to generate Bash completion: {e}"))
      })
    },
    CompletionShell::Fish => {
      generate_dynamic("fish", "fish", out_dir).or_else(|e| {
        eprintln!(
          "Dynamic fish completion failed ({e}), falling back to static"
        );
        generate_to(clap_complete::Shell::Fish, cmd, BINARY_NAME, out_dir)
          .map_err(|e| format!("Failed to generate Fish completion: {e}"))
      })
    },
    CompletionShell::Zsh => {
      generate_dynamic("zsh", "zsh", out_dir).or_else(|e| {
        eprintln!(
          "Dynamic zsh completion failed ({e}), falling back to static"
        );
        generate_to(clap_complete::Shell::Zsh, cmd, BINARY_NAME, out_dir)
          .map_err(|e| format!("Failed to generate Zsh completion: {e}"))
          .and_then(|path| {
            let new_path = path.with_file_name(format!("{BINARY_NAME}.zsh"));
            std::fs::rename(&path, &new_path)
              .map_err(|e| format!("Failed to rename Zsh completion: {e}"))?;
            Ok(new_path)
          })
      })
    },

    // Static generation for shells without dynamic completion support.
    CompletionShell::Elvish => {
      generate_to(clap_complete::Shell::Elvish, cmd, BINARY_NAME, out_dir)
        .map_err(|e| format!("Failed to generate Elvish completion: {e}"))
    },
    CompletionShell::PowerShell => {
      generate_to(clap_complete::Shell::PowerShell, cmd, BINARY_NAME, out_dir)
        .map_err(|e| format!("Failed to generate PowerShell completion: {e}"))
    },
    CompletionShell::Nushell => {
      generate_to(clap_complete_nushell::Nushell, cmd, BINARY_NAME, out_dir)
        .map_err(|e| format!("Failed to generate Nushell completion: {e}"))
    },
  }
}
