//! Built-in backend using xi's native remote rebuild.
//!
//! This wraps the existing `xi os switch --target-host` mechanism as a
//! deployment backend.  It reads `nixosConfigurations` from the flake
//! and deploys via SSH using xi's remote infrastructure.
//!
//! This is the fallback backend when no deployment tool is detected.

use color_eyre::Result;
use color_eyre::eyre::bail;
use nix_command::{CommandKind, NixCommand};
use tracing::{debug, info};
use yansi::{Color, Paint};

use super::DeployBackend;
use crate::args::DeployArgs;
use crate::target::{DeployProfile, DeployTarget, ProfileKind};

/// Built-in deployment backend using xi's SSH remote rebuild.
pub struct BuiltinBackend;

impl DeployBackend for BuiltinBackend {
  fn name(&self) -> &'static str {
    "builtin"
  }

  fn discover_targets(&self, flake_ref: &str) -> Result<Vec<DeployTarget>> {
    discover_nixos_configurations(flake_ref)
  }

  fn deploy(&self, flake_ref: &str, args: &DeployArgs) -> Result<()> {
    let targets = self.discover_targets(flake_ref)?;

    if targets.is_empty() {
      bail!(
        "No nixosConfigurations found in {flake_ref}. \
         The builtin backend requires nixosConfigurations with \
         deployment.targetHost set, or use --target-host."
      );
    }

    println!();
    println!(
      "  {} {} configuration(s) via {}",
      Paint::new("Deploying").bold(),
      targets.len(),
      Paint::new("builtin").fg(Color::Cyan),
    );
    println!();

    for target in &targets {
      println!(
        "    {} → {}",
        Paint::new(&target.name).bold(),
        Paint::new(&target.hostname).fg(Color::Blue),
      );
    }
    println!();

    if args.dry {
      println!("  {}", Paint::new("Dry run — nothing deployed").dim());
      return Ok(());
    }

    // Filter targets
    let deploy_targets: Vec<&DeployTarget> = if args.targets.is_empty() {
      targets.iter().collect()
    } else {
      targets
        .iter()
        .filter(|t| args.targets.contains(&t.name))
        .collect()
    };

    if deploy_targets.is_empty() {
      let available: Vec<&str> =
        targets.iter().map(|t| t.name.as_str()).collect();
      bail!("No matching targets. Available: {}", available.join(", "));
    }

    for target in deploy_targets {
      info!("Deploying to {} ({})", target.name, target.hostname);

      // Build: nix build .#nixosConfigurations.<name>.config.system.build.toplevel
      let toplevel_attr = format!(
        "{flake_ref}#nixosConfigurations.{}.config.system.build.toplevel",
        target.name
      );

      info!("Building {}", toplevel_attr);
      let build_output = NixCommand::new(CommandKind::Build)
        .arg(&toplevel_attr)
        .arg("--no-link")
        .arg("--print-out-paths")
        .output()
        .map_err(|e| {
          color_eyre::eyre::eyre!("Failed to build {}: {e}", target.name)
        })?;

      if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        bail!("Build failed for {}:\n{stderr}", target.name);
      }

      let store_path = String::from_utf8_lossy(&build_output.stdout)
        .trim()
        .to_string();

      // Copy closure to target
      let ssh_target = format!(
        "{}@{}",
        target.ssh_user.as_deref().unwrap_or("root"),
        target.hostname
      );

      info!("Copying closure to {ssh_target}");
      let copy_output = NixCommand::new(CommandKind::Copy)
        .arg("--to")
        .arg(format!("ssh://{ssh_target}"))
        .arg(&store_path)
        .output()
        .map_err(|e| {
          color_eyre::eyre::eyre!("Failed to copy to {ssh_target}: {e}")
        })?;

      if !copy_output.status.success() {
        let stderr = String::from_utf8_lossy(&copy_output.stderr);
        bail!("Copy failed for {}:\n{stderr}", target.name);
      }

      // Activate: switch-to-configuration switch
      info!("Activating on {}", target.hostname);
      let activate_cmd = format!(
        "nix-env --profile /nix/var/nix/profiles/system --set {store_path} && \
         {store_path}/bin/switch-to-configuration switch"
      );

      let status = std::process::Command::new("ssh")
        .args(&target.ssh_opts)
        .arg(&ssh_target)
        .arg("--")
        .arg(&activate_cmd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| {
          color_eyre::eyre::eyre!(
            "SSH activation failed for {}: {e}",
            target.name
          )
        })?;

      if !status.success() {
        bail!("Activation failed for {}", target.name);
      }

      println!(
        "    {} {}",
        Paint::new(&target.name).bold(),
        Paint::new("ok").fg(Color::Green),
      );
    }

    println!();
    println!(
      "  {}",
      Paint::new("All targets deployed successfully")
        .fg(Color::Green)
        .bold()
    );
    println!();

    Ok(())
  }
}

/// Discover NixOS configurations from the flake's `nixosConfigurations` output.
///
/// Each configuration becomes a deploy target.  Without a way to know the
/// target hostname from pure Nix evaluation, we use the configuration
/// attribute name as the hostname (users can override via the target list).
fn discover_nixos_configurations(flake_ref: &str) -> Result<Vec<DeployTarget>> {
  let attr = format!("{flake_ref}#nixosConfigurations");
  debug!(attr, "Discovering nixosConfigurations");

  let cmd = NixCommand::new(CommandKind::Eval)
    .arg(&attr)
    .arg("--apply")
    .arg("x: builtins.attrNames x")
    .arg("--json");

  let output = cmd
    .output()
    .map_err(|e| color_eyre::eyre::eyre!("Failed to evaluate {attr}: {e}"))?;

  if !output.status.success() {
    return Ok(Vec::new());
  }

  let names: Vec<String> =
    serde_json::from_slice(&output.stdout).map_err(|e| {
      color_eyre::eyre::eyre!("Failed to parse nixosConfigurations: {e}")
    })?;

  Ok(
    names
      .into_iter()
      .map(|name| {
        let hostname = name.clone();
        DeployTarget {
          name: name.clone(),
          hostname,
          profiles: vec![DeployProfile {
            name: "system".to_string(),
            path: format!(
              "{flake_ref}#nixosConfigurations.{name}.config.system.build.toplevel"
            ),
            user: Some("root".to_string()),
            kind: ProfileKind::NixOS,
            profile_path: None,
          }],
          ssh_user: Some("root".to_string()),
          ssh_opts: Vec::new(),
          tags: Vec::new(),
          magic_rollback: false,
          confirm_timeout: 30,
        }
      })
      .collect(),
  )
}
