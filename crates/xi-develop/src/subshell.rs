//! Subshell spawning and environment setup.
//!
//! The subshell model is simple:
//! 1. Parent spawns a child shell with `__XI_IN_DEVSHELL=1` set
//! 2. Child loads user's rc normally (.zshrc, .bashrc, config.fish)
//! 3. User's rc has the activation hook, which detects `__XI_IN_DEVSHELL` and
//!    redirects to subshell mode (sources env.sh + hook.sh)
//! 4. env.sh PREPENDS nix paths to `$PATH` (doesn't replace it)
//! 5. Exit → child dies. For a nested child-flake spawn, the parent flake's
//!    shell resumes with its original clean env. For the top-level spawn
//!    (directly off the real terminal shell), the exit propagates and the
//!    terminal closes instead of resuming a bare prompt.
//!
//! Exit code convention:
//! - 200: "left flake directory" → parent stays alive (cd-out)
//! - Any other, top-level spawn: user explicitly exited the devshell that
//!   was spawned directly off the real terminal shell → that shell exits
//!   too, closing the terminal instead of popping back to a bare prompt.
//! - Any other, nested spawn: user explicitly exited a *nested* child-flake
//!   devshell → the parent flake's shell stays alive and sets a suppress
//!   flag (prevents re-entry loop without killing the terminal).
//!
//! Nesting:
//! - `__XI_DEPTH` increments per level (parent=1, child=2, …)
//! - Each level's env uses `__XI_INJECTED_VARS_<depth>` so cleanup only affects
//!   its own vars, preserving parent devshell env
//! - Process isolation restores parent env when child shell exits

use std::path::Path;

use crate::shell::ShellType;

/// Sentinel exit code meaning "left flake directory, don't kill parent."
/// The prompt hook uses `exit 200` when CWD leaves the flake root.
pub const EXIT_CD_OUT: u8 = 200;

/// Generate the command to spawn a devshell subshell.
///
/// Sets `__XI_DEPTH` (incremented) so env files scope their cleanup
/// to the correct nesting level.
///
/// `top_level` distinguishes the real-terminal-facing spawn (from
/// `xi develop`'s prompt hook, gated by `xi develop trust`) from a nested
/// child-flake spawn (from an already-active devshell). On non-cd-out exit:
/// top-level propagates the exit to close the terminal; nested sets
/// `__XI_SUPPRESS` so the parent flake's devshell resumes instead.
#[must_use]
pub fn generate_spawn_command(
  shell: ShellType,
  flake_root: &Path,
  original_cwd: &Path,
  xi_bin: &str,
  top_level: bool,
) -> String {
  let root = flake_root.display();
  let cwd = original_cwd.display();
  let sentinel = EXIT_CD_OUT;

  match shell {
    ShellType::Bash | ShellType::Zsh => {
      let shell_name = shell.name();
      let on_explicit_exit = if top_level {
        "exit $__xi_status".to_string()
      } else {
        format!("export __XI_SUPPRESS='{root}'")
      };
      format!(
        "\
__xi_cwd_save=\"$PWD\"
cd '{cwd}'
__XI_IN_DEVSHELL=1 __XI_FLAKE_ROOT='{root}' __XI_BIN='{xi_bin}' \
         __XI_DEPTH=$((${{__XI_DEPTH:-0}}+1)) {shell_name}
__xi_status=$?
cd \"$__xi_cwd_save\"
if [ $__xi_status -ne {sentinel} ]; then {on_explicit_exit}; fi
"
      )
    },
    ShellType::Fish => {
      let on_explicit_exit = if top_level {
        "exit $__xi_status".to_string()
      } else {
        format!("set -gx __XI_SUPPRESS '{root}'")
      };
      format!(
        "\
set -l __xi_cwd_save $PWD
cd '{cwd}'
__XI_IN_DEVSHELL=1 __XI_FLAKE_ROOT='{root}' __XI_BIN='{xi_bin}' \
         __XI_DEPTH=(math (set -q __XI_DEPTH; and echo $__XI_DEPTH; or echo \
         0) + 1) fish
set -l __xi_status $status
cd $__xi_cwd_save
if test $__xi_status -ne {sentinel}; {on_explicit_exit}; end
"
      )
    },
  }
}
