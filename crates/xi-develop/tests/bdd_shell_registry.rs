//! BDD: tests/features/12_multi_terminal.feature
//! BDD: tests/features/06_leaving_flake.feature
//!
//! Tests for ShellRegistry -- per-PID tracking.
//! These tests document the expected behavior. They will FAIL (TDD red)
//! until F1 implements the methods.

use xi_develop::daemon::shell_registry::ShellRegistry;

/// Register a consumer, verify count increments
#[test]
fn register_increments_count() {
  let mut reg = ShellRegistry::new();
  assert_eq!(reg.consumer_count(), 0);
  reg.register(1001, None, "abc123", "default");
  assert_eq!(reg.consumer_count(), 1);
}

/// Deregister removes PID, count decrements
#[test]
fn deregister_decrements_count() {
  let mut reg = ShellRegistry::new();
  reg.register(1001, None, "abc123", "default");
  assert!(reg.deregister(1001));
  assert_eq!(reg.consumer_count(), 0);
}

/// Deregister non-existent PID returns false
#[test]
fn deregister_nonexistent_returns_false() {
  let mut reg = ShellRegistry::new();
  assert!(!reg.deregister(9999));
}

/// should_source_env returns true when daemon gen > shell's last gen
#[test]
fn should_source_env_when_gen_ahead() {
  let mut reg = ShellRegistry::new();
  reg.register(1001, None, "abc123", "default");
  // PID just registered with gen 0, daemon is at gen 1
  assert!(reg.should_source_env(1001, 1));
}

/// should_source_env returns false when gens match
#[test]
fn should_source_env_false_when_current() {
  let mut reg = ShellRegistry::new();
  reg.register(1001, None, "abc123", "default");
  reg.mark_sourced_env(1001, 5);
  assert!(!reg.should_source_env(1001, 5));
}

/// should_source_hook returns true when hook gen changed
#[test]
fn should_source_hook_when_hook_gen_ahead() {
  let mut reg = ShellRegistry::new();
  reg.register(1001, None, "abc123", "default");
  assert!(reg.should_source_hook(1001, 1));
}

/// mark_sourced_env updates per-PID gen
#[test]
fn mark_sourced_env_updates_gen() {
  let mut reg = ShellRegistry::new();
  reg.register(1001, None, "abc123", "default");
  reg.mark_sourced_env(1001, 3);
  assert!(!reg.should_source_env(1001, 3));
  assert!(reg.should_source_env(1001, 4));
}

/// get returns ShellInstance for registered PID
#[test]
fn get_returns_instance() {
  let mut reg = ShellRegistry::new();
  reg.register(1001, Some(1000), "abc123", "default");
  let inst = reg.get(1001);
  assert!(inst.is_some());
  let inst = inst.expect("just registered");
  assert_eq!(inst.pid, 1001);
  assert_eq!(inst.parent_pid, Some(1000));
  assert_eq!(inst.flake_id, "abc123");
}

/// Register with parent_pid tracks nesting
#[test]
fn register_with_parent_tracks_nesting() {
  let mut reg = ShellRegistry::new();
  reg.register(1001, None, "flakeA", "default");
  reg.register(1002, Some(1001), "flakeB", "default");
  assert_eq!(reg.consumer_count(), 2);
  let child = reg.get(1002).expect("child registered");
  assert_eq!(child.parent_pid, Some(1001));
}
