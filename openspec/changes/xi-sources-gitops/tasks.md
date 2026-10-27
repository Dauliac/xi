# Tasks: xi-sources-gitops

Live state in beads under `change:xi-sources-gitops`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-gitops/spec.md`.
- [ ] 1.2 Cross-link with model/config/conflict/cache-prestage.

## 2. Daemon crate (P1)

- [ ] 2.1 New crate `crates/xi-sources-daemon/` with its own binary target
      (`xi-sources-daemon`), launched by the Nix module.
- [ ] 2.2 Singleton scheduler per `SourcesId` via `tokio::sync::Semaphore(1)`
      guarding each remote's poll loop (comin
      `internal/scheduler/scheduler.go:30` equivalent).
- [ ] 2.3 Fetch via `git fetch` subprocess (NOT libgit2 - users expect
      `~/.gitconfig` to be honoured; libgit2 does not read it fully).

## 3. Commit selection + conflict enforcement (P1)

- [ ] 3.1 Call into `xi-sources::conflict::resolve` with the new remote sha;
      refuse the advance when `conflict` returns `Error`.
- [ ] 3.2 Reject non-descendant advances (force push) via
      `git
      merge-base --is-ancestor`.

## 4. Signing (P1)

- [ ] 4.1 Support `require_signed = true` with `allowed_signers` (SSH) and
      `gpg_public_keys` (GPG). Only the tip must verify.
- [ ] 4.2 On missing/invalid signature, abort the tick and set `Degraded`.

## 5. Multi-remote (P2)

- [ ] 5.1 `.xi.toml [[sources.remote]]` array; each entry: `url`, `branch`,
      `poll_interval`.
- [ ] 5.2 Independent scheduler per entry; shared `sources_head` writer with a
      mutex.

## 6. Named Nix profile (P1)

- [ ] 6.1 Profile path: `/nix/var/nix/profiles/system-profiles/xi-sources`
      (system) or `~/.local/state/nix/profiles/xi-sources` (user).
- [ ] 6.2 Each successful build registers a new generation via
      `nix-env --profile <path> --set <storepath>`.

## 7. machineId guard (P1)

- [ ] 7.1 On activation, compare `/etc/machine-id` (NixOS) or hostname
      (home-manager) against the generation's embedded id.
- [ ] 7.2 Mismatch → refuse unless `--ignore-machine-id`.

## 8. Sentinel file (P1)

- [ ] 8.1 Clone creates `.xi-managed` with a JSON header
      `{
      "sources_id": "...", "created_by": "xi", "version": N }`.
- [ ] 8.2 Every write path asserts the sentinel exists.

## 9. Hooks (P2)

- [ ] 9.1 `on_advance`, `on_build`, `on_switch` as shell command strings;
      stdout/stderr captured to the state dir.
- [ ] 9.2 Hook failure does NOT roll back the git advance; it is logged and
      surfaced as `Degraded`.

## 10. Reboot window (P2)

- [ ] 10.1 Parse `reboot_window = "HH:MM-HH:MM"` as a daily window.
- [ ] 10.2 When an activation requires a reboot (checked via
      `nixos-rebuild
       --dry-run` output), defer until next window.

## 11. Tests (P1)

- [ ] 11.1 BDD: fast-forward on remote advance.
- [ ] 11.2 BDD: refuse force push.
- [ ] 11.3 Unit: machineId guard rejects on mismatch.
- [ ] 11.4 Unit: sentinel absence aborts tick.

## 12. Follow-ups (P4)

- [ ] 12.1 Prometheus exporter (comin pattern). Deferred.
- [ ] 12.2 gRPC control surface for IDE integrations. Deferred.
- [ ] 12.3 Webhook trigger (push notification from forge). Deferred.
