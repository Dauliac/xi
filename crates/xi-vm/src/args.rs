//! The `xi vm` CLI surface (`openspec/changes/xi-vm-lifecycle/proposal.md`).

use clap::{Args, Subcommand};
use xi_core::checks::{FeatureRequirements, FlakeFeatures};

#[derive(Args, Debug)]
#[clap(verbatim_doc_comment)]
/// Declarative multi-VM dev-space lifecycle
///
/// "Vagrant / docker-compose, but for NixOS machines, inside xi."
/// Operates on `xi.vm.<name>` entries declared by the current flake;
/// see `docs/guides/vm.md`.
pub struct VmArgs {
  /// Flake reference the `xi.vm.<name>` manifest and its rendered
  /// packages are read from.
  #[arg(long, default_value = ".", global = true)]
  pub flake: String,

  #[command(subcommand)]
  pub subcommand: VmSubcommand,
}

impl VmArgs {
  /// `xi vm` always evaluates a flake (the manifest + per-VM packages);
  /// it never falls back to a legacy, non-flake path.
  #[must_use]
  pub fn get_feature_requirements(&self) -> Box<dyn FeatureRequirements> {
    Box::new(FlakeFeatures)
  }
}

#[derive(Debug, Subcommand)]
pub enum VmSubcommand {
  /// Enumerate every declared VM with its last-known status and check
  /// verdict
  List,

  /// Build a VM's artifact
  Build(VmBuildArgs),

  /// Build and run a VM's check (or every VM's)
  Check(VmCheckArgs),

  /// Build (if needed) and launch a VM
  Start(VmStartArgs),

  /// Terminate a backgrounded VM
  Stop(VmStopArgs),

  /// Connect to a running VM
  Connect(VmConnectArgs),

  /// Report a VM's running/stopped status
  Status(VmStatusArgs),
}

#[derive(Debug, Args)]
pub struct VmBuildArgs {
  /// VM name, or omit with --all
  pub name: Option<String>,

  /// Build every declared VM
  #[arg(long, conflicts_with = "name")]
  pub all: bool,

  /// Build the persistent, bootable image (the default)
  #[arg(long, conflicts_with = "test")]
  pub dev: bool,

  /// Build the `runNixOSTest`/`nix-vm-test` check derivation
  #[arg(long)]
  pub test: bool,

  /// Build `vmWithBootLoader` instead of the plain `vm`
  #[arg(long, short = 'B')]
  pub with_bootloader: bool,
}

#[derive(Debug, Args)]
pub struct VmCheckArgs {
  /// VM name, or omit with --all
  pub name: Option<String>,

  /// Check every declared VM; one failure does not abort the rest
  #[arg(long, conflicts_with = "name")]
  pub all: bool,

  /// Print the result as a versioned JSON envelope
  #[arg(long)]
  pub json: bool,

  /// Build only the boot-free `system.build.toplevel` pre-check, never
  /// booting a VM
  #[arg(long)]
  pub build_only: bool,
}

#[derive(Debug, Args)]
pub struct VmStartArgs {
  /// VM name, or omit with --all
  pub name: Option<String>,

  /// Start every declared `kind = "nixos"` entry, implying --background
  #[arg(long, conflicts_with = "name")]
  pub all: bool,

  /// Open a QEMU display, overriding the manifest default
  #[arg(long, conflicts_with = "headless")]
  pub graphical: bool,

  /// Suppress the QEMU display, overriding the manifest default
  #[arg(long)]
  pub headless: bool,

  /// Detach after the VM is reachable, registering it for `status`/`stop`
  #[arg(long)]
  pub background: bool,

  /// Fall back to dynamic port resolution when a declared `hostPort`
  /// hint is already taken, instead of failing loudly
  #[arg(long)]
  pub allow_port_fallback: bool,

  /// Build `vmWithBootLoader` instead of the plain `vm`
  #[arg(long, short = 'B')]
  pub with_bootloader: bool,
}

#[derive(Debug, Args)]
pub struct VmStopArgs {
  /// VM name, or omit with --all
  pub name: Option<String>,

  /// Stop every backgrounded `kind = "nixos"` instance
  #[arg(long, conflicts_with = "name")]
  pub all: bool,
}

#[derive(Debug, Args)]
pub struct VmConnectArgs {
  /// VM name
  pub name: String,

  /// Attach to the QEMU serial console instead of `SSH`ing in
  #[arg(long)]
  pub console: bool,
}

#[derive(Debug, Args)]
pub struct VmStatusArgs {
  /// VM name, or omit with --all
  pub name: Option<String>,

  /// Report every declared VM
  #[arg(long, conflicts_with = "name")]
  pub all: bool,

  /// Print the result as a versioned JSON envelope
  #[arg(long)]
  pub json: bool,
}
