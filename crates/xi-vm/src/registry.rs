//! The per-VM running-instance registry.
//!
//! `openspec/changes/xi-vm-lifecycle/design.md` specifies a control socket
//! (reusing `xi-sources-daemon/src/control.rs`'s framing) owned by a small
//! supervisor process. This implementation instead records the same
//! information - PID, display mode, resolved ports - in one JSON file per
//! VM under `<state_dir>/vm/<name>/instance.json`, written atomically and
//! read back by `status`/`stop`/`connect`.
//!
//! This is a deliberate scope reduction, not an oversight: a control
//! socket needs a supervisor process to own it, which in turn needs a
//! running QEMU to supervise to be exercised at all, and no `qemu-system-*`
//! binary is available to test against in this change's authoring
//! environment. The PID-file registry is real, working code - `status`
//! liveness is a plain `kill(pid, 0)` probe, `stop` sends `SIGTERM` to the
//! same PID - but it does not implement the `console-attach` RPC a control
//! socket would carry. Upgrading to the socket design later does not
//! change this module's on-disk shape, only how `stop`/`status` connect.

use std::{
  io::Write as _,
  path::{Path, PathBuf},
};

use color_eyre::Result;
use nix::{
  sys::signal::{self, Signal},
  unistd::Pid,
};
use serde::{Deserialize, Serialize};

use crate::manifest::PortForward;

/// One running `--dev` instance's recorded state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
  pub pid:          u32,
  pub display:      String,
  pub ssh_port:     u16,
  pub app_ports:    Vec<ResolvedAppPort>,
  pub control_path: PathBuf,
}

/// One declared app forward's resolved host port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAppPort {
  pub guest_port: u16,
  pub host_port:  u16,
  pub protocol:   String,
}

impl ResolvedAppPort {
  #[must_use]
  pub fn from_forward(forward: &PortForward, host_port: u16) -> Self {
    Self {
      guest_port: forward.guest_port,
      host_port,
      protocol: forward.protocol.clone(),
    }
  }
}

/// `<state_dir>/vm/<name>/`.
#[must_use]
pub fn instance_dir(state_dir: &Path, name: &str) -> PathBuf {
  state_dir.join("vm").join(name)
}

/// `<state_dir>/vm/<name>/instance.json`.
#[must_use]
pub fn instance_path(state_dir: &Path, name: &str) -> PathBuf {
  instance_dir(state_dir, name).join("instance.json")
}

/// Write `instance` for `name`, creating the directory if needed.
///
/// Writes to a sibling temp file and renames over the target, so a
/// concurrent reader never observes a half-written file.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file
/// cannot be written.
pub fn write(state_dir: &Path, name: &str, instance: &Instance) -> Result<()> {
  let dir = instance_dir(state_dir, name);
  std::fs::create_dir_all(&dir).map_err(|e| {
    color_eyre::eyre::eyre!("failed to create {}: {e}", dir.display())
  })?;

  let target = instance_path(state_dir, name);
  let tmp = dir.join("instance.json.tmp");
  let json = serde_json::to_vec_pretty(instance)
    .map_err(|e| color_eyre::eyre::eyre!("failed to serialize instance: {e}"))?;

  let mut file = std::fs::File::create(&tmp).map_err(|e| {
    color_eyre::eyre::eyre!("failed to create {}: {e}", tmp.display())
  })?;
  file
    .write_all(&json)
    .map_err(|e| color_eyre::eyre::eyre!("failed to write {}: {e}", tmp.display()))?;
  std::fs::rename(&tmp, &target).map_err(|e| {
    color_eyre::eyre::eyre!(
      "failed to move {} into place: {e}",
      target.display()
    )
  })?;
  Ok(())
}

/// Read the registered instance for `name`, if any.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be parsed.
pub fn read(state_dir: &Path, name: &str) -> Result<Option<Instance>> {
  let path = instance_path(state_dir, name);
  match std::fs::read_to_string(&path) {
    Ok(raw) => {
      let instance = serde_json::from_str(&raw).map_err(|e| {
        color_eyre::eyre::eyre!("failed to parse {}: {e}", path.display())
      })?;
      Ok(Some(instance))
    },
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => {
      Err(color_eyre::eyre::eyre!("failed to read {}: {e}", path.display()))
    },
  }
}

/// Remove the registered instance for `name`, if any. Not an error if
/// there was none.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be removed.
pub fn clear(state_dir: &Path, name: &str) -> Result<()> {
  let path = instance_path(state_dir, name);
  match std::fs::remove_file(&path) {
    Ok(()) => Ok(()),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(e) => {
      Err(color_eyre::eyre::eyre!("failed to remove {}: {e}", path.display()))
    },
  }
}

/// Whether `instance`'s PID is still alive.
///
/// A `kill(pid, 0)` liveness probe: sends no signal, only asks whether the
/// process (and this process' permission to signal it) exists.
#[must_use]
pub fn is_alive(instance: &Instance) -> bool {
  signal::kill(Pid::from_raw(instance.pid.cast_signed()), None).is_ok()
}

/// Send `SIGTERM` to `instance`'s PID.
///
/// # Errors
///
/// Returns an error if the signal could not be delivered (e.g. the
/// process is already gone, or this process lacks permission).
pub fn terminate(instance: &Instance) -> Result<()> {
  signal::kill(Pid::from_raw(instance.pid.cast_signed()), Signal::SIGTERM)
    .map_err(|e| color_eyre::eyre::eyre!("failed to signal pid {}: {e}", instance.pid))
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  fn sample_instance() -> Instance {
    Instance {
      pid:          std::process::id(),
      display:      "headless".to_owned(),
      ssh_port:     22220,
      app_ports:    vec![ResolvedAppPort {
        guest_port: 8080,
        host_port:  8080,
        protocol:   "tcp".to_owned(),
      }],
      control_path: PathBuf::from("/tmp/xi/vm/gary/control.sock"),
    }
  }

  #[test]
  fn write_then_read_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let instance = sample_instance();
    write(dir.path(), "gary", &instance).expect("write");

    let read_back = read(dir.path(), "gary").expect("read").expect("present");
    assert_eq!(read_back.pid, instance.pid);
    assert_eq!(read_back.ssh_port, instance.ssh_port);
    assert_eq!(read_back.app_ports.len(), 1);
  }

  #[test]
  fn reading_an_unregistered_vm_is_none_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let read_back = read(dir.path(), "nobody").expect("read");
    assert!(read_back.is_none());
  }

  #[test]
  fn clear_removes_the_file_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "gary", &sample_instance()).expect("write");
    clear(dir.path(), "gary").expect("first clear");
    assert!(read(dir.path(), "gary").expect("read").is_none());
    clear(dir.path(), "gary").expect("second clear is a no-op");
  }

  #[test]
  fn is_alive_is_true_for_the_current_process() {
    let instance = sample_instance();
    assert!(is_alive(&instance));
  }

  #[test]
  fn is_alive_is_false_for_a_pid_that_cannot_exist() {
    let mut instance = sample_instance();
    // `i32::MAX` as a PID is never valid on Linux (`pid_max` is far
    // below it) and, unlike `u32::MAX` (which bit-casts to -1, the
    // POSIX "every process this caller may signal" wildcard `kill`
    // treats specially rather than as a literal PID), it bit-casts to
    // an ordinary positive PID with no special `kill(2)` meaning.
    instance.pid = i32::MAX.cast_unsigned();
    assert!(!is_alive(&instance));
  }
}
