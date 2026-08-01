//! Flake output completion via nix's `NIX_GET_COMPLETIONS` mechanism.
//!
//! Each completion scope queries the relevant flake output category
//! (e.g. `packages`, `apps`, `devShells`) and strips the qualified prefix
//! so users get bare names matching xi's simplified API.
//!
//! Functions return `Vec<CompletionCandidate>` for direct use with
//! `clap_complete::engine::ArgValueCompleter`.

use std::ffi::OsStr;
use std::process::Command;

use clap_complete::engine::CompletionCandidate;

use crate::flake_output::FlakeOutput;

/// Return the current nix system string (e.g. `x86_64-linux`).
fn current_system() -> String {
  crate::flake_output::current_nix_system()
}

/// Query nix's built-in completion for a given installable prefix.
///
/// Uses `NIX_GET_COMPLETIONS=<pos> nix <subcmd> <query>` which returns
/// tab-separated candidates. First line is the completion type (`attrs`,
/// `filenames`, `normal`), remaining lines are candidates.
fn nix_complete(nix_subcmd: &str, query: &str, position: &str) -> Vec<String> {
  let nix_bin =
    std::env::var("XI_NIX_BIN").unwrap_or_else(|_| "nix".to_string());

  let output = Command::new(&nix_bin)
    .env("NIX_GET_COMPLETIONS", position)
    .args([nix_subcmd, query])
    .stderr(std::process::Stdio::null())
    .output();

  let Ok(output) = output else {
    return Vec::new();
  };

  let stdout = String::from_utf8_lossy(&output.stdout);
  stdout
    .lines()
    .skip(1) // skip type line (attrs/filenames/normal)
    .filter_map(|line| {
      let candidate = line.trim_end_matches('\t');
      if candidate.is_empty() {
        None
      } else {
        Some(candidate.to_string())
      }
    })
    .collect()
}

/// Extract the bare attr name from a qualified completion like
/// `.#packages.x86_64-linux.hello` → `hello`.
fn strip_to_bare_name(candidate: &str, prefix: &str) -> Option<String> {
  candidate
    .strip_prefix(prefix)
    .map(std::string::ToString::to_string)
}

/// Complete per-system flake outputs for a given category.
fn complete_per_system(
  category: &str,
  user_prefix: &str,
) -> Vec<CompletionCandidate> {
  let system = current_system();
  let query = format!(".#{category}.{system}.{user_prefix}");
  let prefix = format!(".#{category}.{system}.");

  nix_complete("build", &query, "2")
    .into_iter()
    .filter_map(|c| strip_to_bare_name(&c, &prefix))
    .map(CompletionCandidate::new)
    .collect()
}

/// Complete flat (non-per-system) flake outputs for a given category.
fn complete_flat(
  category: &str,
  user_prefix: &str,
) -> Vec<CompletionCandidate> {
  let query = format!(".#{category}.{user_prefix}");
  let prefix = format!(".#{category}.");

  nix_complete("build", &query, "2")
    .into_iter()
    .filter_map(|c| strip_to_bare_name(&c, &prefix))
    .map(CompletionCandidate::new)
    .collect()
}

/// Complete package names for `xi build`.
#[must_use]
pub fn complete_packages(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  complete_per_system(FlakeOutput::Packages.as_str(), prefix)
}

/// Complete app names for `xi run`.
///
/// Queries `apps` first, then `packages` as fallback (matching `nix run`
/// resolution order).
#[must_use]
pub fn complete_apps(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  let mut results = complete_per_system(FlakeOutput::Apps.as_str(), prefix);
  let existing: Vec<String> = results
    .iter()
    .filter_map(|c| {
      c.get_value().to_str().map(std::string::ToString::to_string)
    })
    .collect();
  for candidate in complete_per_system(FlakeOutput::Packages.as_str(), prefix) {
    if let Some(val) = candidate.get_value().to_str()
      && !existing.contains(&val.to_string())
    {
      results.push(candidate);
    }
  }
  results
}

/// Complete check names for `xi check`.
#[must_use]
pub fn complete_checks(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  complete_per_system(FlakeOutput::Checks.as_str(), prefix)
}

/// Complete devShell names for `xi develop`.
#[must_use]
pub fn complete_devshells(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  complete_per_system(FlakeOutput::DevShells.as_str(), prefix)
}

/// Complete NixOS configuration names for `xi os --hostname`.
#[must_use]
pub fn complete_nixos_configs(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  complete_flat(FlakeOutput::NixosConfigurations.as_str(), prefix)
}

/// Complete home-manager configuration names.
#[must_use]
pub fn complete_home_configs(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  complete_flat(FlakeOutput::HomeConfigurations.as_str(), prefix)
}

/// Complete darwin configuration names.
#[must_use]
pub fn complete_darwin_configs(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  complete_flat(FlakeOutput::DarwinConfigurations.as_str(), prefix)
}

/// Complete system-manager configuration names.
#[must_use]
pub fn complete_system_configs(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");
  complete_flat(FlakeOutput::SystemConfigs.as_str(), prefix)
}

/// Complete flake input names for `xi update`.
#[must_use]
pub fn complete_flake_inputs(current: &OsStr) -> Vec<CompletionCandidate> {
  let prefix = current.to_str().unwrap_or("");

  let nix_bin =
    std::env::var("XI_NIX_BIN").unwrap_or_else(|_| "nix".to_string());

  let output = Command::new(&nix_bin)
    .env("NIX_GET_COMPLETIONS", "3")
    .args(["flake", "update", prefix])
    .stderr(std::process::Stdio::null())
    .output();

  let Ok(output) = output else {
    return Vec::new();
  };

  let stdout = String::from_utf8_lossy(&output.stdout);
  stdout
    .lines()
    .skip(1)
    .filter_map(|line| {
      let candidate = line.trim_end_matches('\t');
      if candidate.is_empty() {
        None
      } else {
        Some(CompletionCandidate::new(candidate))
      }
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn strip_qualified_package_name() {
    assert_eq!(
      strip_to_bare_name(
        ".#packages.x86_64-linux.hello",
        ".#packages.x86_64-linux."
      ),
      Some("hello".to_string())
    );
  }

  #[test]
  fn strip_flat_config_name() {
    assert_eq!(
      strip_to_bare_name(
        ".#nixosConfigurations.myhost",
        ".#nixosConfigurations."
      ),
      Some("myhost".to_string())
    );
  }

  #[test]
  fn strip_no_match() {
    assert_eq!(
      strip_to_bare_name(".#apps.x86_64-linux.foo", ".#packages.x86_64-linux."),
      None
    );
  }

  #[test]
  fn current_system_format() {
    let sys = current_system();
    assert!(sys.contains('-'), "system should be arch-os: {sys}");
  }
}
