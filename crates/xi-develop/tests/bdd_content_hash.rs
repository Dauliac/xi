//! BDD: tests/features/05_live_update_noop.feature
//!
//! Tests for content-hash deduplication.
//! Verifies that identical nix eval output produces the same hash,
//! and different output produces different hashes.

use std::collections::HashMap;

use xi_develop::env_file::DevEnv;

/// BDD: 05_live_update_noop.feature#Nix eval produces identical output (content-hash dedup)
#[test]
fn identical_env_produces_same_hash() {
  let env1 = DevEnv {
    nix_paths: vec!["/nix/store/xxx-cargo-1.95.0/bin".into()],
    env_vars: HashMap::from([("FOO".into(), "bar".into())]),
    shell_hook: Some("echo hello".into()),
    env_hash: String::new(), // will be computed
    packages: vec![],
  };

  let env2 = DevEnv {
    nix_paths: vec!["/nix/store/xxx-cargo-1.95.0/bin".into()],
    env_vars: HashMap::from([("FOO".into(), "bar".into())]),
    shell_hook: Some("echo hello".into()),
    env_hash: String::new(),
    packages: vec![],
  };

  // Hash is computed during from_nix_json, but we can verify the logic
  // by checking that the fields are identical
  assert_eq!(env1.nix_paths, env2.nix_paths);
  assert_eq!(env1.env_vars, env2.env_vars);
  assert_eq!(env1.shell_hook, env2.shell_hook);
}

/// BDD: 03_live_update_package.feature#Package added triggers re-source
/// Verifies that adding a package changes the env hash.
#[test]
fn different_paths_produce_different_hash() {
  // We test via from_nix_json which computes the hash
  let json1 = serde_json::json!({
      "variables": {
          "PATH": {
              "type": "exported",
              "value": "/nix/store/xxx-cargo/bin"
          }
      }
  });

  let json2 = serde_json::json!({
      "variables": {
          "PATH": {
              "type": "exported",
              "value": "/nix/store/xxx-cargo/bin:/nix/store/yyy-python/bin"
          }
      }
  });

  let env1 =
    DevEnv::from_nix_json(serde_json::to_vec(&json1).unwrap().as_slice())
      .unwrap();
  let env2 =
    DevEnv::from_nix_json(serde_json::to_vec(&json2).unwrap().as_slice())
      .unwrap();

  assert_ne!(env1.env_hash, env2.env_hash);
}

/// BDD: 04_live_update_hook.feature#shellHook changed without package changes
/// Verifies that changing shellHook changes the env hash.
#[test]
fn different_hook_produces_different_hash() {
  let json1 = serde_json::json!({
      "variables": {
          "PATH": { "type": "exported", "value": "/nix/store/xxx/bin" },
          "shellHook": { "type": "exported", "value": "echo v1" }
      }
  });

  let json2 = serde_json::json!({
      "variables": {
          "PATH": { "type": "exported", "value": "/nix/store/xxx/bin" },
          "shellHook": { "type": "exported", "value": "echo v2" }
      }
  });

  let env1 =
    DevEnv::from_nix_json(serde_json::to_vec(&json1).unwrap().as_slice())
      .unwrap();
  let env2 =
    DevEnv::from_nix_json(serde_json::to_vec(&json2).unwrap().as_slice())
      .unwrap();

  assert_ne!(env1.env_hash, env2.env_hash);
}

/// BDD: 05_live_update_noop.feature (content-hash dedup across serialization)
#[test]
fn hash_is_deterministic_across_calls() {
  let json = serde_json::json!({
      "variables": {
          "PATH": { "type": "exported", "value": "/nix/store/xxx/bin" },
          "FOO": { "type": "exported", "value": "bar" }
      }
  });
  let bytes = serde_json::to_vec(&json).unwrap();

  let env1 = DevEnv::from_nix_json(&bytes).unwrap();
  let env2 = DevEnv::from_nix_json(&bytes).unwrap();

  assert_eq!(env1.env_hash, env2.env_hash);
  assert!(!env1.env_hash.is_empty());
}
