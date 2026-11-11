//! `xi vm start` / `stop` / `connect` / `status`: the `kind = "nixos"`
//! background-instance lifecycle, backed by [`crate::registry`].

use std::{
  os::unix::process::CommandExt as _,
  path::{Path, PathBuf},
  process::{Command, Stdio},
};

use color_eyre::Result;
use tracing::info;

use crate::{
  manifest::{Manifest, VmKind, VmManifestEntry},
  port,
  registry::{self, Instance, ResolvedAppPort},
  ssh_key,
};

/// Display mode for `xi vm start`, resolved from the manifest default and
/// an optional CLI override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
  Graphical,
  Headless,
}

impl DisplayMode {
  #[must_use]
  pub const fn resolve(
    entry: &VmManifestEntry,
    graphical_flag: bool,
    headless_flag: bool,
  ) -> Self {
    if headless_flag {
      return Self::Headless;
    }
    if graphical_flag {
      return Self::Graphical;
    }
    match entry.display {
      crate::manifest::Display::Graphical => Self::Graphical,
      crate::manifest::Display::Headless => Self::Headless,
    }
  }

  const fn as_str(self) -> &'static str {
    match self {
      Self::Graphical => "graphical",
      Self::Headless => "headless",
    }
  }
}

/// Find the `bin/run-*-vm` runner script nixpkgs' `qemu-vm.nix` places in
/// a `system.build.vm`/`vmWithBootLoader` output.
///
/// Its exact name depends on the backing configuration's `networking.hostName`
/// (`run-<hostname>-vm`), which this crate has no reason to duplicate, so
/// it is located by globbing `bin/` instead of recomputed.
///
/// # Errors
///
/// Returns an error if `bin/` has no `run-*-vm` entry.
pub fn find_runner_script(vm_store_path: &Path) -> Result<PathBuf> {
  let bin = vm_store_path.join("bin");
  let entries = std::fs::read_dir(&bin).map_err(|e| {
    color_eyre::eyre::eyre!("failed to read {}: {e}", bin.display())
  })?;
  for entry in entries.flatten() {
    let file_name = entry.file_name();
    let name = file_name.to_string_lossy();
    if name.starts_with("run-") && name.ends_with("-vm") {
      return Ok(entry.path());
    }
  }
  color_eyre::eyre::bail!(
    "no run-*-vm runner script found under {}",
    bin.display()
  )
}

/// Resolve every port a `--background` (or foreground) start needs:
/// the implicit SSH forward plus every declared app forward.
///
/// # Errors
///
/// Returns an error if any port cannot be resolved (see
/// [`port::resolve_app_port`]).
pub fn resolve_ports(
  entry: &VmManifestEntry,
  allow_port_fallback: bool,
) -> Result<(u16, Vec<ResolvedAppPort>)> {
  let ssh_port = port::resolve_free_port(port::SSH_BASE_PORT)?;
  let mut app_ports = Vec::with_capacity(entry.ports.len());
  for forward in &entry.ports {
    let host_port = port::resolve_app_port(
      forward.host_port,
      allow_port_fallback,
      port::SSH_BASE_PORT.saturating_add(1000),
    )?;
    app_ports.push(ResolvedAppPort::from_forward(forward, host_port));
  }
  Ok((ssh_port, app_ports))
}

/// Build the `QEMU_NET_OPTS` hostfwd fragment for the resolved ports.
///
/// Matches nixpkgs' `qemu-vm.nix` runner script convention: a
/// comma-separated list of `hostfwd=<proto>::<host>-:<guest>` clauses,
/// appended to its own default `-netdev` by way of `QEMU_NET_OPTS`.
#[must_use]
pub fn qemu_net_opts(ssh_port: u16, app_ports: &[ResolvedAppPort]) -> String {
  let mut clauses = vec![format!("hostfwd=tcp::{ssh_port}-:22")];
  for app in app_ports {
    clauses.push(format!(
      "hostfwd={}::{}-:{}",
      app.protocol, app.host_port, app.guest_port
    ));
  }
  clauses.join(",")
}

/// `xi vm start <name>`: build the `--dev` artifact, then launch it.
///
/// Foreground: blocks for the VM's lifetime (`Command::exec`, replacing
/// this process, matching how `xi os build-vm --run` already launches a
/// VM). `--background`: spawns detached, registers the instance, and
/// returns once spawned.
///
/// # Errors
///
/// Returns an error if the VM is unknown, is `kind = "distro"` with
/// `--background` requested, if the build fails, or if the runner script
/// cannot be found or spawned.
#[expect(
  clippy::too_many_arguments,
  clippy::fn_params_excessive_bools,
  reason = "mirrors the CLI surface 1:1"
)]
pub fn start(
  flakeref: &str,
  manifest: &Manifest,
  state_dir: &Path,
  name: &str,
  graphical_flag: bool,
  headless_flag: bool,
  background: bool,
  allow_port_fallback: bool,
  with_bootloader: bool,
) -> Result<()> {
  let entry = manifest
    .get(name)
    .ok_or_else(|| color_eyre::eyre::eyre!("no VM named '{name}' in xi.vm"))?;

  if entry.kind != VmKind::Nixos {
    color_eyre::eyre::bail!(
      "'{name}' is kind = \"distro\": `start` runs its interactive driver in \
       the foreground and does not support --background; omit --background to \
       run it"
    );
  }

  let (_private_key, _public_key) = ssh_key::ensure_keypair(state_dir)?;

  let display = DisplayMode::resolve(entry, graphical_flag, headless_flag);
  let vm_path = crate::build::build_one(
    flakeref,
    manifest,
    name,
    crate::build::BuildVariant::Dev,
    with_bootloader,
  )?;
  let runner = find_runner_script(&vm_path)?;

  let (ssh_port, app_ports) = resolve_ports(entry, allow_port_fallback)?;
  let net_opts = qemu_net_opts(ssh_port, &app_ports);
  let qemu_opts = match display {
    DisplayMode::Headless => "-nographic".to_owned(),
    DisplayMode::Graphical => String::new(),
  };

  let display_label = display.as_str();
  info!("Starting '{name}' ({display_label}, ssh port {ssh_port})");

  let mut command = Command::new(&runner);
  command
    .env("QEMU_NET_OPTS", &net_opts)
    .env("QEMU_OPTS", &qemu_opts);

  if background {
    let control_path =
      registry::instance_dir(state_dir, name).join("control.sock");
    std::fs::create_dir_all(control_path.parent().unwrap_or(state_dir))
      .map_err(|e| {
        color_eyre::eyre::eyre!("failed to create state dir: {e}")
      })?;

    command
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null());
    // New session so the VM survives this process exiting; there is no
    // control-socket supervisor to keep it attached to (see
    // `registry`'s header comment).
    unsafe {
      command.pre_exec(|| {
        nix::unistd::setsid()
          .map(|_| ())
          .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
      });
    }

    let child = command.spawn().map_err(|e| {
      color_eyre::eyre::eyre!("failed to spawn {}: {e}", runner.display())
    })?;

    registry::write(state_dir, name, &Instance {
      pid: child.id(),
      display: display.as_str().to_owned(),
      ssh_port,
      app_ports,
      control_path,
    })?;
    info!("'{name}' started in the background (pid {})", child.id());
    Ok(())
  } else {
    // Foreground: replace this process, matching `xi os build-vm --run`.
    let error = command.exec();
    Err(color_eyre::eyre::eyre!(
      "failed to exec {}: {error}",
      runner.display()
    ))
  }
}

/// `xi vm stop <name>`: terminate a backgrounded instance.
///
/// # Errors
///
/// Returns an error if no instance is registered, if the VM was not
/// started with `--background`, or if it is `kind = "distro"`.
pub fn stop(manifest: &Manifest, state_dir: &Path, name: &str) -> Result<()> {
  let entry = manifest
    .get(name)
    .ok_or_else(|| color_eyre::eyre::eyre!("no VM named '{name}' in xi.vm"))?;
  if entry.kind != VmKind::Nixos {
    color_eyre::eyre::bail!(
      "'{name}' is kind = \"distro\": stop is unsupported for this kind"
    );
  }

  let Some(instance) = registry::read(state_dir, name)? else {
    color_eyre::eyre::bail!(
      "'{name}' has no registered instance (not started with --background, or \
       already stopped)"
    );
  };

  registry::terminate(&instance)?;
  registry::clear(state_dir, name)?;
  info!("'{name}' stopped (was pid {})", instance.pid);
  Ok(())
}

/// One VM's `xi vm status` entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Status {
  pub name:      String,
  pub running:   bool,
  pub ssh_port:  Option<u16>,
  pub app_ports: Vec<ResolvedAppPort>,
}

/// `xi vm status [<name>|--all]`.
///
/// # Errors
///
/// Returns an error if `name` is given and unknown.
pub fn status(
  manifest: &Manifest,
  state_dir: &Path,
  name: &str,
) -> Result<Status> {
  if !manifest.contains_key(name) {
    color_eyre::eyre::bail!("no VM named '{name}' in xi.vm");
  }
  let instance = registry::read(state_dir, name)?;
  let running = instance.as_ref().is_some_and(registry::is_alive);
  Ok(Status {
    name: name.to_owned(),
    running,
    ssh_port: instance.as_ref().filter(|_| running).map(|i| i.ssh_port),
    app_ports: instance
      .filter(|_| running)
      .map(|i| i.app_ports)
      .unwrap_or_default(),
  })
}

/// `xi vm connect <name> [--console]`.
///
/// `--console` is not implemented by this PID-file-registry build (it
/// needs the control-socket `console-attach` RPC the design calls for;
/// see `registry`'s header comment) and returns a clear "not supported"
/// error rather than attempting and failing confusingly.
///
/// # Errors
///
/// Returns an error if the VM is unknown, is `kind = "distro"`, has no
/// running instance, or if `console` is requested.
pub fn connect(
  manifest: &Manifest,
  state_dir: &Path,
  name: &str,
  console: bool,
) -> Result<()> {
  let entry = manifest
    .get(name)
    .ok_or_else(|| color_eyre::eyre::eyre!("no VM named '{name}' in xi.vm"))?;
  if entry.kind != VmKind::Nixos {
    color_eyre::eyre::bail!(
      "'{name}' is kind = \"distro\": connect is unsupported - `start` \
       already opens an interactive console"
    );
  }
  if console {
    color_eyre::eyre::bail!(
      "--console is not implemented: it needs a control-socket supervisor \
       this build's PID-file registry does not provide"
    );
  }

  let Some(instance) = registry::read(state_dir, name)? else {
    color_eyre::eyre::bail!(
      "'{name}' has no running instance; run `xi vm start {name} \
       --background` first"
    );
  };
  if !registry::is_alive(&instance) {
    color_eyre::eyre::bail!(
      "'{name}' is registered but its process is gone; run `xi vm stop \
       {name}` to clear it"
    );
  }

  let (private_key, _public_key) = ssh_key::ensure_keypair(state_dir)?;

  let error = Command::new("ssh")
    .arg("-i")
    .arg(&private_key)
    .arg("-p")
    .arg(instance.ssh_port.to_string())
    .arg("-o")
    .arg("StrictHostKeyChecking=accept-new")
    .arg("-o")
    .arg("IdentitiesOnly=yes")
    .arg(format!("{}@127.0.0.1", entry.ssh_user))
    .exec();
  Err(color_eyre::eyre::eyre!("failed to exec ssh: {error}"))
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  fn entry(
    display: crate::manifest::Display,
    ports: Vec<crate::manifest::PortForward>,
  ) -> VmManifestEntry {
    VmManifestEntry {
      kind: VmKind::Nixos,
      display,
      with_bootloader: false,
      ssh_user: "xi".to_owned(),
      ports,
      distro: None,
      version: None,
    }
  }

  #[test]
  fn display_mode_prefers_explicit_flags_over_the_manifest_default() {
    let graphical_default = entry(crate::manifest::Display::Graphical, vec![]);
    assert_eq!(
      DisplayMode::resolve(&graphical_default, false, true),
      DisplayMode::Headless
    );
    let headless_default = entry(crate::manifest::Display::Headless, vec![]);
    assert_eq!(
      DisplayMode::resolve(&headless_default, true, false),
      DisplayMode::Graphical
    );
  }

  #[test]
  fn display_mode_falls_back_to_the_manifest_default() {
    let graphical_default = entry(crate::manifest::Display::Graphical, vec![]);
    assert_eq!(
      DisplayMode::resolve(&graphical_default, false, false),
      DisplayMode::Graphical
    );
  }

  #[test]
  fn qemu_net_opts_always_includes_the_implicit_ssh_forward() {
    let opts = qemu_net_opts(22220, &[]);
    assert_eq!(opts, "hostfwd=tcp::22220-:22");
  }

  #[test]
  fn qemu_net_opts_appends_app_forwards() {
    let app_ports = vec![ResolvedAppPort {
      guest_port: 8080,
      host_port:  18080,
      protocol:   "tcp".to_owned(),
    }];
    let opts = qemu_net_opts(22220, &app_ports);
    assert_eq!(opts, "hostfwd=tcp::22220-:22,hostfwd=tcp::18080-:8080");
  }

  #[test]
  fn status_of_an_unknown_vm_is_an_error() {
    let manifest = Manifest::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let err = status(&manifest, dir.path(), "nope").expect_err("must fail");
    assert!(err.to_string().contains("nope"));
  }

  #[test]
  fn status_of_a_never_started_vm_is_not_running() {
    let mut manifest = Manifest::new();
    manifest.insert(
      "gary".to_owned(),
      entry(crate::manifest::Display::Headless, vec![]),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let result = status(&manifest, dir.path(), "gary").expect("status");
    assert!(!result.running);
    assert!(result.ssh_port.is_none());
  }

  #[test]
  fn stop_of_a_never_started_vm_is_an_error() {
    let mut manifest = Manifest::new();
    manifest.insert(
      "gary".to_owned(),
      entry(crate::manifest::Display::Headless, vec![]),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let err = stop(&manifest, dir.path(), "gary").expect_err("must fail");
    assert!(err.to_string().contains("no registered instance"));
  }
}
