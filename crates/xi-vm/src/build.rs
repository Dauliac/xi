//! `xi vm build` / `xi vm check`: build the derivations
//! `modules/flake-parts/vm.nix` renders for one or every declared VM.

use std::path::PathBuf;

use color_eyre::Result;
use tracing::{error, info};

use crate::{
  attrs,
  manifest::{Manifest, VmKind},
};

/// Which variant of the `kind = "nixos"` build to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildVariant {
  /// The persistent, bootable image `start`/`connect` use.
  Dev,
  /// The `runNixOSTest` derivation `check` builds and runs.
  Test,
}

/// Build one VM's `--dev` (or `--test`) artifact.
///
/// # Errors
///
/// Returns an error if the VM is unknown, is `kind = "distro"` (no
/// `--dev`/`--test` split for that kind - the caller should use
/// [`build_distro`] instead), or if the underlying `nix build` fails.
pub fn build_one(
  flakeref: &str,
  manifest: &Manifest,
  name: &str,
  variant: BuildVariant,
  with_bootloader: bool,
) -> Result<PathBuf> {
  let entry = manifest.get(name).ok_or_else(|| {
    color_eyre::eyre::eyre!(
      "no VM named '{name}' in xi.vm (declared: {})",
      manifest.keys().cloned().collect::<Vec<_>>().join(", ")
    )
  })?;

  if entry.kind != VmKind::Nixos {
    color_eyre::eyre::bail!(
      "'{name}' is kind = \"distro\"; use `xi vm build {name} --test` is \
       not meaningful here - distro VMs have no --dev/--test split, see \
       `xi vm check {name}`/`xi vm start {name}`"
    );
  }

  let attr = match (variant, with_bootloader) {
    (BuildVariant::Dev, false) => attrs::dev_vm(name),
    (BuildVariant::Dev, true) => attrs::dev_vm_with_bootloader(name),
    (BuildVariant::Test, _) => attrs::test(name),
  };

  info!("Building {flakeref}#{attr}");
  crate::manifest::build_and_resolve_path(&format!("{flakeref}#{attr}"))
}

/// Build one `kind = "distro"` VM's non-interactive `.driver` output.
///
/// No `--dev`/`--test` split for this kind (see
/// `openspec/changes/xi-vm-lifecycle/design.md`'s backend-scope decision):
/// this is always the same attribute `build` and `check` both build, the
/// latter also running it.
///
/// # Errors
///
/// Returns an error if the VM is unknown or is `kind = "nixos"`.
pub fn build_distro(flakeref: &str, manifest: &Manifest, name: &str) -> Result<PathBuf> {
  let entry = manifest.get(name).ok_or_else(|| {
    color_eyre::eyre::eyre!("no VM named '{name}' in xi.vm")
  })?;
  if entry.kind != VmKind::Distro {
    color_eyre::eyre::bail!("'{name}' is not kind = \"distro\"");
  }
  let attr = attrs::distro_build(name);
  info!("Building {flakeref}#{attr}");
  crate::manifest::build_and_resolve_path(&format!("{flakeref}#{attr}"))
}

/// Build the boot-free `--build-only` pre-check for one `kind = "nixos"`
/// VM.
///
/// # Errors
///
/// Returns an error if the VM is unknown or the build fails.
pub fn build_only(flakeref: &str, manifest: &Manifest, name: &str) -> Result<PathBuf> {
  if !manifest.contains_key(name) {
    color_eyre::eyre::bail!("no VM named '{name}' in xi.vm");
  }
  let attr = attrs::build_only(name);
  info!("Building {flakeref}#{attr} (boot-free)");
  crate::manifest::build_and_resolve_path(&format!("{flakeref}#{attr}"))
}

/// Build and run one VM's check derivation; its own build success/failure
/// IS the verdict (see `openspec/changes/xi-vm-lifecycle/design.md`).
///
/// # Errors
///
/// Returns an error if the VM is unknown.
pub fn check_one(
  flakeref: &str,
  manifest: &Manifest,
  name: &str,
  build_only_flag: bool,
) -> Result<CheckOutcome> {
  if !manifest.contains_key(name) {
    color_eyre::eyre::bail!("no VM named '{name}' in xi.vm");
  }

  if build_only_flag {
    return match build_only(flakeref, manifest, name) {
      Ok(_) => Ok(CheckOutcome::ok(name)),
      Err(e) => Ok(CheckOutcome::failed(name, "build-failed", &e.to_string())),
    };
  }

  let attr = attrs::test(name);
  match crate::manifest::build_and_resolve_path(&format!("{flakeref}#{attr}")) {
    Ok(_) => Ok(CheckOutcome::ok(name)),
    Err(e) => Ok(CheckOutcome::failed(name, "check-failed", &e.to_string())),
  }
}

/// Check every declared VM, continuing past a failure (SC-VM-004).
///
/// # Errors
///
/// Only returns an error for a programming/IO failure outside any single
/// VM's check; a check failure is reported in the returned outcomes, not
/// as an `Err`.
pub fn check_all(
  flakeref: &str,
  manifest: &Manifest,
  build_only_flag: bool,
) -> Result<Vec<CheckOutcome>> {
  let mut outcomes = Vec::with_capacity(manifest.len());
  for name in manifest.keys() {
    match check_one(flakeref, manifest, name, build_only_flag) {
      Ok(outcome) => outcomes.push(outcome),
      Err(e) => {
        error!("xi vm check {name}: {e}");
        outcomes.push(CheckOutcome::failed(name, "internal-error", &e.to_string()));
      },
    }
  }
  Ok(outcomes)
}

/// One VM's check verdict, shaped for the `--json` envelope
/// (SC-VM-004/SC-VM-003).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckOutcome {
  pub name:   String,
  pub status: &'static str,
  pub reason: Option<String>,
  pub detail: Option<String>,
}

impl CheckOutcome {
  fn ok(name: &str) -> Self {
    Self {
      name:   name.to_owned(),
      status: "ok",
      reason: None,
      detail: None,
    }
  }

  fn failed(name: &str, reason: &str, detail: &str) -> Self {
    Self {
      name:   name.to_owned(),
      status: "failed",
      reason: Some(reason.to_owned()),
      detail: Some(detail.to_owned()),
    }
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  fn manifest_with(name: &str, kind: VmKind) -> Manifest {
    let mut manifest = Manifest::new();
    manifest.insert(
      name.to_owned(),
      crate::manifest::VmManifestEntry {
        kind,
        display: crate::manifest::Display::Headless,
        with_bootloader: false,
        ssh_user: "xi".to_owned(),
        ports: vec![],
        distro: None,
        version: None,
      },
    );
    manifest
  }

  #[test]
  fn build_one_rejects_an_unknown_vm_name() {
    let manifest = manifest_with("gary", VmKind::Nixos);
    let err = build_one(".", &manifest, "nope", BuildVariant::Dev, false)
      .expect_err("must fail");
    assert!(err.to_string().contains("nope"));
  }

  #[test]
  fn build_one_rejects_a_distro_vm() {
    let manifest = manifest_with("patrick", VmKind::Distro);
    let err = build_one(".", &manifest, "patrick", BuildVariant::Dev, false)
      .expect_err("must fail");
    assert!(err.to_string().contains("distro"));
  }

  #[test]
  fn check_one_rejects_an_unknown_vm_name() {
    let manifest = manifest_with("gary", VmKind::Nixos);
    let err =
      check_one(".", &manifest, "nope", false).expect_err("must fail");
    assert!(err.to_string().contains("nope"));
  }
}
