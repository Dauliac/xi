//! Nix CLI flag registry, generated at build time from `nix __dump-cli`.
//!
//! Regenerated automatically when the nix package changes (via nix build)
//! or when `nix` is available on PATH (via cargo build).
//!
//! If nix was not available at build time, [`SCHEMA_AVAILABLE`] is `false`
//! and all lookups return `None`.

include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn schema_available_reflects_build_environment() {
    // In CI/dev with nix on PATH, this should be true.
    // We can't assert a specific value since it depends on the build env,
    // but we can verify the const exists and is a bool.
    let _: bool = SCHEMA_AVAILABLE;
  }

  #[test]
  fn global_flags_are_sorted() {
    for window in GLOBAL_FLAGS.windows(2) {
      assert!(
        window[0].name <= window[1].name,
        "GLOBAL_FLAGS not sorted: {:?} > {:?}",
        window[0].name,
        window[1].name
      );
    }
  }

  #[test]
  fn commands_are_sorted() {
    for window in COMMANDS.windows(2) {
      assert!(
        window[0] <= window[1],
        "COMMANDS not sorted: {:?} > {:?}",
        window[0],
        window[1]
      );
    }
  }

  #[test]
  fn global_flag_arity_returns_none_for_unknown() {
    assert_eq!(global_flag_arity("this-flag-does-not-exist"), None);
  }

  #[test]
  fn command_flag_arity_returns_none_for_unknown_command() {
    assert_eq!(command_flag_arity("nonexistent-command", "foo"), None);
  }

  // The following tests only run when the schema was generated from a real nix.
  #[test]
  fn well_known_global_flags_when_schema_available() {
    if !SCHEMA_AVAILABLE {
      return;
    }
    // These flags exist in every nix version with the new CLI.
    assert_eq!(global_flag_arity("keep-going"), Some(0));
    assert_eq!(global_flag_arity("max-jobs"), Some(1));
    assert_eq!(global_flag_arity("option"), Some(2));
    assert_eq!(global_flag_arity("show-trace"), Some(0));
    assert_eq!(global_flag_arity("offline"), Some(0));
    assert_eq!(global_flag_arity("verbose"), Some(0));
  }

  #[test]
  fn well_known_commands_when_schema_available() {
    if !SCHEMA_AVAILABLE {
      return;
    }
    assert!(COMMANDS.contains(&"build"));
    assert!(COMMANDS.contains(&"develop"));
    assert!(COMMANDS.contains(&"eval"));
    assert!(COMMANDS.contains(&"flake"));
  }

  #[test]
  fn global_flags_not_empty_when_schema_available() {
    if !SCHEMA_AVAILABLE {
      return;
    }
    assert!(
      GLOBAL_FLAGS.len() > 100,
      "Expected 100+ global flags, got {}",
      GLOBAL_FLAGS.len()
    );
  }
}
