use color_eyre::Result;

use crate::shell::ShellType;

/// Generate and print the shell activation script.
///
/// The activation script is minimal: it resolves the xi binary, defines a
/// prompt hook that delegates to `xi develop prompt`, and installs the hook.
/// All flake detection, trust, environment management, and deactivation logic
/// lives in the Rust binary rather than in generated shell code.
///
/// # Errors
///
/// Returns an error if the shell type is not recognized.
pub fn generate_activation_script(shell_str: &str) -> Result<()> {
  let shell = ShellType::parse(shell_str)?;
  let script = match shell {
    ShellType::Bash => BASH_ACTIVATION,
    ShellType::Zsh => ZSH_ACTIVATION,
    ShellType::Fish => FISH_ACTIVATION,
  };
  print!("{script}");
  Ok(())
}

const BASH_ACTIVATION: &str = r#"# xi develop activation for bash
export __XI_BIN="$(command -v xi)"
_xi_hook() { eval "$("$__XI_BIN" develop prompt -s bash --pid $$)"; }
PROMPT_COMMAND="_xi_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
"#;

const ZSH_ACTIVATION: &str = r#"# xi develop activation for zsh
export __XI_BIN="$(command -v xi)"
_xi_hook() { eval "$("$__XI_BIN" develop prompt -s zsh --pid $$)"; }
autoload -Uz add-zsh-hook
add-zsh-hook precmd _xi_hook
"#;

const FISH_ACTIVATION: &str = r"# xi develop activation for fish
set -g __XI_BIN (command -v xi)
function _xi_hook --on-event fish_prompt
    eval ($__XI_BIN develop prompt -s fish --pid %self)
end
";
