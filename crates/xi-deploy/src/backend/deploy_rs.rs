//! deploy-rs backend.
//!
//! Reads the `deploy` flake output, parses nodes and profiles, and
//! deploys using xi's SSH infrastructure.  This is a native Rust
//! implementation that does not shell out to the `deploy` CLI.

use color_eyre::Result;
use color_eyre::eyre::bail;
use nix_command::{CommandKind, NixCommand};
use tracing::{debug, info};
use yansi::{Color, Paint};

use super::DeployBackend;
use crate::args::DeployArgs;
use crate::target::{DeployRsConfig, DeployTarget};

/// Native deploy-rs backend.
pub struct DeployRsBackend {
  config: DeployRsConfig,
}

impl DeployRsBackend {
  /// Try to create a deploy-rs backend by evaluating `<flake>#deploy`.
  ///
  /// Returns `Err` if the flake has no `deploy` output.
  pub fn new(flake_ref: &str) -> Result<Self> {
    let config = eval_deploy_config(flake_ref)?;
    Ok(Self { config })
  }
}

impl DeployBackend for DeployRsBackend {
  fn name(&self) -> &'static str {
    "deploy-rs"
  }

  fn discover_targets(&self, _flake_ref: &str) -> Result<Vec<DeployTarget>> {
    // Clone config to convert — in practice we'd cache this
    let config_json =
      serde_json::to_value(&self.config).map_err(|e| {
        color_eyre::eyre::eyre!("Failed to re-serialize deploy config: {e}")
      })?;
    let config: DeployRsConfig = serde_json::from_value(config_json)?;
    Ok(config.into_targets())
  }

  fn deploy(&self, flake_ref: &str, args: &DeployArgs) -> Result<()> {
    let all_targets = self.discover_targets(flake_ref)?;

    if all_targets.is_empty() {
      bail!(
        "No deployment nodes found in {flake_ref}#deploy. \
         Add nodes to your deploy configuration."
      );
    }

    // Filter targets by name if specified
    let targets = if args.targets.is_empty() {
      all_targets
    } else {
      let filtered: Vec<DeployTarget> = all_targets
        .into_iter()
        .filter(|t| args.targets.iter().any(|name| name == &t.name))
        .collect();

      if filtered.is_empty() {
        let available: Vec<&str> = self
          .config
          .nodes
          .keys()
          .map(String::as_str)
          .collect();
        bail!(
          "No matching nodes found. Available: {}",
          available.join(", ")
        );
      }
      filtered
    };

    println!();
    println!(
      "  {} {} node(s) via {}",
      Paint::new("Deploying").bold(),
      targets.len(),
      Paint::new("deploy-rs").fg(Color::Cyan),
    );
    println!();

    // Print deployment plan
    for target in &targets {
      let profiles: Vec<&str> =
        target.profiles.iter().map(|p| p.name.as_str()).collect();
      println!(
        "    {} → {} ({})",
        Paint::new(&target.name).bold(),
        Paint::new(&target.hostname).fg(Color::Blue),
        profiles.join(", "),
      );
    }
    println!();

    if args.dry {
      println!(
        "  {}",
        Paint::new("Dry run — nothing deployed").dim()
      );
      return Ok(());
    }

    // Deploy each target sequentially
    for target in &targets {
      deploy_target(flake_ref, target, args)?;
    }

    println!(
      "  {}",
      Paint::new("All nodes deployed successfully")
        .fg(Color::Green)
        .bold()
    );
    println!();

    Ok(())
  }
}

/// Evaluate `<flake>#deploy` and parse the JSON config.
fn eval_deploy_config(flake_ref: &str) -> Result<DeployRsConfig> {
  let attr = format!("{flake_ref}#deploy");
  debug!(attr, "Evaluating deploy-rs config");

  let cmd = NixCommand::new(CommandKind::Eval)
    .arg(&attr)
    .arg("--json");

  let output = cmd.output().map_err(|e| {
    color_eyre::eyre::eyre!("Failed to evaluate {attr}: {e}")
  })?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("No deploy output found (nix eval {attr} failed):\n{stderr}");
  }

  let config: DeployRsConfig =
    serde_json::from_slice(&output.stdout).map_err(|e| {
      color_eyre::eyre::eyre!(
        "Failed to parse deploy-rs config from {attr}: {e}"
      )
    })?;

  debug!("Found {} node(s)", config.nodes.len());
  Ok(config)
}

/// Deploy a single target: build → push → activate for each profile.
fn deploy_target(
  flake_ref: &str,
  target: &DeployTarget,
  args: &DeployArgs,
) -> Result<()> {
  info!("Deploying to {} ({})", target.name, target.hostname);

  for profile in &target.profiles {
    deploy_profile(flake_ref, target, profile, args)?;
  }

  Ok(())
}

/// Deploy a single profile on a target.
fn deploy_profile(
  _flake_ref: &str,
  target: &DeployTarget,
  profile: &crate::target::DeployProfile,
  args: &DeployArgs,
) -> Result<()> {
  let profile_display = format!("{}.{}", target.name, profile.name);
  info!("Building profile {profile_display}");

  // Phase 1: Build the profile derivation
  let mut build_cmd = NixCommand::new(CommandKind::Build)
    .arg(&profile.path)
    .arg("--no-link");

  if args.show_trace {
    build_cmd = build_cmd.arg("--show-trace");
  }

  let output = build_cmd.output().map_err(|e| {
    color_eyre::eyre::eyre!("Failed to build {profile_display}: {e}")
  })?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("Build failed for {profile_display}:\n{stderr}");
  }

  // Phase 2: Copy closure to target
  info!(
    "Copying closure to {}@{}",
    target.ssh_user.as_deref().unwrap_or("root"),
    target.hostname
  );

  let ssh_target = format!(
    "{}@{}",
    target.ssh_user.as_deref().unwrap_or("root"),
    target.hostname
  );

  let mut copy_cmd = NixCommand::new(CommandKind::Copy)
    .arg("--to")
    .arg(format!("ssh://{ssh_target}"))
    .arg(&profile.path);

  if !target.ssh_opts.is_empty() {
    let ssh_opts_str = target.ssh_opts.join(" ");
    copy_cmd = copy_cmd
      .arg("--option")
      .arg("ssh-opts")
      .arg(&ssh_opts_str);
  }

  let copy_output = copy_cmd.output().map_err(|e| {
    color_eyre::eyre::eyre!("Failed to copy to {ssh_target}: {e}")
  })?;

  if !copy_output.status.success() {
    let stderr = String::from_utf8_lossy(&copy_output.stderr);
    bail!("Copy failed for {profile_display}:\n{stderr}");
  }

  // Phase 3: Activate on target via SSH
  info!("Activating {profile_display}");

  let activate_script = format!("{}/deploy-rs-activate", profile.path);
  let ssh_cmd = build_ssh_activate_command(
    target,
    &activate_script,
    profile.user.as_deref(),
  );

  let activate_status = std::process::Command::new("ssh")
    .args(&target.ssh_opts)
    .arg(&ssh_target)
    .arg("--")
    .arg(&ssh_cmd)
    .stdin(std::process::Stdio::inherit())
    .stdout(std::process::Stdio::inherit())
    .stderr(std::process::Stdio::inherit())
    .status()
    .map_err(|e| {
      color_eyre::eyre::eyre!("SSH activation failed for {profile_display}: {e}")
    })?;

  if !activate_status.success() {
    bail!("Activation failed for {profile_display}");
  }

  println!(
    "    {} {}",
    Paint::new(&profile_display).bold(),
    Paint::new("ok").fg(Color::Green),
  );

  Ok(())
}

/// Build the SSH command string for activation.
fn build_ssh_activate_command(
  _target: &DeployTarget,
  activate_script: &str,
  user: Option<&str>,
) -> String {
  let user_part = user.map_or_else(String::new, |u| format!("sudo -u {u} "));
  format!("{user_part}{activate_script}")
}
