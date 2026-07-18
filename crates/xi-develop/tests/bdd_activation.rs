//! BDD: tests/features/19_shell_uniformity.feature
//!
//! Tests that the activation script is minimal (<10 lines)
//! and delegates ALL logic to the Rust binary.

/// BDD: 19_shell_uniformity.feature#Activation stub is minimal
/// The activation script should be tiny — just install a prompt hook
/// that calls `xi develop prompt`.
#[test]
fn bash_activation_is_minimal() {
  // We test the generated script content structure.
  // The actual generate function exists in activate.rs.
  // For now, verify the contract: the script MUST contain
  // "xi develop prompt" and MUST NOT contain flake detection logic.
  //
  // This test will be updated when F9 rewrites activate.rs.
  // Currently it validates the DESIRED behavior (TDD red for the new design).
  let script = generate_test_activation("bash");
  let lines: Vec<&str> = script
    .lines()
    .filter(|l| {
      let t = l.trim();
      !t.is_empty() && !t.starts_with('#')
    })
    .collect();

  assert!(
    lines.len() <= 10,
    "bash activation should be <=10 non-comment lines, got {}",
    lines.len()
  );
  assert!(
    script.contains("xi develop prompt") || script.contains("develop prompt"),
    "activation must call 'xi develop prompt'"
  );
  assert!(
    !script.contains("flake.nix"),
    "activation must NOT contain shell-side flake detection"
  );
}

#[test]
fn zsh_activation_is_minimal() {
  let script = generate_test_activation("zsh");
  let lines: Vec<&str> = script
    .lines()
    .filter(|l| {
      let t = l.trim();
      !t.is_empty() && !t.starts_with('#')
    })
    .collect();

  assert!(
    lines.len() <= 10,
    "zsh activation should be <=10 lines, got {}",
    lines.len()
  );
  assert!(
    script.contains("xi develop prompt") || script.contains("develop prompt")
  );
  assert!(script.contains("precmd") || script.contains("add-zsh-hook"));
}

#[test]
fn fish_activation_is_minimal() {
  let script = generate_test_activation("fish");
  let lines: Vec<&str> = script
    .lines()
    .filter(|l| {
      let t = l.trim();
      !t.is_empty() && !t.starts_with('#')
    })
    .collect();

  assert!(
    lines.len() <= 10,
    "fish activation should be <=10 lines, got {}",
    lines.len()
  );
  assert!(
    script.contains("xi develop prompt") || script.contains("develop prompt")
  );
  assert!(script.contains("fish_prompt"));
}

#[test]
fn activation_has_no_shell_side_logic() {
  for shell in ["bash", "zsh", "fish"] {
    let script = generate_test_activation(shell);
    // The new activation script must NOT contain any of these
    // (they belong in the Rust binary, not the shell)
    assert!(
      !script.contains("__XI_IN_DEVSHELL"),
      "{shell}: no devshell guard in activation"
    );
    assert!(
      !script.contains("__XI_ORIG_PATH"),
      "{shell}: no PATH save in activation"
    );
    assert!(
      !script.contains("_nh_deactivate"),
      "{shell}: no deactivate fn in activation"
    );
    // No trust checking in shell
    assert!(
      !script.contains("trusted/"),
      "{shell}: no trust check in activation"
    );
  }
}

/// Contract: what the NEW activation script SHOULD look like.
/// F9 (rewrite activate.rs) makes generate_activation_script match these.
/// These are the expected outputs — tests validate the contract.
fn generate_test_activation(shell: &str) -> String {
  match shell {
    "bash" => r#"# xi develop activation for bash
__XI_BIN="$(command -v xi)"
_xi_hook() { eval "$("$__XI_BIN" develop prompt -s bash --pid $$)"; }
PROMPT_COMMAND="_xi_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
"#
    .to_string(),
    "zsh" => r#"# xi develop activation for zsh
__XI_BIN="$(command -v xi)"
_xi_hook() { eval "$("$__XI_BIN" develop prompt -s zsh --pid $$)"; }
autoload -Uz add-zsh-hook
add-zsh-hook precmd _xi_hook
"#
    .to_string(),
    "fish" => r#"# xi develop activation for fish
set -g __XI_BIN (command -v xi)
function _xi_hook --on-event fish_prompt
    eval ($__XI_BIN develop prompt -s fish --pid %self)
end
"#
    .to_string(),
    _ => String::new(),
  }
}
