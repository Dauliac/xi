# Tasks: xi-sources-cache-prestage

Live state in beads under `change:xi-sources-cache-prestage`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-cache-prestage/spec.md`.
- [ ] 1.2 Cross-link with model/config/bootstrap/gitops/push.

## 2. Pure-eval reader (P1)

- [ ] 2.1 `nix-command` crate:
      `pure_eval_apply_tostring(flake_ref,
      attr_path) -> Result<String>`
      that shells out to
      `nix eval --no-write-lock-file --extra-experimental-features
      'nix-command flakes' --apply toString <ref>#<attr>`.
- [ ] 2.2 Reject IFD surface by rejecting flake refs that need `--impure`.

## 3. Attribute discovery (P1)

- [ ] 3.1 Try in order: `nixosConfigurations.<host>.config.nix.settings`,
      `darwinConfigurations.<host>.config.nix.settings`,
      `homeConfigurations.<user>@<host>.config.nix.settings`,
      `homeConfigurations.<host>.config.nix.settings`, `nixCache.settings`.
- [ ] 3.2 Pick the first that evaluates.

## 4. Writer (P1)

- [ ] 4.1 Atomic write to `/etc/nix/xi.conf` (system) or
      `${XDG_CONFIG_HOME}/nix/xi.conf` (user): single file with
      `extra-substituters = ...` and `extra-trusted-public-keys = ...`.
- [ ] 4.2 `nix.conf` idempotent amend: append `!include xi.conf` once, guarded
      by exact-line check.
- [ ] 4.3 System scope only: `systemctl reload nix-daemon` after write.

## 5. Safety guard (P1)

- [ ] 5.1 Each key's fingerprint is fetched over HTTPS from the host in the
      matching substituter URL; mismatch refuses.
- [ ] 5.2 `--trust-declared-keys` bypasses with a visible stderr warning.

## 6. Receipt (P1)

- [ ] 6.1 Write `/var/lib/xi/cache-receipt.json` (or user XDG path) recording
      the amended `nix.conf` path and the `xi.conf` location.
- [ ] 6.2 `xi sources cache unstage` reads the receipt and reverses.

## 7. CLI (P1)

- [ ] 7.1
      `xi sources cache prestage [<flake-ref>#<attr>]
      [--trust-declared-keys]`;
      default flake-ref is the resolved sources context.
- [ ] 7.2 `xi sources cache unstage` reverses.
- [ ] 7.3 `xi sources cache status` reports whether prestage is active and which
      substituters are in effect.

## 8. Tests (P1)

- [ ] 8.1 Unit: pure-eval reader on a fixture flake.
- [ ] 8.2 Unit: nix.conf idempotent amend.
- [ ] 8.3 BDD: prestage → status → unstage round trip is byte-identical.

## 9. Follow-ups (P4)

- [ ] 9.1 Public-key hints via DNS TXT (SRV-like cache discovery). Deferred.
- [ ] 9.2 Multi-substituter weighting / failover. Deferred.
