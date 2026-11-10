# Tasks: devshell-activation-contract

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads.

## 1. Specification (P1)

- [ ] 1.1 Land the spec delta under `specs/xi-devshell-activation/spec.md`.
- [ ] 1.2 Add a top-of-file doc comment to each of `activate.rs`, `subshell.rs`,
      `env_file.rs`, `enter.rs` linking to the spec section that owns their
      contract.
- [ ] 1.3 Cross-link with `xi-devshell-daemon` — the shortcut-path env variables
      live in this spec.

## 2. Contract enforcement tests (P1)

- [ ] 2.1 BDD `bdd_activation_env_vars`: after
      `xi develop activate <shell> |
      source`-equivalent, assert every
      declared `__XI_*` var is set with the documented lifetime.
- [ ] 2.2 BDD `bdd_activation_path_prepend`: enter a devshell, assert that every
      pre-existing PATH entry is still present and in relative order, with nix
      entries prepended.
- [ ] 2.3 BDD `bdd_activation_skip_vars`: verify HOME/USER/SHELL/TERM/
      SSH_AUTH_SOCK/etc. are not overwritten even if `nix print-dev-env` returns
      them.
- [ ] 2.4 BDD `bdd_activation_no_build_leak`: assert
      NIX_BUILD_TOP/NIX_LOG_DIR/NIX_BUILD_CORES/NIX_ENFORCE_*/NIX_STORE/
      TEMP/TMP are absent post-activation.
- [ ] 2.5 BDD `bdd_activation_exit_codes`: cd-out yields exit 200 and no
      suppress; explicit exit yields non-200 and sets suppress.
- [ ] 2.6 BDD `bdd_activation_nesting`: exit a depth-2 devshell, assert
      depth-1's injected-var registry is untouched and depth-1's env is intact.

## 3. Activation script minimality (P1)

- [ ] 3.1 Assert `xi develop activate bash|zsh|fish` output is ≤ 5 non-blank
      shell lines and contains no `find`/`test -e`/`case`/`if` that touches the
      filesystem or user env beyond hook installation.
- [ ] 3.2 BDD `bdd_activate_script_static`: golden-file compare each shell's
      activation script; drift fails CI. Reuses the same golden-file test
      pattern used by the v3 protocol schema.

## 4. Shell adapter interface (P2)

- [ ] 4.1 Codify the four extension points (`ShellType` variant, activation
      snippet, spawn command, env-file emitter) as a `#[cfg(test)]` trait check
      so new shells fail to compile if they miss one.
- [ ] 4.2 Document the extension procedure in a top-of-file doc comment on
      `shell.rs`.

## 5. Daemon-mediated re-source protocol (P1, newly discovered)

- [ ] 5.1 Land the "Daemon-mediated re-source protocol" requirement in
      `specs/xi-devshell-activation/spec.md` (already drafted in this delta) and
      cross-link it from `daemon/server.rs`'s response-builder region and
      `daemon/shell_registry.rs` with a top-of-file doc comment.
- [ ] 5.2 BDD `bdd_activation_resource_generations`: a consumer behind on env
      generation is told to re-source; a consumer on the current generation is
      not; env and hook generations advance independently.
- [ ] 5.3 BDD `bdd_activation_symlink_atomicity`: two evaluations completing in
      quick succession before a consumer re-sources resolve to the latest env
      file via the current-symlink, never an intermediate one.

## 6. Lock-hash and original-PATH gaps (P1, newly discovered)

- [ ] 6.1 Decide and implement: either export `__XI_LOCK_HASH` (hash of
      `flake.lock`) into the child shell at activation time so
      `hook::shortcut::should_shortcut`'s fast path can actually fire, or remove
      the dead read in `hook/shortcut.rs` if the shortcut path is no longer
      wanted. Confirmed today: no writer exists outside `shortcut.rs`'s own unit
      tests.
- [ ] 6.2 Decide and implement: either export `__XI_ORIG_PATH` at first
      activation and use it for an explicit PATH-restoration path, or delete the
      dead read of it in `trust.rs:35`. Confirmed today: read but never written.
- [ ] 6.3 BDD lock-in test for whichever decision is made in 6.1/6.2, so the
      chosen behaviour cannot silently regress again.

## 7. Follow-ups (P4, deferred to future changes)

- [ ] 7.1 Nushell adapter (design + delta on the extension points).
- [ ] 7.2 `direnv` integration, needs its own proposal covering the handshake
      between direnv's env dump and xi's activation contract.
