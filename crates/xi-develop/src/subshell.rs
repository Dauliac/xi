//! Subshell spawning and environment setup.
//!
//! The subshell model is simple:
//! 1. Parent spawns a child shell with `__XI_IN_DEVSHELL=1` set
//! 2. Child loads user's rc normally (.zshrc, .bashrc, config.fish)
//! 3. User's rc has the activation hook, which detects `__XI_IN_DEVSHELL` and
//!    redirects to subshell mode (sources env.sh + hook.sh)
//! 4. env.sh PREPENDS nix paths to `$PATH` (doesn't replace it)
//! 5. Exit → child dies, parent resumes with original clean env
//!
//! Exit code convention:
//! - 200: "left flake directory" → parent stays alive (cd-out), and follows the
//!   user to the directory recorded in `__XI_CDOUT_FILE`
//! - Any other: user explicitly exited (`exit` / Ctrl-D) → the parent shell
//!   re-exits with the same status, cascading up through every async wrapper
//!   level so the top-level terminal (e.g. a herdr split) closes. Async
//!   activation is transparent on the way in, so an explicit exit is treated as
//!   "close this terminal", not "pop one invisible layer". (Sync `xi develop`
//!   entries are unaffected: that path execs the shell directly with no
//!   wrapper, so its exit returns to the invoking shell.)
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

/// Env var the parent sets, pointing the child subshell at a scratch file to
/// record its final `$PWD` into before a cd-out exit. Lets the parent follow
/// the user to wherever they `cd`'d, instead of snapping back to the
/// directory it was in before spawning the subshell.
pub const CDOUT_FILE_VAR: &str = "__XI_CDOUT_FILE";

/// Generate the shell snippet that records the current directory (so the
/// parent can follow the user out) and exits with the cd-out sentinel.
#[must_use]
pub fn generate_cd_out_exit(shell: ShellType) -> String {
  let sentinel = EXIT_CD_OUT;
  match shell {
    ShellType::Bash | ShellType::Zsh => {
      // `>|` (not `>`): the file already exists (the parent `mktemp`'d it
      // before spawning), and zsh's `NO_CLOBBER` option makes a plain `>`
      // onto an existing file fail with "file exists"  -  silently dropping
      // the write, which made the parent fall back to the pre-entry
      // directory instead of following the user out. `>|` force-writes
      // regardless of that option, and is a no-op difference under bash.
      format!(
        "[ -n \"${CDOUT_FILE_VAR}\" ] && pwd >| \"${CDOUT_FILE_VAR}\"\nexit \
         {sentinel}\n"
      )
    },
    ShellType::Fish => {
      format!(
        "if test -n \"${CDOUT_FILE_VAR}\"; pwd > \"${CDOUT_FILE_VAR}\"; \
         end\nexit {sentinel}\n"
      )
    },
  }
}

/// Generate the command to spawn a devshell subshell.
///
/// Sets `__XI_DEPTH` (incremented) so env files scope their cleanup
/// to the correct nesting level. On cd-out exit (status 200) the parent
/// follows the recorded directory and stays alive; on any other exit the
/// parent re-exits with the same status, cascading the exit up to the
/// top-level terminal (see the module docs' exit-code convention).
#[must_use]
pub fn generate_spawn_command(
  shell: ShellType,
  flake_root: &Path,
  original_cwd: &Path,
  xi_bin: &str,
) -> String {
  let root = flake_root.display();
  let cwd = original_cwd.display();
  let sentinel = EXIT_CD_OUT;

  match shell {
    ShellType::Bash | ShellType::Zsh => {
      let shell_name = shell.name();
      format!(
        "\
__xi_cwd_save=\"$PWD\"
cd '{cwd}'
__xi_cdout=\"$(mktemp)\"
{CDOUT_FILE_VAR}=\"$__xi_cdout\" __XI_IN_DEVSHELL=1 __XI_FLAKE_ROOT='{root}' \
         __XI_BIN='{xi_bin}' __XI_DEPTH=$((${{__XI_DEPTH:-0}}+1)) {shell_name}
__xi_status=$?
if [ \"$__xi_status\" -eq {sentinel} ]; then
  if [ -s \"$__xi_cdout\" ]; then
    cd \"$(cat \"$__xi_cdout\")\"
  else
    cd \"$__xi_cwd_save\"
  fi
  rm -f \"$__xi_cdout\"
else
  rm -f \"$__xi_cdout\"
  exit \"$__xi_status\"
fi
"
      )
    },
    ShellType::Fish => {
      format!(
        "\
set -l __xi_cwd_save $PWD
cd '{cwd}'
set -l __xi_cdout (mktemp)
{CDOUT_FILE_VAR}=$__xi_cdout __XI_IN_DEVSHELL=1 __XI_FLAKE_ROOT='{root}' \
         __XI_BIN='{xi_bin}' __XI_DEPTH=(math (set -q __XI_DEPTH; and echo \
         $__XI_DEPTH; or echo 0) + 1) fish
set -l __xi_status $status
if test $__xi_status -eq {sentinel}
  if test -s $__xi_cdout
    cd (cat $__xi_cdout)
  else
    cd $__xi_cwd_save
  end
  rm -f $__xi_cdout
else
  rm -f $__xi_cdout
  exit $__xi_status
end
"
      )
    },
  }
}
