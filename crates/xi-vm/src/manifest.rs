//! The `xi-vm-manifest` JSON built by `modules/flake-parts/vm.nix`
//! (via `modules/shared/lib/vm.nix`'s `mkVmPackages`).
//!
//! Display/runtime metadata only - the heavy build/check derivations
//! themselves are resolved by [`crate::attrs`]'s naming convention and
//! fetched directly by [`crate::build`], not through this file.

use std::{collections::BTreeMap, path::PathBuf};

use color_eyre::{Result, eyre::bail};
use nix_command::{CommandKind, NixCommand};
use serde::Deserialize;

use crate::attrs;

/// Which backend builds and runs a declared VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VmKind {
  Nixos,
  Distro,
}

/// Default display mode for `xi vm start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Display {
  Graphical,
  Headless,
}

/// One declared app port forward.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForward {
  pub guest_port: u16,
  pub host_port:  Option<u16>,
  pub protocol:   String,
}

/// One `xi.vm.<name>` entry's display/runtime metadata.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmManifestEntry {
  pub kind:            VmKind,
  pub display:         Display,
  pub with_bootloader: bool,
  pub ssh_user:        String,
  pub ports:           Vec<PortForward>,
  pub distro:          Option<String>,
  pub version:         Option<String>,
}

/// Every declared VM, keyed by name.
pub type Manifest = BTreeMap<String, VmManifestEntry>;

/// Parse a manifest JSON document already read from disk.
///
/// Split from [`fetch`] so manifest-shape tests don't need a working
/// `nix` binary.
///
/// # Errors
///
/// Returns an error if `raw` is not a valid manifest document.
pub fn parse(raw: &str) -> Result<Manifest> {
  serde_json::from_str(raw)
    .map_err(|e| color_eyre::eyre::eyre!("invalid xi-vm-manifest.json: {e}"))
}

/// Build `<flakeref>#xi-vm-manifest` and parse it.
///
/// # Errors
///
/// Returns an error if the build fails (most commonly: the consuming
/// flake never wired `config.lib.mkVmPackages` into its own `packages`,
/// see `modules/flake-parts/vm.nix`'s `xi.vm` option doc) or if the
/// resulting file is not valid JSON.
pub fn fetch(flakeref: &str) -> Result<Manifest> {
  let installable = format!("{flakeref}#{}", attrs::MANIFEST);
  let path = build_and_resolve_path(&installable)?;
  let raw = std::fs::read_to_string(&path).map_err(|e| {
    color_eyre::eyre::eyre!("failed to read {}: {e}", path.display())
  })?;
  parse(&raw)
}

/// Build `installable` and return its realised store path.
///
/// Mirrors `xi-flake`'s `locate::build_and_resolve_path`: build once
/// (so build failures surface with full logs), then query
/// `--print-out-paths` (instant - the derivation is already cached).
///
/// # Errors
///
/// Returns an error if the build fails or produces no output path.
pub fn build_and_resolve_path(installable: &str) -> Result<PathBuf> {
  let build = NixCommand::new(CommandKind::Build)
    .print_build_logs(false)
    .arg(installable)
    .arg("--no-link");

  let status = build
    .run_with_logs()
    .map_err(|e| color_eyre::eyre::eyre!("failed to run nix build: {e}"))?;
  if !status.success() {
    bail!("nix build {installable} failed");
  }

  let query = NixCommand::new(CommandKind::Build)
    .print_build_logs(false)
    .arg(installable)
    .arg("--no-link")
    .arg("--print-out-paths");

  let output = query
    .output()
    .map_err(|e| color_eyre::eyre::eyre!("failed to resolve store path: {e}"))?;
  if !output.status.success() {
    bail!(
      "nix build --print-out-paths failed:\n{}",
      String::from_utf8_lossy(&output.stderr)
    );
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  let store_path = stdout
    .lines()
    .next()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .ok_or_else(|| color_eyre::eyre::eyre!("nix build produced no output"))?;

  Ok(PathBuf::from(store_path))
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn parses_a_nixos_entry_with_an_app_port() {
    let raw = r#"{
      "gary": {
        "kind": "nixos",
        "display": "graphical",
        "withBootloader": false,
        "sshUser": "xi",
        "ports": [{"guestPort": 8080, "hostPort": 8080, "protocol": "tcp"}],
        "distro": null,
        "version": null
      }
    }"#;
    let manifest = parse(raw).expect("parse");
    let gary = manifest.get("gary").expect("gary entry");
    assert_eq!(gary.kind, VmKind::Nixos);
    assert_eq!(gary.display, Display::Graphical);
    assert!(!gary.with_bootloader);
    assert_eq!(gary.ssh_user, "xi");
    assert_eq!(gary.ports.len(), 1);
    assert_eq!(gary.ports[0].guest_port, 8080);
    assert_eq!(gary.ports[0].host_port, Some(8080));
  }

  #[test]
  fn parses_a_distro_entry_with_no_ports() {
    let raw = r#"{
      "patrick": {
        "kind": "distro",
        "display": "headless",
        "withBootloader": false,
        "sshUser": "xi",
        "ports": [],
        "distro": "debian",
        "version": "13"
      }
    }"#;
    let manifest = parse(raw).expect("parse");
    let patrick = manifest.get("patrick").expect("patrick entry");
    assert_eq!(patrick.kind, VmKind::Distro);
    assert_eq!(patrick.distro, Some("debian".to_owned()));
    assert_eq!(patrick.version, Some("13".to_owned()));
    assert!(patrick.ports.is_empty());
  }

  #[test]
  fn an_empty_manifest_parses_to_an_empty_map() {
    let manifest = parse("{}").expect("parse");
    assert!(manifest.is_empty());
  }

  #[test]
  fn malformed_json_is_a_clear_error() {
    let err = parse("not json").expect_err("must fail");
    assert!(err.to_string().contains("xi-vm-manifest.json"));
  }
}
