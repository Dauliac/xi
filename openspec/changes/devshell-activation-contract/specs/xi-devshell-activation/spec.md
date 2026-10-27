# xi-devshell-activation (delta)

## ADDED Requirements

### Requirement: Well-known activation environment variables

The activation surface MUST publish exactly the following environment variables
into the devshell child process, with the documented semantics:

- `__XI_IN_DEVSHELL=1` — presence indicates the current process is running
  inside an xi-managed devshell subshell.
- `__XI_FLAKE_ROOT=<canonical path>` — canonical directory containing the flake
  root that produced this subshell.
- `__XI_BIN=<path>` — the resolved absolute path to the xi binary that spawned
  this subshell.
- `__XI_DEPTH=<n>` — the current nesting depth (1 for the first devshell,
  incremented per nested spawn).
- `__XI_INJECTED_VARS_<depth>=<comma-list>` — the set of env-var names this
  depth-level has added to the environment (used for cleanup on exit).
- `__XI_LOCK_HASH=<hex>` — the hash of `flake.lock` at spawn time, used by the
  prompt-hook shortcut path.
- `__XI_ORIG_PATH=<value>` — the parent shell's `$PATH` at first activation in
  this shell lifetime; captured once and never mutated.

No other `__XI_*` variables MAY be published by the activation surface without a
spec update.

#### Scenario: Enter a devshell from a clean shell

- **WHEN** a devshell subshell is spawned by the prompt hook
- **THEN** every listed variable is set with a non-empty value, and no other
  `__XI_*` variable is set

#### Scenario: Enter a nested devshell

- **WHEN** a devshell subshell is spawned from inside an existing depth-1
  devshell
- **THEN** `__XI_DEPTH=2`, `__XI_INJECTED_VARS_2` is created for the child
  level, `__XI_INJECTED_VARS_1` is preserved unchanged, and `__XI_ORIG_PATH`
  remains the outermost captured value

### Requirement: PATH prepend-not-replace with original capture

The devshell env file MUST prepend nix store paths to `$PATH` and MUST NOT
remove, reorder, or replace any pre-existing PATH entries. On first activation
in a shell lifetime, the parent's `$PATH` MUST be captured verbatim into
`__XI_ORIG_PATH` so that de-activation can subtract rather than reconstruct.

#### Scenario: PATH prepend preserves user PATH

- **WHEN** a shell whose PATH contains `/home/u/bin:/usr/local/bin:/usr/bin`
  enters a devshell whose nix paths are `/nix/store/A/bin` and
  `/nix/store/B/bin`
- **THEN** the resulting `$PATH` starts with
  `/nix/store/A/bin:/nix/store/
  B/bin:` and every original entry appears after
  in its original order

#### Scenario: `__XI_ORIG_PATH` is captured exactly once

- **WHEN** a shell enters, exits, and re-enters a devshell without dying
- **THEN** `__XI_ORIG_PATH` retains the value captured on the first activation
  and is not mutated on the second

### Requirement: Depth-scoped injected-var registry

Each nesting level MUST maintain its cleanup registry under a distinct env var
(`__XI_INJECTED_VARS_<depth>`). Exiting a devshell at depth N MUST clean up only
the vars listed in `__XI_INJECTED_VARS_<N>`; the parent level's registry MUST
NOT be read or modified.

#### Scenario: Exit nested devshell without disturbing parent

- **WHEN** the user exits a depth-2 devshell (Ctrl-D) that had added `FOO_BAR`
  and `BAZ_QUX` to its environment
- **THEN** `FOO_BAR` and `BAZ_QUX` are unset, `__XI_INJECTED_VARS_2` is unset,
  `__XI_INJECTED_VARS_1` is unchanged, and every variable the depth-1 level had
  injected is still present

### Requirement: Exit-code conventions distinguish cd-out from user-exit

The prompt hook and subshell spawn wrapper MUST agree that:

- Exit code `200` = "the shell exited because CWD left the flake root". The
  parent MUST NOT set `__XI_SUPPRESS`; re-entry MAY happen automatically on a
  subsequent `cd` back into the flake.
- Any other non-zero exit = "the user explicitly exited the shell". The parent
  MUST set `__XI_SUPPRESS=<flake_root>` so the very next prompt tick at the same
  location does not re-enter and produce an exit loop.
- Exit code `0` from the subshell is treated the same as any other non-`200`
  code (explicit exit); it also sets `__XI_SUPPRESS`.

#### Scenario: cd-out from a devshell

- **WHEN** the user changes directory to a path outside the flake root and the
  subshell exits `200`
- **THEN** the parent shell does not set `__XI_SUPPRESS` and the next `cd` back
  into the flake root triggers re-entry

#### Scenario: Explicit exit from a devshell

- **WHEN** the user types `exit` inside the devshell and the subshell returns
  exit code `0`
- **THEN** the parent sets `__XI_SUPPRESS=<flake_root>` and the next prompt tick
  at the same location does NOT re-spawn a devshell

#### Scenario: Suppress clears on cd-out then cd-in

- **WHEN** `__XI_SUPPRESS=<flake_root>` is set, the user `cd`s out of the flake
  root and then `cd`s back in
- **THEN** the suppress flag is cleared and the next prompt tick enters the
  devshell normally

### Requirement: Session-identity SKIP_VARS invariant

The following environment variables MUST NOT be overwritten by the devshell env
even if `nix print-dev-env --json` returns values for them: `HOME`, `USER`,
`LOGNAME`, `SHELL`, `TERM`, `COLORTERM`, `TERM_PROGRAM`, `DISPLAY`,
`WAYLAND_DISPLAY`, `XDG_SESSION_TYPE`, `XDG_RUNTIME_DIR`, `XDG_DATA_DIRS`,
`DBUS_SESSION_BUS_ADDRESS`, `SSH_AUTH_SOCK`, `SSH_AGENT_PID`, `LANG`,
`LANGUAGE`, `LC_ALL`, `LC_CTYPE`, `TZ`, `PAGER`, `EDITOR`, `VISUAL`, `PWD`,
`OLDPWD`, `SHLVL`, `TMPDIR`, `_`.

#### Scenario: Nix env contains SSH_AUTH_SOCK

- **WHEN** `nix print-dev-env` returns a value for `SSH_AUTH_SOCK` (from a
  package's shell hook)
- **THEN** the devshell env file does not overwrite `SSH_AUTH_SOCK`, and the
  child shell continues to see the parent's value

#### Scenario: PATH is handled specially, not skipped

- **WHEN** `nix print-dev-env` returns a value for `PATH`
- **THEN** it is processed by the PATH-prepend logic (see prior requirement)
  rather than being either skipped outright or overwritten

### Requirement: Nix build-time variables must not leak

The following build-time variables from `nix print-dev-env` MUST NOT be
propagated into the interactive shell: `NIX_BUILD_TOP`, `NIX_LOG_DIR`,
`NIX_BUILD_CORES`, `NIX_ENFORCE_PURITY`, `NIX_ENFORCE_NO_NATIVE`, `NIX_STORE`,
`TEMP`, `TEMPDIR`, `TMP`.

#### Scenario: Post-activation env inspection

- **WHEN** a devshell is activated and the child shell inspects its environment
- **THEN** none of the listed build-time variables are set

### Requirement: Activation script minimality

The output of `xi develop activate <shell>` MUST contain only shell code that
(a) resolves the xi binary path into `__XI_BIN`, (b) defines a prompt-hook
function that shells out to `xi develop prompt`, and (c) installs that function
using the shell's native prompt-hook mechanism (`PROMPT_COMMAND`, `precmd`,
`fish_prompt`). It MUST NOT contain any filesystem probing, trust checking,
flake detection, or environment mutation beyond `__XI_BIN` assignment.

#### Scenario: Activation script size

- **WHEN** `xi develop activate bash` (or `zsh` / `fish`) is run
- **THEN** the emitted script is at most five non-blank lines and contains no
  `find`, `test -e`, `if`, or `case` constructs outside the hook body itself

#### Scenario: All decisions happen in Rust

- **WHEN** a prompt-hook tick fires
- **THEN** control immediately transfers to the Rust `xi develop prompt` binary;
  the shell-side hook does not perform its own flake, trust, or cache decisions

### Requirement: Shell adapter surface is exhaustive and additive

Adding support for a new shell MUST require code changes only inside `shell.rs`
(a new `ShellType` variant), `activate.rs` (a new activation snippet),
`subshell.rs` (a new spawn-command branch), and `env_file.rs` (a new emitter
branch). No changes to `enter.rs`, `hook/`, or `daemon/` MAY be required by
adding a new shell.

#### Scenario: Adding a new shell adapter

- **WHEN** a developer adds support for a new shell by extending the four named
  modules
- **THEN** the codebase compiles and every existing test continues to pass
  without further modification
