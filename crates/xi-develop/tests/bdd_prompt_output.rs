//! BDD: tests/features/01_first_entry_trusted.feature
//! BDD: tests/features/06_leaving_flake.feature
//!
//! Tests for shell code generation from PromptResponse.
//! These test the Rust→shell code generation layer.

use xi_develop::daemon::protocol::*;

/// Helper: build a PromptResponse for testing.
fn make_response(
  source_env: bool,
  source_hook: bool,
  exit: bool,
  spawn: bool,
) -> PromptResponse {
  PromptResponse {
    should_source_env: source_env,
    env_file_path: if source_env {
      Some("/state/env-default.current.sh".into())
    } else {
      None
    },
    should_source_hook: source_hook,
    hook_file_path: if source_hook {
      Some("/state/hook-default.current.sh".into())
    } else {
      None
    },
    should_exit: exit,
    should_spawn_subshell: spawn,
    spawn_flake_root: if spawn {
      Some("/home/user/project".into())
    } else {
      None
    },
    notifications: vec![],
    daemon_state: DaemonState::Ready,
    is_trusted: true,
  }
}

/// BDD: 03_live_update_package.feature#Package added triggers re-source
#[test]
fn source_env_generates_source_command() {
  let resp = make_response(true, false, false, false);
  let output = generate_subshell_output(&resp, "bash");
  assert!(
    output.contains("source '/state/env-default.current.sh'"),
    "should contain source command for env file: {output}"
  );
}

/// BDD: 04_live_update_hook.feature#shellHook changed
#[test]
fn source_hook_generates_source_command() {
  let resp = make_response(false, true, false, false);
  let output = generate_subshell_output(&resp, "bash");
  assert!(
    output.contains("source '/state/hook-default.current.sh'"),
    "should contain source command for hook file: {output}"
  );
}

/// BDD: 06_leaving_flake.feature#User cd's out
#[test]
fn should_exit_generates_exit_command() {
  let resp = make_response(false, false, true, false);
  let output = generate_subshell_output(&resp, "bash");
  assert!(output.contains("exit 0"), "should contain exit 0: {output}");
}

/// BDD: 05_live_update_noop.feature#no-op
#[test]
fn no_changes_generates_empty_output() {
  let resp = make_response(false, false, false, false);
  let output = generate_subshell_output(&resp, "bash");
  assert!(
    output.trim().is_empty(),
    "no changes should produce empty output: '{output}'"
  );
}

/// BDD: 01_first_entry_trusted.feature (both env and hook)
#[test]
fn source_both_env_and_hook() {
  let resp = make_response(true, true, false, false);
  let output = generate_subshell_output(&resp, "bash");
  assert!(output.contains("source '/state/env-default.current.sh'"));
  assert!(output.contains("source '/state/hook-default.current.sh'"));
}

/// BDD: exit takes priority over source
#[test]
fn exit_takes_priority_over_source() {
  let mut resp = make_response(true, true, true, false);
  resp.should_exit = true;
  let output = generate_subshell_output(&resp, "bash");
  assert!(output.contains("exit 0"), "exit should take priority");
  // When exiting, no need to source
  assert!(!output.contains("source"), "should not source when exiting");
}

/// Generate shell output from a PromptResponse (subshell mode).
/// This is the contract that prompt.rs must implement.
fn generate_subshell_output(resp: &PromptResponse, _shell: &str) -> String {
  let mut lines = Vec::new();

  if resp.should_exit {
    lines.push("exit 0".to_string());
    return lines.join("\n");
  }

  if resp.should_source_env {
    if let Some(ref path) = resp.env_file_path {
      lines.push(format!("source '{path}'"));
    }
  }

  if resp.should_source_hook {
    if let Some(ref path) = resp.hook_file_path {
      lines.push(format!("source '{path}'"));
    }
  }

  lines.join("\n")
}
