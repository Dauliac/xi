# Change: devshell-activation-contract

## Why

The devshell "activation" surface is the boundary where xi transitions a user's
plain shell into a devshell subshell with the flake's environment applied. Today
this surface is spread across four modules:

- `activate.rs` — generates the shell rc snippet that installs the prompt hook
  (`xi develop activate {bash,zsh,fish}`).
- `enter.rs` — the synchronous `xi develop` entry command that builds, evals,
  and execs a subshell.
- `subshell.rs` — the spawn-command generator that fires up the child shell with
  the correct sentinel + depth env vars.
- `env_file.rs` — writes the actual `env.sh` / `hook.sh` files that the child
  shell sources.

There is a **contract** between these modules — env-var names (`__XI_*`), exit
codes (200 = cd-out), depth-scoped injected-var registries, PATH
prepend-not-replace semantics, sourced-file layout — but it has never been
specified. As a result:

- Regression fixes to one module have historically broken another (e.g. a fix in
  `env_file.rs`'s cleanup broke `subshell.rs`'s nesting because the registry key
  changed).
- Adding a new shell (currently: bash, zsh, fish; requested: nushell) means
  reverse-engineering the contract from four modules.
- The prompt-hook budget work in `xi-devshell-daemon` implicitly relies on
  `__XI_IN_DEVSHELL` and `__XI_LOCK_HASH` being set correctly by activation —
  but nothing enforces that.

## What Changes

- Introduce capability `xi-devshell-activation` that owns the shared contract:

  - **Well-known env vars** the activation surface publishes into the child
    shell: `__XI_IN_DEVSHELL`, `__XI_FLAKE_ROOT`, `__XI_BIN`, `__XI_DEPTH`,
    `__XI_INJECTED_VARS_<depth>`, `__XI_LOCK_HASH`, `__XI_SUPPRESS`,
    `__XI_ORIG_PATH`. Names, semantics, lifetimes, and cleanup ownership
    documented.

  - **Exit-code conventions** the subshell/hook boundary agrees on:
    - `200` = "shell exited because CWD left the flake root" — parent MUST NOT
      set the suppress flag and MUST allow re-entry on next `cd` in.
    - any other non-zero = "user explicitly exited (e.g. `exit`, `logout`,
      Ctrl-D)" — parent MUST set `__XI_SUPPRESS=<flake_root>` to prevent an
      immediate re-entry loop; the suppress flag clears on `cd` out then in
      again.

  - **PATH discipline**: `env.sh` MUST prepend nix paths to `$PATH` — never
    replace or reorder existing entries. The original PATH MUST be captured into
    `__XI_ORIG_PATH` on first activation (once per shell lifetime) so that
    de-activation is a subtraction, not a lookup.

  - **Depth-scoped cleanup registry**: nesting devshells MUST NOT cause a child
    to clean up the parent's injected vars on exit. The registry env var MUST be
    `__XI_INJECTED_VARS_<depth>` where `<depth>` = current `$__XI_DEPTH`. Each
    level owns and cleans exactly its own registry.

  - **SKIP_VARS invariant**: env vars that describe the user's session identity
    (`HOME`, `USER`, `SHELL`, `TERM`, `SSH_AUTH_SOCK`, `WAYLAND_DISPLAY`, etc.)
    MUST NOT be overwritten by the devshell env, even if `nix print-dev-env`
    includes them. The full skip list is normative.

  - **Nix build-time vars discarded**: `NIX_BUILD_TOP`, `NIX_LOG_DIR`,
    `NIX_BUILD_CORES`, `NIX_ENFORCE_*`, `NIX_STORE`, `TEMP*`, `TMP*` MUST NOT
    leak into the interactive shell.

  - **Activation script minimality**: the `xi develop activate <shell>` output
    MUST be a small, static snippet that (1) locates the xi binary, (2) defines
    a prompt hook, (3) installs it — no flake detection, no trust logic, no env
    manipulation in generated shell code. All decisions live in the Rust binary.

- Formalise the **shell adapter surface**: adding a new shell means (a)
  extending `ShellType`, (b) providing an activation snippet, (c) providing a
  spawn-command generator, (d) providing an env-file emitter. No other module
  changes.

## Impact

- **Affected specs**: `xi-devshell-activation` (new capability — first delta
  introduces its requirements).
- **Affected code**: `crates/xi-develop/src/activate.rs`,
  `crates/xi-develop/src/subshell.rs`, `crates/xi-develop/src/env_file.rs`,
  `crates/xi-develop/src/enter.rs`, `crates/xi-develop/src/shell.rs`. Existing
  behaviour is already correct in most places; this change codifies the
  invariants so regressions surface as spec-scenario failures rather than at
  runtime.
- **Coordination with `async-daemon-cqrs`**: the client-side shortcut path in
  `xi-devshell-daemon` reads `__XI_IN_DEVSHELL`, `__XI_FLAKE_ROOT`, and
  `__XI_LOCK_HASH` — this spec is the authoritative source for those variables'
  names and semantics.
- **Coordination with `devshell-trust-model`**: `xi develop activate` output
  never inspects the trust store; trust is checked at prompt-hook time inside
  the Rust binary. This spec's minimality clause enforces that separation.
- **Breaking?**: No. Env-var names, exit codes, and file layout are preserved
  from current behaviour.
- **Migration**: None.

## Success Criteria

- **SC-A-001**: `xi develop activate bash|zsh|fish` output is ≤ 5 lines of shell
  code and contains no flake detection, no trust check, and no environment
  mutation beyond `PROMPT_COMMAND` / `precmd` / `fish_prompt` wiring.
- **SC-A-002**: Entering a devshell prepends nix paths to `$PATH` and leaves
  every pre-existing path entry present and in its original relative order.
- **SC-A-003**: Exiting a nested devshell (`depth=2`) cleans up exactly the
  depth-2 injected vars and leaves depth-1's env intact.
- **SC-A-004**: A shell whose CWD leaves the flake root causes the subshell to
  exit `200`; the parent does not set `__XI_SUPPRESS` and re-entry happens
  automatically on `cd` back in.
- **SC-A-005**: A user typing `exit` inside a devshell causes the parent to set
  `__XI_SUPPRESS=<flake_root>`; the next prompt tick in the same location does
  NOT re-enter until the user `cd`s out then back in.
- **SC-A-006**: `HOME`, `USER`, `SHELL`, `TERM`, `SSH_AUTH_SOCK`,
  `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and every other member of the
  session-identity SKIP list are preserved from the parent env even when
  `nix print-dev-env` includes them.
- **SC-A-007**: `NIX_BUILD_TOP` and every other build-time nix var are absent
  from the interactive shell after activation.
- **SC-A-008**: Adding a new shell adapter requires code changes only to
  `shell.rs`, `activate.rs`, `subshell.rs`, and `env_file.rs`; no changes to
  `enter.rs`, `hook/`, or `daemon/` are required.

## Assumptions

- Shell adapters remain the exhaustive dispatch pattern (`ShellType` enum); a
  plugin registry is out of scope.
- Nushell, elvish, and other non-POSIX-adjacent shells are out of scope for this
  delta; they can be added later without changing the contract.
- The prompt hook installed by `xi develop activate` is the only supported
  activation trigger. `direnv` integration is a separate concern and lives in
  its own future proposal.
- `nix print-dev-env --json` remains the authoritative source of the devshell
  env; changes to that upstream format are handled at the parser boundary
  (`env_file.rs::DevEnv`).
