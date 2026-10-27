# Change: xi-sources-cache-prestage

## Why

The user flagged this as a chicken-and-egg problem worth documenting: "for a
bootstrap xi should setup the cache configs if they are nix managed by the
module before the deployment of all things to take benefits from the cache
before the build in the machine (this is a complem[entary] mecanism we can note
for later)".

Without prestage, every bootstrap starts by building every derivation from
source because the user's own binary caches (FlakeHub / Cachix / self-hosted
Attic / S3 bucket) are only configured AFTER the first build lands them in the
activated system. That is a 30-to-90-minute waste on a fresh machine with 100
Mbps internet.

The bootstrap research
(`openspec/research/xi-sources/
bootstrap-curl-pipe-sh.md`, §4.4) found that
**nixos-anywhere** solved this with a single trick:
`nix eval --apply toString
${flake}#${attr}.nix.settings.substituters` is a pure
eval (no IFD, no build) that returns the user's declared substituters BEFORE
anything is built. xi steals this directly.

This change is scoped as a standalone capability because:

- The cache-prestage logic is reused by [[xi-sources-bootstrap]] (fresh
  machine), [[xi-sources-gitops]] (daemon activates a generation whose
  substituter config has changed), and [[xi-sources-push]] (push target has no
  cache config yet).
- The user asked for it to be "noted for later" - this change does the noting,
  in spec form, so follow-on changes can depend on it.

## What Changes

- Introduce capability `xi-sources-cache-prestage` that owns:
  - A pure-eval reader of a flake's `nix.settings.substituters` and
    `nix.settings.trusted-public-keys` via
    `nix eval --no-write-lock-file --extra-experimental-features
    'nix-command flakes' --apply toString
    ${flake}#${attr}.config.nix.settings.substituters`
    (and the parallel `trusted-public-keys` call). No IFD; no build; fails fast
    if the attribute path does not exist.
  - A **well-known attribute discovery**: xi tries, in order, these attribute
    paths matching the active scope:
    - `nixosConfigurations.<host>.config.nix.settings`
    - `darwinConfigurations.<host>.config.nix.settings`
    - `homeConfigurations.<user>@<host>.config.nix.settings`
    - `homeConfigurations.<host>.config.nix.settings`
    - fallback: a well-known top-level attribute `nixCache.settings` that users
      can expose from a flake that lacks a NixOS module (home-manager-only
      flake) - see open question in bootstrap research doc §6.
  - An **atomic write** to `/etc/nix/xi.conf` (system) or
    `${XDG_CONFIG_HOME}/nix/xi.conf` (user) containing
    `extra-substituters = ...` and `extra-trusted-public-keys = ...`. `nix.conf`
    is amended with `!include xi.conf` once; xi never edits `nix.conf` body (so
    users retain their file).
  - A **nix-daemon reload** step for system scope so the new config takes effect
    before the first build. For user scope no reload is required (Nix reads
    `~/.config/nix/nix.conf` per invocation).
  - A **safety guard**: by default the key list is treated as sensitive and xi
    refuses to prestage keys whose signing fingerprint cannot be fetched over
    HTTPS from the key server named in the substituter URL. The guard is
    bypassed with `--trust-declared-keys` for airgapped setups.
  - A **reversible receipt** at `/var/lib/xi/cache-receipt.json` so
    `xi sources cache unstage` removes the `!include xi.conf` line and the file
    on uninstall.

## Impact

- **Affected specs**: `xi-sources-cache-prestage` (new capability).
- **Affected code (planned)**: `crates/xi-sources/src/cache_prestage.rs` with
  the pure-eval reader and nix.conf writer; `nix-command` crate gains a
  `pure_eval_apply_tostring()` helper.
- **Coordination**: [[xi-sources-bootstrap]] calls into this capability as Phase
  2 (before the first build). [[xi-sources-gitops]] re-runs the prestage when a
  generation changes the substituter list. [[xi-sources-push]] optionally runs
  the prestage on the target with `--prestage-cache`.
- **Breaking?**: No. The `!include xi.conf` approach preserves the user's
  `nix.conf`; the receipt enables full reversal.
- **Migration**: None.

## Success Criteria

- **SC-K-001**: `xi sources cache prestage <flake-url>#<attr>` writes
  `/etc/nix/xi.conf` (or XDG equivalent) with the substituters and keys pulled
  from the flake's `nix.settings` via pure eval, without building anything.
- **SC-K-002**: `nix.conf` gains an `!include xi.conf` line only if absent;
  running prestage twice is idempotent.
- **SC-K-003**: After prestage, `nix show-config | grep substituters` reflects
  the new values.
- **SC-K-004**: On a flake lacking `config.nix.settings.substituters`
  (home-manager-only), xi falls back to the `nixCache.settings` convention; if
  neither exists, prestage exits cleanly with `status: Missing` (no `xi.conf`
  written).
- **SC-K-005**: The declared-keys safety guard refuses an unknown key by
  default; `--trust-declared-keys` bypasses with a visible warning.
- **SC-K-006**: `xi sources cache unstage` removes the `!include
  xi.conf` line
  and the file; `nix.conf` is byte-identical to pre-prestage state.
- **SC-K-007**: On system scope, nix-daemon is reloaded after prestage so the
  next build sees the new substituters.
