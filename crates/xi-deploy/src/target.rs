//! Unified deployment target model.
//!
//! All backends translate their native configuration into this common
//! model so the CLI can display consistent information regardless of
//! which tool is used.

use serde::Deserialize;

/// A single deployment target (machine/node).
#[derive(Debug, Clone)]
pub struct DeployTarget {
  /// Node name (flake attribute key).
  pub name: String,
  /// SSH hostname / IP address.
  pub hostname: String,
  /// Profiles to deploy on this target.
  pub profiles: Vec<DeployProfile>,
  /// SSH connection user (overridable per-profile).
  pub ssh_user: Option<String>,
  /// Extra SSH options (e.g. `["-p", "2222"]`).
  pub ssh_opts: Vec<String>,
  /// Tags for filtering (colmena-style).
  pub tags: Vec<String>,
  /// Whether to use magic rollback (deploy-rs).
  pub magic_rollback: bool,
  /// Seconds to wait for confirmation before rollback.
  pub confirm_timeout: u64,
}

/// A single profile within a deployment target.
#[derive(Debug, Clone)]
pub struct DeployProfile {
  /// Profile name (e.g. "system", "home", custom name).
  pub name: String,
  /// Nix store path of the built profile, or flake attribute to build.
  pub path: String,
  /// User to activate as (may differ from SSH user → triggers sudo).
  pub user: Option<String>,
  /// What kind of activation to perform.
  pub kind: ProfileKind,
  /// Custom nix profile install location.
  pub profile_path: Option<String>,
}

/// What type of activation a profile uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileKind {
  /// NixOS system activation (`switch-to-configuration switch`).
  NixOS,
  /// Home Manager activation (`activate`).
  HomeManager,
  /// nix-darwin activation (`darwin-rebuild activate`).
  Darwin,
  /// Custom activation command.
  Custom { activate_command: String },
  /// No activation (just install the closure).
  Noop,
}

// ---------------------------------------------------------------------------
// deploy-rs JSON deserialization types
// ---------------------------------------------------------------------------

/// Top-level `deploy` flake output as parsed from `nix eval .#deploy --json`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DeployRsConfig {
  #[serde(default)]
  pub nodes: std::collections::BTreeMap<String, DeployRsNode>,
  #[serde(default, rename = "sshUser")]
  pub ssh_user: Option<String>,
  #[serde(default, rename = "sshOpts")]
  pub ssh_opts: Vec<String>,
  #[serde(default = "default_true", rename = "autoRollback")]
  pub auto_rollback: bool,
  #[serde(default = "default_true", rename = "magicRollback")]
  pub magic_rollback: bool,
  #[serde(default = "default_confirm_timeout", rename = "confirmTimeout")]
  pub confirm_timeout: u64,
}

/// A single node in deploy-rs config.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DeployRsNode {
  pub hostname: String,
  #[serde(default)]
  pub profiles: std::collections::BTreeMap<String, DeployRsProfile>,
  #[serde(default, rename = "sshUser")]
  pub ssh_user: Option<String>,
  #[serde(default, rename = "sshOpts")]
  pub ssh_opts: Vec<String>,
  #[serde(default, rename = "profilesOrder")]
  pub profiles_order: Vec<String>,
  #[serde(default = "default_true", rename = "magicRollback")]
  pub magic_rollback: bool,
  #[serde(default = "default_confirm_timeout", rename = "confirmTimeout")]
  pub confirm_timeout: u64,
}

/// A single profile in deploy-rs config.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct DeployRsProfile {
  pub path: String,
  #[serde(default)]
  pub user: Option<String>,
  #[serde(default, rename = "profilePath")]
  pub profile_path: Option<String>,
  #[serde(default = "default_true", rename = "magicRollback")]
  pub magic_rollback: bool,
}

const fn default_true() -> bool {
  true
}

const fn default_confirm_timeout() -> u64 {
  30
}

impl DeployRsConfig {
  /// Convert deploy-rs config into unified [`DeployTarget`]s.
  #[must_use]
  pub fn into_targets(self) -> Vec<DeployTarget> {
    let global_ssh_user = self.ssh_user;
    let global_ssh_opts = self.ssh_opts;
    let global_magic_rollback = self.magic_rollback;
    let global_confirm_timeout = self.confirm_timeout;

    self
      .nodes
      .into_iter()
      .map(|(name, node)| {
        let ssh_user = node.ssh_user.or_else(|| global_ssh_user.clone());
        let ssh_opts = if node.ssh_opts.is_empty() {
          global_ssh_opts.clone()
        } else {
          node.ssh_opts
        };
        let magic_rollback = node.magic_rollback && global_magic_rollback;
        let confirm_timeout = node.confirm_timeout.min(global_confirm_timeout);

        // Order profiles: explicit order first, then remaining
        let mut ordered_profiles = Vec::new();
        for ordered_name in &node.profiles_order {
          if let Some((_key, profile)) =
            node.profiles.iter().find(|(k, _)| *k == ordered_name)
          {
            ordered_profiles.push((ordered_name.clone(), profile.clone()));
          }
        }
        for (pname, profile) in &node.profiles {
          if !node.profiles_order.contains(pname) {
            ordered_profiles.push((pname.clone(), profile.clone()));
          }
        }

        let profiles = ordered_profiles
          .into_iter()
          .map(|(pname, profile)| {
            let kind = infer_profile_kind(&pname);
            DeployProfile {
              name: pname,
              path: profile.path.clone(),
              user: profile.user.clone(),
              kind,
              profile_path: profile.profile_path.clone(),
            }
          })
          .collect();

        DeployTarget {
          name,
          hostname: node.hostname,
          profiles,
          ssh_user,
          ssh_opts,
          tags: Vec::new(), // deploy-rs has no tags
          magic_rollback,
          confirm_timeout,
        }
      })
      .collect()
  }
}

/// Infer the activation kind from a profile name.
fn infer_profile_kind(name: &str) -> ProfileKind {
  match name {
    "system" => ProfileKind::NixOS,
    "home" | "home-manager" => ProfileKind::HomeManager,
    "darwin" => ProfileKind::Darwin,
    _ => ProfileKind::NixOS, // deploy-rs default
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn deserialize_deploy_rs_config() {
    let json = serde_json::json!({
      "sshUser": "deploy",
      "nodes": {
        "web1": {
          "hostname": "10.0.0.1",
          "profiles": {
            "system": {
              "path": "/nix/store/abc-system",
              "user": "root"
            }
          }
        },
        "web2": {
          "hostname": "10.0.0.2",
          "sshUser": "admin",
          "profilesOrder": ["system"],
          "profiles": {
            "system": {
              "path": "/nix/store/def-system"
            },
            "app": {
              "path": "/nix/store/ghi-app",
              "user": "app",
              "profilePath": "/nix/var/nix/profiles/per-user/app/my-app"
            }
          }
        }
      }
    });

    let config: DeployRsConfig =
      serde_json::from_value(json).expect("valid config");

    assert_eq!(config.nodes.len(), 2);
    assert_eq!(config.ssh_user.as_deref(), Some("deploy"));

    let targets = config.into_targets();
    assert_eq!(targets.len(), 2);

    let web1 = targets.iter().find(|t| t.name == "web1").expect("web1");
    assert_eq!(web1.hostname, "10.0.0.1");
    assert_eq!(web1.ssh_user.as_deref(), Some("deploy"));
    assert_eq!(web1.profiles.len(), 1);
    assert_eq!(web1.profiles[0].name, "system");
    assert_eq!(web1.profiles[0].kind, ProfileKind::NixOS);

    let web2 = targets.iter().find(|t| t.name == "web2").expect("web2");
    assert_eq!(web2.hostname, "10.0.0.2");
    assert_eq!(web2.ssh_user.as_deref(), Some("admin"));
    // profiles_order puts "system" first
    assert_eq!(web2.profiles[0].name, "system");
    assert_eq!(web2.profiles[1].name, "app");
    assert!(web2.profiles[1].profile_path.is_some());
  }

  #[test]
  fn infer_profile_kinds() {
    assert_eq!(infer_profile_kind("system"), ProfileKind::NixOS);
    assert_eq!(infer_profile_kind("home"), ProfileKind::HomeManager);
    assert_eq!(infer_profile_kind("home-manager"), ProfileKind::HomeManager);
    assert_eq!(infer_profile_kind("darwin"), ProfileKind::Darwin);
    assert_eq!(infer_profile_kind("my-app"), ProfileKind::NixOS);
  }
}
