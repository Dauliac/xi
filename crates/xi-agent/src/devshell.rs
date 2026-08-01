use std::time::Instant;

use color_eyre::eyre::Result;
use xi_develop::{compute_flake_id, daemon::client, dirs};

use crate::{
  args::DevshellArgs,
  schema::{Diagnostic, Envelope, DevshellPayload, DevshellState, elapsed_ms, emit},
};

pub const CMD: &str = "devshell";

pub fn run(args: DevshellArgs) -> Result<()> {
  let start = Instant::now();
  let payload = collect(&args);
  let env = Envelope::ok(CMD, payload, elapsed_ms(start));
  emit(&env)?;
  Ok(())
}

/// Query the daemon (fast path). Emits `NotRunning` if the socket does
/// not exist, `Degraded` on any transport error. Never blocks.
pub fn collect(args: &DevshellArgs) -> DevshellPayload {
  let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
  let flake_id = compute_flake_id(&cwd);
  let socket = dirs::daemon_socket_path(&flake_id);

  if !client::is_alive(&socket) {
    return DevshellPayload {
      state:               DevshellState::NotRunning,
      target:              None,
      package_count:       0,
      daemon_state:        None,
      active_cache_pushes: Vec::new(),
      entered_command:     "xi develop".to_owned(),
    };
  }

  let _ = args.timeout_ms; // reserved for future socket-level timeout
  match client::status(&socket) {
    Ok(status) => {
      let state = match status.state {
        xi_develop::daemon::protocol::DaemonState::Ready => DevshellState::Ready,
        xi_develop::daemon::protocol::DaemonState::Evaluating
        | xi_develop::daemon::protocol::DaemonState::Starting => {
          DevshellState::Evaluating
        },
        _ => DevshellState::Degraded,
      };
      DevshellPayload {
        state,
        target: Some(status.current_target),
        package_count: status.package_count,
        daemon_state: Some(format!("{:?}", status.state)),
        active_cache_pushes: status.active_cache_pushes,
        entered_command: "xi develop".to_owned(),
      }
    },
    Err(_) => DevshellPayload {
      state:               DevshellState::Degraded,
      target:              None,
      package_count:       0,
      daemon_state:        None,
      active_cache_pushes: Vec::new(),
      entered_command:     "xi develop".to_owned(),
    },
  }
}

/// Convenience: build a `Diagnostic` when the caller wants to attach a
/// non-fatal note about the devshell.
#[must_use]
pub fn hint_not_running() -> Diagnostic {
  Diagnostic::new(
    "daemon.not-running",
    "xi-develop daemon is not running for this project",
  )
  .with_hint("run `xi develop` to start it")
}
