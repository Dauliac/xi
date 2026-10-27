# Tasks: xi-sources-push

Live state in beads under `change:xi-sources-push`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-push/spec.md`.
- [ ] 1.2 Cross-link with model/config/conflict and bootstrap (bootstrap uses
      `xi push --phase install`).

## 2. New verb `xi push` (P1)

- [ ] 2.1 Positional `<targets...>`, flags `--on`, `--backend`, `--phase`,
      `--mode`, `--magic-rollback`, `--parallel`, `--interactive-sudo`,
      `--copy-via`, `--fail-fast`, `--force-dirty`.
- [ ] 2.2 Resolve sources context before dispatch; refuse if sources are
      `Missing`.

## 3. Backend parity gaps (P1)

- [ ] 3.1 `builtin`: wire magic-rollback (deploy-rs pattern) via a sentinel
      file + canary SSH probe.
- [ ] 3.2 `builtin`: wire `--copy-via target` to invoke
      `nix-copy-closure
      --to <host> --use-substitutes`.
- [ ] 3.3 `builtin`: allocate PTY under `--interactive-sudo`.
- [ ] 3.4 `builtin`: pre-flight (SSH, disk, sudo, `nixos-rebuild`).

## 4. `--mode sources` (P2)

- [ ] 4.1 Rsync sources dir to target (`${target}:/etc/xi/sources`), excluding
      `.git/` and operator-specific files.
- [ ] 4.2 Remote: `nixos-rebuild switch --flake /etc/xi/sources#<host>`.

## 5. Secret stanza (P2)

- [ ] 5.1 `.xi.toml [sources.secrets.<name>]` section with `source`,
      `destination`, `owner`, `group`, `mode`, `upload_at` (`pre_activate` |
      `post_activate`).
- [ ] 5.2 Upload via `scp` or `sftp` with the configured mode.
- [ ] 5.3 Warn if a conflicting agenix or sops-nix secret of the same name is
      declared.

## 6. Parallelism (P2)

- [ ] 6.1 Independent `tokio::sync::Semaphore` for eval and apply phases so eval
      can preface apply without a global lock.

## 7. Backend detection (P1)

- [ ] 7.1 Prefer explicit `--backend`; else detect `deploy.*` → `colmenaHive` →
      fall back to `builtin`.
- [ ] 7.2 Target discovery reads FlakeOutput taxonomy variants.

## 8. Tests (P1)

- [ ] 8.1 BDD: single-host push via builtin.
- [ ] 8.2 BDD: multi-host push with `--parallel 2`.
- [ ] 8.3 Unit: preflight failures reported per target.
- [ ] 8.4 Unit: dirty-working-dir refusal composes with `xi-sources-conflict`.

## 9. Follow-ups (P4)

- [ ] 9.1 `xi push --dry-run` emits the per-target plan without executing.
      Deferred to future spec.
- [ ] 9.2 Push over Tailscale / Nebula with pre-shared keys. Deferred.
- [ ] 9.3 Reverse mode (target initiates pull from operator over SSH tunnel).
      Deferred.
