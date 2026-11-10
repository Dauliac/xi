#!/usr/bin/env bash
# Build xi from this working tree (via this flake's own devshell, so the
# build toolchain comes from `xi develop` rather than whatever happens to
# be on PATH already), then install the real activation hook in THIS
# interactive shell using that freshly-built binary - exactly what
# `xi develop activate <shell>` tells you to put in your rc file, just
# pointed at your local build instead of the installed xi.
#
# Usage: source scripts/devshell-test.sh
#
# Must be sourced, not executed: it changes PATH and installs the prompt
# hook in the calling shell. cd into any flake (this repo, one of its
# nested fixtures like tests/flake-module, or anything else) and the real
# activation / subshell / nesting logic runs against your local changes.

devshell_test_main() {
  local repo_root xi_bin shell_name activate_script

  repo_root="$(git -C "${PWD}" rev-parse --show-toplevel 2>/dev/null)"
  if [ -z "$repo_root" ]; then
    echo "devshell-test: not inside a git repo; cd into the xi checkout first" >&2
    return 1
  fi

  if ! command -v xi >/dev/null 2>&1; then
    echo "devshell-test: no 'xi' on PATH; needed to enter this flake's own devshell to build" >&2
    return 1
  fi

  echo "==> building xi (xi develop -c 'cargo build -p xi')" >&2
  if ! ( cd "$repo_root" && xi develop -c "cargo build -p xi" ); then
    echo "devshell-test: 'xi develop -c cargo build -p xi' failed" >&2
    return 1
  fi

  xi_bin="$repo_root/target/debug/xi"
  if [ ! -x "$xi_bin" ]; then
    echo "devshell-test: build finished but $xi_bin is missing" >&2
    return 1
  fi

  if [ -n "${ZSH_VERSION:-}" ]; then
    shell_name=zsh
  elif [ -n "${BASH_VERSION:-}" ]; then
    shell_name=bash
  else
    shell_name="${1:-bash}"
  fi

  # A stale __XI_SUPPRESS from an earlier plain `exit` out of a devshell
  # in this same directory, in this same shell, blocks re-activation
  # silently (no message, nothing) until cwd moves away and back. This
  # script's whole point is "activate now", so drop it.
  if [ -n "${__XI_SUPPRESS:-}" ]; then
    echo "devshell-test: clearing stale __XI_SUPPRESS=$__XI_SUPPRESS" >&2
    unset __XI_SUPPRESS
  fi

  # Put the local build first on PATH *before* generating the activation
  # script: it does its own `export __XI_BIN="$(command -v xi)"`, so this
  # is what makes that resolve to our build instead of the installed one.
  export PATH="$repo_root/target/debug:$PATH"

  activate_script="$("$xi_bin" develop activate "$shell_name")" || return 1
  eval "$activate_script"

  echo "devshell-test: $shell_name hook installed, using $xi_bin" >&2
  echo "cd into a flake to exercise it, e.g.: cd '$repo_root' or cd '$repo_root/tests/flake-module'" >&2
}

devshell_test_main "$@"
unset -f devshell_test_main
