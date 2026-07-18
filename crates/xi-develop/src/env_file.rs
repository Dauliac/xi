use std::{collections::HashMap, fs, io::Write, path::Path};

use color_eyre::Result;
use serde::Deserialize;

use crate::{shell::ShellType, store_path};

/// Variables to never export (preserve from user's current shell).
const SKIP_VARS: &[&str] = &[
  "HOME",
  "USER",
  "LOGNAME",
  "SHELL",
  "TERM",
  "COLORTERM",
  "TERM_PROGRAM",
  "DISPLAY",
  "WAYLAND_DISPLAY",
  "XDG_SESSION_TYPE",
  "XDG_RUNTIME_DIR",
  "XDG_DATA_DIRS",
  "DBUS_SESSION_BUS_ADDRESS",
  "SSH_AUTH_SOCK",
  "SSH_AGENT_PID",
  "LANG",
  "LANGUAGE",
  "LC_ALL",
  "LC_CTYPE",
  "TZ",
  "PAGER",
  "EDITOR",
  "VISUAL",
  "PWD",
  "OLDPWD",
  "SHLVL",
  "TMPDIR",
  "_",
  // Nix build-only
  "NIX_BUILD_TOP",
  "NIX_LOG_DIR",
  "NIX_BUILD_CORES",
  "NIX_ENFORCE_PURITY",
  "NIX_ENFORCE_NO_NATIVE",
  "NIX_STORE",
  "TEMP",
  "TEMPDIR",
  "TMP",
  // Handled separately
  "PATH",
];

/// Base name for the depth-scoped registry env var that tracks xi-injected
/// variable names. The actual var is `__XI_INJECTED_VARS_<depth>` where depth
/// comes from `$__XI_DEPTH` at runtime (defaults to 1).
/// Each nesting level only cleans up its own vars, preserving parent devshell env.
const INJECTED_VARS_BASE: &str = "__XI_INJECTED_VARS";

/// Parsed development environment from `nix print-dev-env --json`.
#[derive(Debug)]
pub struct DevEnv {
  /// Nix store paths to prepend to PATH.
  pub nix_paths: Vec<String>,
  /// Environment variables to export.
  pub env_vars: HashMap<String, String>,
  /// Shell hook (run once per session).
  pub shell_hook: Option<String>,
  /// Hash of the full environment (for change detection).
  pub env_hash: String,
  /// Parsed packages from nix store paths.
  pub packages: Vec<store_path::PackageInfo>,
}

#[derive(Deserialize)]
struct PrintDevEnv {
  variables: HashMap<String, Variable>,
}

#[derive(Deserialize)]
struct Variable {
  #[serde(rename = "type")]
  var_type: String,
  value: serde_json::Value,
}

impl DevEnv {
  /// Parse a `DevEnv` from `nix print-dev-env --json` output.
  ///
  /// # Errors
  ///
  /// Returns an error if JSON parsing fails.
  pub fn from_nix_json(json: &[u8]) -> Result<Self> {
    let data: PrintDevEnv = serde_json::from_slice(json).map_err(|e| {
      color_eyre::eyre::eyre!("Failed to parse nix print-dev-env output: {e}")
    })?;

    let mut env_vars = HashMap::new();
    let mut shell_hook = None;
    let mut nix_paths = Vec::new();

    for (key, var) in &data.variables {
      if var.var_type != "exported" {
        continue;
      }
      let serde_json::Value::String(value) = &var.value else {
        continue;
      };

      if key == "PATH" {
        // Keep all paths from nix's dev environment (not just /nix/store).
        // Non-store paths like /nix/var/nix/profiles/*/bin or
        // ~/.nix-profile/bin are valid and must not be dropped.
        nix_paths = value
          .split(':')
          .filter(|p| !p.is_empty())
          .map(String::from)
          .collect();
        continue;
      }

      if key == "shellHook" {
        if !value.trim().is_empty() {
          shell_hook = Some(value.clone());
        }
        continue;
      }

      if SKIP_VARS.contains(&key.as_str()) {
        continue;
      }

      env_vars.insert(key.clone(), value.clone());
    }

    let packages = nix_paths
      .iter()
      .filter_map(|p| store_path::parse(p))
      .collect();

    let env_hash = compute_hash(&nix_paths, &env_vars, shell_hook.as_ref());

    Ok(Self {
      nix_paths,
      env_vars,
      shell_hook,
      env_hash,
      packages,
    })
  }
}

/// Write the env file atomically (tmp + fsync + rename).
///
/// # Errors
///
/// Returns an error if file I/O fails.
pub fn write_env_file(
  state_dir: &Path,
  dev_env: &DevEnv,
  shell: ShellType,
  target: &str,
) -> Result<()> {
  fs::create_dir_all(state_dir)?;

  let env_path = state_dir.join(shell.env_file_name(target));
  let content = generate_content(dev_env, shell);

  // Atomic: write to tmp, fsync, rename
  let temp_path = env_path.with_extension("tmp");
  {
    let mut file = fs::File::create(&temp_path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
  }
  fs::rename(&temp_path, &env_path)?;

  tracing::debug!("Wrote env file: {}", env_path.display());
  Ok(())
}

/// Generate the cleanup preamble that unsets previously-injected vars.
///
/// Uses depth-scoped registries (`__XI_INJECTED_VARS_<depth>`) so that
/// nested devshells only clean up their own level, preserving parent vars.
/// The depth is read from `$__XI_DEPTH` at runtime (defaults to 1).
#[must_use]
pub fn generate_cleanup_preamble(shell: ShellType) -> Vec<String> {
  match shell {
    ShellType::Fish => vec![
      // Resolve depth (default 1)
      r#"set -l __xi_d (test -n "$__XI_DEPTH"; and echo $__XI_DEPTH; or echo 1)"#
        .to_string(),
      // Build registry var name and unset its contents
      r#"set -l __xi_reg "{BASE}_$__xi_d""#
        .replace("{BASE}", INJECTED_VARS_BASE),
      format!(
        "if set -q $__xi_reg\n  \
           for __xi_var in $$__xi_reg\n    \
             set -e $__xi_var\n  \
           end\n  \
           set -e $__xi_reg\n\
         end"
      ),
    ],
    ShellType::Bash | ShellType::Zsh => vec![
      // Resolve depth (default 1)
      #[allow(clippy::literal_string_with_formatting_args)]
      r#"__xi_d="${__XI_DEPTH:-1}""#.to_string(),
      // Read the depth-scoped registry and unset its contents
      format!(
        "eval \"__xi_keys=\\${{{BASE}_${{__xi_d}}:-}}\"\n\
         if [[ -n \"$__xi_keys\" ]]; then\n  \
           eval \"unset $__xi_keys\"\n  \
           eval \"unset {BASE}_${{__xi_d}}\"\n\
         fi",
        BASE = INJECTED_VARS_BASE,
      ),
    ],
  }
}

/// Generate the depth-scoped registry export that records which vars were
/// injected at this nesting level. Uses `$__xi_d` (set by cleanup preamble).
#[must_use]
pub fn generate_registry(keys: &[&str], shell: ShellType) -> String {
  let registry_value = keys.join(" ");
  match shell {
    ShellType::Fish => {
      format!("set -gx {INJECTED_VARS_BASE}_$__xi_d '{registry_value}'")
    },
    ShellType::Bash | ShellType::Zsh => {
      let escaped = registry_value.replace('\'', "'\\''");
      format!("eval \"export {INJECTED_VARS_BASE}_${{__xi_d}}='{escaped}'\"")
    },
  }
}

fn generate_content(dev_env: &DevEnv, shell: ShellType) -> String {
  let mut lines = vec!["# Generated by xi develop — do not edit".to_string()];

  // Cleanup preamble: unset all previously-injected vars
  lines.extend(generate_cleanup_preamble(shell));

  // PATH: prepend nix store paths to current $PATH.
  // The subshell inherits the parent's full PATH, so we just prepend.
  if !dev_env.nix_paths.is_empty() {
    match shell {
      ShellType::Fish => {
        lines.push(format!(
          "set -gx PATH {} $PATH",
          dev_env.nix_paths.join(" ")
        ));
      },
      ShellType::Bash | ShellType::Zsh => {
        let nix_path_str = dev_env.nix_paths.join(":");
        lines.push(format!("export PATH=\"{nix_path_str}:$PATH\""));
      },
    }
  }

  // Env vars (sorted for determinism)
  let mut sorted: Vec<_> = dev_env.env_vars.iter().collect();
  sorted.sort_by_key(|(k, _)| *k);
  let mut injected_keys: Vec<&str> = Vec::new();
  for (key, value) in &sorted {
    lines.push(shell.export(key, value));
    injected_keys.push(key);
  }

  // IN_NIX_SHELL indicator
  lines.push(shell.export("IN_NIX_SHELL", "impure"));
  injected_keys.push("IN_NIX_SHELL");

  // Registry: record what we injected so next source can clean up
  lines.push(generate_registry(&injected_keys, shell));

  // Cleanup temp vars used by preamble/registry
  lines.push(generate_temp_cleanup(shell));

  lines.join("\n") + "\n"
}

/// Generate cleanup of temp variables used by the preamble and registry.
fn generate_temp_cleanup(shell: ShellType) -> String {
  match shell {
    ShellType::Fish => "set -e __xi_d __xi_reg __xi_var __xi_keys".to_string(),
    ShellType::Bash | ShellType::Zsh => {
      "unset __xi_d __xi_keys 2>/dev/null".to_string()
    },
  }
}

fn compute_hash(
  paths: &[String],
  env_vars: &HashMap<String, String>,
  hook: Option<&String>,
) -> String {
  use sha2::{Digest, Sha256};

  let mut hasher = Sha256::new();

  for p in paths {
    hasher.update(p.as_bytes());
    hasher.update(b"\0");
  }

  let mut sorted_keys: Vec<_> = env_vars.keys().collect();
  sorted_keys.sort();
  for k in sorted_keys {
    hasher.update(k.as_bytes());
    hasher.update(b"=");
    if let Some(v) = env_vars.get(k) {
      hasher.update(v.as_bytes());
    }
    hasher.update(b"\0");
  }

  if let Some(h) = hook {
    hasher.update(b"hook:");
    hasher.update(h.as_bytes());
  }

  let hash = hasher.finalize();
  crate::dirs::hex_encode(&hash[..16])
}

/// Run a nix print-dev-env evaluation (sync).
/// Used by `switch`, `exec`, and `enter` modules.
///
/// When `profile_path` is `Some`, passes `--profile <path>` to nix so the
/// devshell closure is registered as a GC root automatically — preventing
/// `nix-collect-garbage` from deleting packages referenced in the env/hook
/// files.
///
/// # Errors
///
/// Returns an error if nix fails or output can't be parsed.
pub fn eval_devshell_niced(
  flake_ref: &str,
  target: &str,
  profile_path: Option<&Path>,
) -> Result<DevEnv> {
  let installable = if target == "default" {
    ".#".to_string()
  } else {
    format!(".#{target}")
  };

  let mut cmd = std::process::Command::new(nix_command::find_real_nix_binary());
  cmd.args([
    "print-dev-env",
    &installable,
    "--json",
    "--option",
    "connect-timeout",
    "5",
    "--option",
    "download-attempts",
    "2",
  ]);

  if let Some(profile) = profile_path {
    cmd.args(["--profile", &profile.display().to_string()]);
  }

  let output = cmd
    .current_dir(flake_ref)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .output()?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    color_eyre::eyre::bail!("nix print-dev-env failed:\n{stderr}");
  }

  DevEnv::from_nix_json(&output.stdout)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn generate_bash_content() {
    let dev_env = DevEnv {
      nix_paths: vec!["/nix/store/xxx-cargo-1.95.0/bin".into()],
      env_vars: HashMap::from([("FOO".into(), "bar".into())]),
      shell_hook: Some("echo hello".into()),
      env_hash: "test".into(),
      packages: vec![],
    };

    let content = generate_content(&dev_env, ShellType::Bash);
    assert!(content.contains("export PATH="));
    assert!(content.contains("/nix/store/xxx-cargo-1.95.0/bin"));
    assert!(content.contains("$PATH"));
    assert!(content.contains("export FOO='bar'"));
    assert!(content.contains("export IN_NIX_SHELL='impure'"));
    // Hook code no longer in env.sh — daemon runs hooks and writes hook-env.sh
    assert!(!content.contains("echo hello"));
  }

  #[test]
  fn generate_bash_content_has_depth_scoped_cleanup() {
    let dev_env = DevEnv {
      nix_paths: vec![],
      env_vars: HashMap::from([("FOO".into(), "bar".into())]),
      shell_hook: None,
      env_hash: "test".into(),
      packages: vec![],
    };

    let content = generate_content(&dev_env, ShellType::Bash);
    // Should resolve depth from $__XI_DEPTH
    assert!(content.contains("__xi_d=\"${__XI_DEPTH:-1}\""));
    // Should use depth-scoped registry
    assert!(content.contains("__XI_INJECTED_VARS_${__xi_d}"));
    // Should contain eval unset for cleanup
    assert!(content.contains("eval \"unset"));
    // Registry should use eval with depth
    assert!(
      content.contains("__XI_INJECTED_VARS_${__xi_d}='FOO IN_NIX_SHELL'")
    );
    // Temp vars should be cleaned up at the end
    assert!(content.contains("unset __xi_d __xi_keys"));
  }

  #[test]
  fn generate_fish_content_has_depth_scoped_cleanup() {
    let dev_env = DevEnv {
      nix_paths: vec![],
      env_vars: HashMap::from([("BAR".into(), "baz".into())]),
      shell_hook: None,
      env_hash: "test".into(),
      packages: vec![],
    };

    let content = generate_content(&dev_env, ShellType::Fish);
    // Should resolve depth
    assert!(content.contains("__XI_DEPTH"));
    // Should use depth-scoped registry
    assert!(content.contains("__XI_INJECTED_VARS_$__xi_d"));
    // Should unset via iterator
    assert!(content.contains("set -e $__xi_var"));
  }

  #[test]
  fn generate_fish_content() {
    let dev_env = DevEnv {
      nix_paths: vec!["/nix/store/xxx/bin".into()],
      env_vars: HashMap::new(),
      shell_hook: None,
      env_hash: "test".into(),
      packages: vec![],
    };

    let content = generate_content(&dev_env, ShellType::Fish);
    assert!(content.contains("set -gx PATH"));
    assert!(content.contains("$PATH"));
  }

  #[test]
  fn from_nix_json_preserves_non_store_paths() {
    let json = serde_json::json!({
      "variables": {
        "PATH": {
          "type": "exported",
          "value": "/nix/store/xxx-cargo/bin:/home/user/.nix-profile/bin:/nix/var/nix/profiles/default/bin"
        }
      }
    });
    let dev_env =
      DevEnv::from_nix_json(serde_json::to_vec(&json).unwrap().as_slice())
        .unwrap();
    assert_eq!(
      dev_env.nix_paths,
      vec![
        "/nix/store/xxx-cargo/bin",
        "/home/user/.nix-profile/bin",
        "/nix/var/nix/profiles/default/bin",
      ]
    );
  }
}
