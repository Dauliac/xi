//! BDD: tests/features/01_first_entry_trusted.feature
//! BDD: tests/features/19_shell_uniformity.feature
//!
//! Tests for subshell init script generation.
//! These verify the structure of the generated init scripts.

/// BDD: 01_first_entry_trusted.feature#Subshell initialization sequence
/// The init script must contain all required elements.
#[test]
fn bash_init_script_has_required_elements() {
  let script = expected_bash_init();

  // Must source user's rc
  assert!(script.contains(".bashrc"), "must source ~/.bashrc");
  // Must source env file
  assert!(
    script.contains("env-default.current.sh"),
    "must source env file"
  );
  // Must source hook file
  assert!(
    script.contains("hook-default.current.sh"),
    "must source hook file"
  );
  // Must install subshell prompt hook
  assert!(
    script.contains("xi develop prompt --subshell")
      || script.contains("develop prompt --subshell"),
    "must install subshell prompt hook"
  );
  // Must install EXIT trap
  assert!(
    script.contains("xi develop prompt --exit")
      || script.contains("develop prompt --exit"),
    "must install EXIT trap"
  );
  // Must cd to original CWD
  assert!(script.contains("/original/cwd"), "must cd to original CWD");
}

#[test]
fn zsh_init_script_has_required_elements() {
  let script = expected_zsh_init();

  assert!(script.contains(".zshrc"), "must source .zshrc");
  assert!(
    script.contains("env-default.current.sh"),
    "must source env file"
  );
  assert!(
    script.contains("hook-default.current.sh"),
    "must source hook file"
  );
  assert!(
    script.contains("develop prompt --subshell"),
    "must install subshell prompt hook"
  );
  assert!(
    script.contains("develop prompt --exit"),
    "must install EXIT trap"
  );
  assert!(script.contains("/original/cwd"), "must cd to original CWD");
}

#[test]
fn fish_init_script_has_required_elements() {
  let script = expected_fish_init();

  assert!(
    script.contains("env-default.current.fish"),
    "must source env file"
  );
  assert!(
    script.contains("develop prompt --subshell"),
    "must install subshell prompt hook"
  );
  assert!(
    script.contains("develop prompt --exit"),
    "must install EXIT trap"
  );
  assert!(script.contains("/original/cwd"), "must cd to original CWD");
}

/// BDD: 19_shell_uniformity.feature#All shells use the subshell model
#[test]
fn all_shells_have_exit_trap() {
  for script in [
    expected_bash_init(),
    expected_zsh_init(),
    expected_fish_init(),
  ] {
    assert!(
      script.contains("develop prompt --exit"),
      "all init scripts must have EXIT trap"
    );
  }
}

// Expected init script templates (what F10 should produce).
// These are the contract — F10 makes generate_init_script match these.

fn expected_bash_init() -> String {
  r#"# xi develop subshell init (bash)
[ -f ~/.bashrc ] && source ~/.bashrc
source '/state/env-default.current.sh'
source '/state/hook-default.current.sh'
_xi_hook() { eval "$("$__XI_BIN" develop prompt --subshell -s bash --pid $$)"; }
PROMPT_COMMAND="_xi_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
trap '"$__XI_BIN" develop prompt --exit --pid $$' EXIT
cd '/original/cwd'
"#
  .to_string()
}

fn expected_zsh_init() -> String {
  r#"# xi develop subshell init (zsh)
[ -f "${ZDOTDIR:-$HOME}/.zshrc" ] && source "${ZDOTDIR:-$HOME}/.zshrc"
source '/state/env-default.current.sh'
source '/state/hook-default.current.sh'
_xi_hook() { eval "$("$__XI_BIN" develop prompt --subshell -s zsh --pid $$)"; }
autoload -Uz add-zsh-hook
add-zsh-hook precmd _xi_hook
trap '"$__XI_BIN" develop prompt --exit --pid $$' EXIT
cd '/original/cwd'
"#
  .to_string()
}

fn expected_fish_init() -> String {
  r#"# xi develop subshell init (fish)
source '/state/env-default.current.fish'
source '/state/hook-default.current.sh'
function _xi_hook --on-event fish_prompt
    eval ($__XI_BIN develop prompt --subshell -s fish --pid %self)
end
function __xi_exit --on-event fish_exit
    eval ($__XI_BIN develop prompt --exit --pid %self)
end
cd '/original/cwd'
"#
  .to_string()
}
