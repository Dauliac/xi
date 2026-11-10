# xi devshell test harness - source this, don't execute it.
#
#   source scripts/devshell-test.sh
#
# Installs the `xi develop` prompt hook in your current bash/zsh shell,
# pointed at a local build instead of whatever `xi` is on PATH. Binary
# resolution priority:
#
#   1. ./target/debug/xi    (cargo build)
#   2. ./target/release/xi  (cargo build --release)
#   3. xi on PATH           (command -v xi)
#
# Repo root is resolved via `git rev-parse --show-toplevel` from the
# current directory, so you can source this from anywhere inside the repo.
#
# The tricky part: once trusted, the hook spawns a *subshell* (a fresh
# `bash`/`zsh` process) to hold the devshell env. That fresh process reads
# your real ~/.bashrc or ~/.zshrc on startup, not this script - so unless
# you've permanently installed `xi develop activate` there, the subshell
# comes up with no hook at all and nothing visibly happens. To test without
# touching your real dotfiles, this script shadows the bare `bash`/`zsh`
# command (as a shell function, for the lifetime of this session only) so
# that whenever xi's hook spawns one, it starts from a throwaway rcfile that
# sources your real rc *and* re-runs this script - so the hook, the binary
# resolution, and the shadowing itself all carry into nested devshells too.
#
# Re-sourcing is safe: it just re-resolves the binary and re-installs the
# hook (idempotent in zsh via add-zsh-hook; guarded in bash against
# duplicate PROMPT_COMMAND entries) and the shadow functions.

# Must be sourced: `return` only succeeds inside a function or a sourced
# file, so this works to detect direct execution in both bash and zsh.
if ! (return 0 2>/dev/null); then
  echo "devshell-test.sh must be sourced, not executed:" >&2
  echo "  source scripts/devshell-test.sh" >&2
  exit 1
fi

_xi_devshell_test_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

if [ -x "$_xi_devshell_test_root/target/debug/xi" ]; then
  __XI_BIN="$_xi_devshell_test_root/target/debug/xi"
  _xi_devshell_test_source="target/debug"
elif [ -x "$_xi_devshell_test_root/target/release/xi" ]; then
  __XI_BIN="$_xi_devshell_test_root/target/release/xi"
  _xi_devshell_test_source="target/release"
elif command -v xi >/dev/null 2>&1; then
  __XI_BIN="$(command -v xi)"
  _xi_devshell_test_source="PATH"
else
  echo "xi not found: no ./target/debug/xi, no ./target/release/xi, no xi on PATH" >&2
  unset _xi_devshell_test_root
  return 1
fi

export __XI_BIN
echo "xi devshell test: using $__XI_BIN ($_xi_devshell_test_source)" >&2

_xi_devshell_test_script="$_xi_devshell_test_root/scripts/devshell-test.sh"

if [ -n "${ZSH_VERSION:-}" ]; then
  _xi_hook() { eval "$("$__XI_BIN" develop prompt -s zsh --pid $$)"; }
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _xi_hook

  # Shadow bare `zsh` (what xi's subshell spawn command runs) so the nested
  # shell gets a throwaway ZDOTDIR: real ~/.zshrc first, then this script
  # again. `command zsh` below bypasses this function to avoid recursing.
  zsh() {
    local _xi_rcdir
    _xi_rcdir="$(mktemp -d)"
    {
      [ -f "$HOME/.zshrc" ] && printf 'source %q\n' "$HOME/.zshrc"
      printf 'source %q\n' "$_xi_devshell_test_script"
    } >"$_xi_rcdir/.zshrc"
    ZDOTDIR="$_xi_rcdir" command zsh "$@"
  }
elif [ -n "${BASH_VERSION:-}" ]; then
  _xi_hook() { eval "$("$__XI_BIN" develop prompt -s bash --pid $$)"; }
  case "${PROMPT_COMMAND:-}" in
  *_xi_hook*) : ;; # already installed, don't duplicate
  "") PROMPT_COMMAND="_xi_hook" ;;
  *) PROMPT_COMMAND="_xi_hook;$PROMPT_COMMAND" ;;
  esac

  # Same idea as the zsh case, via --rcfile (only honored for interactive,
  # non-login shells - exactly what xi's subshell spawn command starts).
  bash() {
    local _xi_rc
    _xi_rc="$(mktemp -d)/.bashrc"
    {
      [ -f "$HOME/.bashrc" ] && printf 'source %q\n' "$HOME/.bashrc"
      printf 'source %q\n' "$_xi_devshell_test_script"
    } >"$_xi_rc"
    command bash --rcfile "$_xi_rc" "$@"
  }
else
  echo "unsupported shell (not bash or zsh) - source this from bash or zsh" >&2
  unset _xi_devshell_test_root _xi_devshell_test_source _xi_devshell_test_script
  return 1
fi

unset _xi_devshell_test_root _xi_devshell_test_source
