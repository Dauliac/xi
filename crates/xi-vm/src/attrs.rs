//! Flake attribute names for a declared VM.
//!
//! `modules/flake-parts/vm.nix` renders every `xi.vm.<name>` entry under
//! these exact names (see that module's `xi.vm` option doc). Kept as pure
//! string functions, not a `nix eval`, so `xi vm` can compute them without
//! a round trip once it already knows a VM's `kind` from the manifest.

/// `xi-vm-<name>-vm`: the plain build, no guest account.
#[must_use]
pub fn vm(name: &str) -> String {
  format!("xi-vm-{name}-vm")
}

/// `xi-vm-<name>-vm-with-bootloader`.
#[must_use]
pub fn vm_with_bootloader(name: &str) -> String {
  format!("xi-vm-{name}-vm-with-bootloader")
}

/// `xi-vm-<name>-dev-vm`: extended with the guest account, for
/// `start`/`connect`.
#[must_use]
pub fn dev_vm(name: &str) -> String {
  format!("xi-vm-{name}-dev-vm")
}

/// `xi-vm-<name>-dev-vm-with-bootloader`.
#[must_use]
pub fn dev_vm_with_bootloader(name: &str) -> String {
  format!("xi-vm-{name}-dev-vm-with-bootloader")
}

/// `xi-vm-<name>-build-only`: boot-free `system.build.toplevel` pre-check.
#[must_use]
pub fn build_only(name: &str) -> String {
  format!("xi-vm-{name}-build-only")
}

/// `xi-vm-<name>-test`: the `runNixOSTest`/`nix-vm-test` check derivation.
#[must_use]
pub fn test(name: &str) -> String {
  format!("xi-vm-{name}-test")
}

/// `xi-vm-<name>-distro-build`: `kind = "distro"`'s non-interactive driver.
#[must_use]
pub fn distro_build(name: &str) -> String {
  format!("xi-vm-{name}-distro-build")
}

/// `xi-vm-<name>-distro-interactive`: `kind = "distro"`'s interactive
/// driver (used by `start`).
#[must_use]
pub fn distro_interactive(name: &str) -> String {
  format!("xi-vm-{name}-distro-interactive")
}

/// `xi-vm-manifest`: the JSON manifest `crates/xi-vm` reads at runtime.
pub const MANIFEST: &str = "xi-vm-manifest";

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_follow_the_nix_module_convention() {
    assert_eq!(vm("gary"), "xi-vm-gary-vm");
    assert_eq!(vm_with_bootloader("gary"), "xi-vm-gary-vm-with-bootloader");
    assert_eq!(dev_vm("gary"), "xi-vm-gary-dev-vm");
    assert_eq!(
      dev_vm_with_bootloader("gary"),
      "xi-vm-gary-dev-vm-with-bootloader"
    );
    assert_eq!(build_only("gary"), "xi-vm-gary-build-only");
    assert_eq!(test("gary"), "xi-vm-gary-test");
    assert_eq!(distro_build("patrick"), "xi-vm-patrick-distro-build");
    assert_eq!(
      distro_interactive("patrick"),
      "xi-vm-patrick-distro-interactive"
    );
  }
}
