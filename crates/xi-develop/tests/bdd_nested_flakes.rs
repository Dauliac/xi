//! BDD: tests/features/07_nested_flakes.feature
//!
//! Tests for nested flake detection via find_flake_stack.
//! These tests WILL FAIL (TDD red) until F2 implements find_flake_stack.

use std::fs;
use xi_develop::flake_detect;

/// find_flake_stack from deeply nested dir returns [outermost, innermost]
#[test]
fn find_flake_stack_nested() {
  let tmp = tempfile::tempdir().unwrap();
  let mono = tmp.path().join("mono");
  let api = mono.join("services/api");
  fs::create_dir_all(&api).unwrap();
  fs::write(mono.join("flake.nix"), "{}").unwrap();
  fs::write(api.join("flake.nix"), "{}").unwrap();

  let stack = flake_detect::find_flake_stack(&api);
  assert_eq!(stack.len(), 2);
  assert_eq!(stack[0], mono); // outermost first
  assert_eq!(stack[1], api); // innermost last
}

/// find_flake_stack from single flake dir returns [flake_root]
#[test]
fn find_flake_stack_single() {
  let tmp = tempfile::tempdir().unwrap();
  let project = tmp.path().join("project");
  fs::create_dir_all(&project).unwrap();
  fs::write(project.join("flake.nix"), "{}").unwrap();

  let stack = flake_detect::find_flake_stack(&project);
  assert_eq!(stack.len(), 1);
  assert_eq!(stack[0], project);
}

/// find_flake_stack from non-flake dir returns empty vec
#[test]
fn find_flake_stack_no_flake() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = tmp.path().join("noflake");
  fs::create_dir_all(&dir).unwrap();

  let stack = flake_detect::find_flake_stack(&dir);
  assert!(stack.is_empty());
}

/// find_flake_stack from subdir inside flake (but no nested flake)
#[test]
fn find_flake_stack_subdir_single() {
  let tmp = tempfile::tempdir().unwrap();
  let mono = tmp.path().join("mono");
  let src = mono.join("src/lib");
  fs::create_dir_all(&src).unwrap();
  fs::write(mono.join("flake.nix"), "{}").unwrap();

  let stack = flake_detect::find_flake_stack(&src);
  assert_eq!(stack.len(), 1);
  assert_eq!(stack[0], mono);
}

/// find_flake_root returns nearest (innermost) flake
#[test]
fn find_flake_root_returns_nearest() {
  let tmp = tempfile::tempdir().unwrap();
  let mono = tmp.path().join("mono");
  let api = mono.join("services/api");
  fs::create_dir_all(&api).unwrap();
  fs::write(mono.join("flake.nix"), "{}").unwrap();
  fs::write(api.join("flake.nix"), "{}").unwrap();

  let root = flake_detect::find_flake_root(&api);
  assert_eq!(root, Some(api));
}
