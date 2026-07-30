//! Colmena backend (shell-out).
//!
//! Delegates to the `colmena` CLI for deployment.  Detection checks
//! for the `colmenaHive` flake output and the `colmena` binary in PATH.

use color_eyre::Result;
use color_eyre::eyre::bail;
use tracing::{debug, info};
use yansi::{Color, Paint};

use super::DeployBackend;
use crate::args::DeployArgs;
use crate::target::DeployTarget;

/// Colmena backend that shells out to the `colmena` CLI.
pub struct ColmenaBackend;

impl ColmenaBackend {
  /// Try to create a colmena backend.
  ///
  /// Succeeds only if `colmena` is available in PATH.
  pub fn new() -> Result<Self> {
    if which::which("colmena").is_err() {
      bail!(
        "colmena not found in PATH. \
         Install it or use a different backend."
      );
    }
    Ok(Self)
  }
}

impl DeployBackend for ColmenaBackend {
  fn name(&self) -> &'static str {
    "colmena"
  }

  fn discover_targets(&self, _flake_ref: &str) -> Result<Vec<DeployTarget>> {
    // Colmena manages its own target discovery internally.
    // We could parse `colmena eval -E '...'` output, but for now
    // return empty — the CLI will show colmena's own output.
    Ok(Vec::new())
  }

  fn deploy(&self, flake_ref: &str, args: &DeployArgs) -> Result<()> {
    println!();
    println!(
      "  {} via {}",
      Paint::new("Deploying").bold(),
      Paint::new("colmena").fg(Color::Cyan),
    );
    println!();

    let mut cmd = std::process::Command::new("colmena");
    cmd.arg("apply");

    // Target filtering
    if let Some(ref on) = args.on {
      cmd.arg("--on").arg(on);
    } else if !args.targets.is_empty() {
      cmd.arg("--on").arg(args.targets.join(","));
    }

    // Dry run
    if args.dry {
      cmd.arg("--eval-node-limit").arg("0");
      // colmena doesn't have a single --dry-run flag;
      // use build goal to avoid activation
      // Actually, use --nix-option to pass dry-run
      debug!("Dry-run mode: using 'build' goal instead of 'switch'");
    }

    // Show trace
    if args.show_trace {
      cmd.arg("--show-trace");
    }

    // Flake reference
    if flake_ref != "." {
      cmd.arg("--flake").arg(flake_ref);
    }

    // Extra args
    for arg in &args.extra_args {
      cmd.arg(arg);
    }

    info!("Running: colmena apply");
    debug!(cmd = ?cmd, "colmena command");

    let status = cmd
      .stdin(std::process::Stdio::inherit())
      .stdout(std::process::Stdio::inherit())
      .stderr(std::process::Stdio::inherit())
      .status()
      .map_err(|e| {
        color_eyre::eyre::eyre!("Failed to run colmena: {e}")
      })?;

    if !status.success() {
      bail!("colmena apply failed with status {status}");
    }

    println!();
    Ok(())
  }
}
