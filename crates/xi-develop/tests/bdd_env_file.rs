//! BDD: tests/features/03_live_update_package.feature (env file content)
//! BDD: tests/features/04_live_update_hook.feature (hook sourcing)
//! BDD: tests/features/19_shell_uniformity.feature (shell syntax)
//!
//! Tests for env file generation, cleanup preamble, and injected vars registry.

use xi_develop::env_file::DevEnv;
use xi_develop::shell::ShellType;

fn make_env(
  paths: Vec<&str>,
  vars: Vec<(&str, &str)>,
  hook: Option<&str>,
) -> DevEnv {
  DevEnv {
    nix_paths: paths.into_iter().map(String::from).collect(),
    env_vars: vars
      .into_iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect(),
    shell_hook: hook.map(String::from),
    env_hash: "test".into(),
    packages: vec![],
  }
}

/// BDD: 19_shell_uniformity.feature#Env file syntax matches shell (bash/zsh)
#[test]
fn bash_env_file_uses_export_syntax() {
  let env =
    make_env(vec!["/nix/store/xxx-cargo/bin"], vec![("FOO", "bar")], None);

  let tmp = tempfile::tempdir().unwrap();
  xi_develop::env_file::write_env_file(
    tmp.path(),
    &env,
    ShellType::Bash,
    "default",
  )
  .unwrap();

  let content =
    std::fs::read_to_string(tmp.path().join("env-default.sh")).unwrap();

  assert!(content.contains("export PATH="));
  assert!(content.contains("/nix/store/xxx-cargo/bin"));
  assert!(content.contains("$PATH"));
  assert!(content.contains("export FOO='bar'"));
  assert!(content.contains("export IN_NIX_SHELL='impure'"));
}

/// BDD: 19_shell_uniformity.feature#Env file syntax matches shell (fish)
#[test]
fn fish_env_file_uses_set_syntax() {
  let env = make_env(vec!["/nix/store/xxx/bin"], vec![("FOO", "bar")], None);

  let tmp = tempfile::tempdir().unwrap();
  xi_develop::env_file::write_env_file(
    tmp.path(),
    &env,
    ShellType::Fish,
    "default",
  )
  .unwrap();

  let content =
    std::fs::read_to_string(tmp.path().join("env-default.fish")).unwrap();

  assert!(content.contains("set -gx PATH"));
  assert!(content.contains("$PATH"));
  assert!(content.contains("set -gx FOO 'bar'"));
}

/// BDD: 03_live_update_package.feature#Package removed triggers re-source with cleanup
#[test]
fn cleanup_preamble_present_in_bash() {
  let env = make_env(vec![], vec![("FOO", "bar")], None);

  let tmp = tempfile::tempdir().unwrap();
  xi_develop::env_file::write_env_file(
    tmp.path(),
    &env,
    ShellType::Bash,
    "default",
  )
  .unwrap();

  let content =
    std::fs::read_to_string(tmp.path().join("env-default.sh")).unwrap();

  // Depth-scoped cleanup preamble should be present
  assert!(content.contains("__xi_d=\"${__XI_DEPTH:-1}\""));
  assert!(content.contains("__XI_INJECTED_VARS_${__xi_d}"));
  assert!(content.contains("eval \"unset"));
}

/// BDD: 03_live_update_package.feature (injected vars registry)
#[test]
fn injected_vars_registry_tracks_exports() {
  let env = make_env(vec![], vec![("FOO", "bar"), ("BAZ", "qux")], None);

  let tmp = tempfile::tempdir().unwrap();
  xi_develop::env_file::write_env_file(
    tmp.path(),
    &env,
    ShellType::Bash,
    "default",
  )
  .unwrap();

  let content =
    std::fs::read_to_string(tmp.path().join("env-default.sh")).unwrap();

  // Depth-scoped registry should list all injected vars (sorted)
  assert!(content.contains("__XI_INJECTED_VARS_${__xi_d}"));
  assert!(content.contains("BAZ"));
  assert!(content.contains("FOO"));
  assert!(content.contains("IN_NIX_SHELL"));
}

/// BDD: 19_shell_uniformity.feature (fish cleanup preamble)
#[test]
fn cleanup_preamble_present_in_fish() {
  let env = make_env(vec![], vec![("BAR", "baz")], None);

  let tmp = tempfile::tempdir().unwrap();
  xi_develop::env_file::write_env_file(
    tmp.path(),
    &env,
    ShellType::Fish,
    "default",
  )
  .unwrap();

  let content =
    std::fs::read_to_string(tmp.path().join("env-default.fish")).unwrap();

  assert!(content.contains("set -e $__xi_var"));
  assert!(content.contains("__XI_INJECTED_VARS_$__xi_d"));
}

/// BDD: 03_live_update_package.feature (shell hook NOT in env file)
#[test]
fn shell_hook_not_in_env_file() {
  let env = make_env(vec!["/nix/store/xxx/bin"], vec![], Some("echo hello"));

  let tmp = tempfile::tempdir().unwrap();
  xi_develop::env_file::write_env_file(
    tmp.path(),
    &env,
    ShellType::Bash,
    "default",
  )
  .unwrap();

  let content =
    std::fs::read_to_string(tmp.path().join("env-default.sh")).unwrap();

  // Hook content should NOT be in the env file
  // (daemon writes it to a separate hook file)
  assert!(!content.contains("echo hello"));
}

/// BDD: env vars that should be skipped (SKIP_VARS)
#[test]
fn skip_vars_not_exported() {
  let json = serde_json::json!({
      "variables": {
          "PATH": { "type": "exported", "value": "/nix/store/xxx/bin" },
          "HOME": { "type": "exported", "value": "/home/test" },
          "USER": { "type": "exported", "value": "test" },
          "SHELL": { "type": "exported", "value": "/bin/bash" },
          "MY_CUSTOM_VAR": { "type": "exported", "value": "keep-me" }
      }
  });

  let env =
    DevEnv::from_nix_json(serde_json::to_vec(&json).unwrap().as_slice())
      .unwrap();

  // HOME, USER, SHELL should be skipped
  assert!(!env.env_vars.contains_key("HOME"));
  assert!(!env.env_vars.contains_key("USER"));
  assert!(!env.env_vars.contains_key("SHELL"));
  // Custom vars should be kept
  assert!(env.env_vars.contains_key("MY_CUSTOM_VAR"));
}
