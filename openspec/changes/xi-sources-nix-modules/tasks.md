# Tasks: xi-sources-nix-modules

Live state in beads under `change:xi-sources-nix-modules`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-nix-modules/spec.md`.
- [ ] 1.2 Cross-link with every other xi-sources-* spec that references a module
      option.

## 2. Shared library (P1)

- [ ] 2.1 `modules/shared/lib/sources.nix` carries the `optionTree` attribute
      set with every `xi.sources.*` option (type, default, description,
      example).
- [ ] 2.2 Three-head parity: each of `modules/nixos/sources.nix`,
      `modules/home-manager/sources.nix`, `modules/flake-parts/sources.nix`
      imports the shared library and declares `options.xi.sources = optionTree`.

## 3. Option tree enumeration (P1)

- [ ] 3.1 `xi.sources.enable` (bool, default false), master switch.
- [ ] 3.2 `xi.sources.remote` (str, nullable), remote URL.
- [ ] 3.3 `xi.sources.branch` (str, default "main"), tracked branch.
- [ ] 3.4 `xi.sources.working-dir` (path, nullable), optional working dir path
      (user scope only; assertion-rejected at system scope).
- [ ] 3.5 `xi.sources.poll-interval` (str, default "60s"), duration.
- [ ] 3.6 `xi.sources.require-signed` (bool, default false).
- [ ] 3.7 `xi.sources.allowed-signers` (path, nullable).
- [ ] 3.8 `xi.sources.gpg-public-keys` (listOf path, default []).
- [ ] 3.9 `xi.sources.sync-mode` (enum prompt|auto-copy|never, default
      "prompt").
- [ ] 3.10 `xi.sources.gitops.enable` (bool, default false).
- [ ] 3.11 `xi.sources.gitops.reboot-window` (str, default "always").
- [ ] 3.12 `xi.sources.gitops.hooks.on-advance` (str, nullable).
- [ ] 3.13 `xi.sources.gitops.hooks.on-build` (str, nullable).
- [ ] 3.14 `xi.sources.gitops.hooks.on-switch` (str, nullable).
- [ ] 3.15 `xi.sources.bootstrap.installer` (enum detsys|lix|none, nullable,
      default picks lix when `cache.lix.systems` is in the effective
      substituters list else detsys).
- [ ] 3.16 `xi.sources.cache-prestage.enable` (bool, default true).
- [ ] 3.17 `xi.sources.cache-prestage.trust-declared-keys` (bool, default
      false).
- [ ] 3.18 `xi.sources.secrets.<name>.source` (path), per-secret upload (owned
      by [[xi-sources-push]]).
- [ ] 3.19 `xi.sources.secrets.<name>.destination` (path).
- [ ] 3.20 `xi.sources.secrets.<name>.owner` (str, default "root").
- [ ] 3.21 `xi.sources.secrets.<name>.group` (str, nullable).
- [ ] 3.22 `xi.sources.secrets.<name>.mode` (str, default "0600").
- [ ] 3.23 `xi.sources.secrets.<name>.upload-at` (enum
      pre-activate|post-activate, default "pre-activate").
- [ ] 3.24 `xi.sources.credentials.<kind>.*` (owned by
      [[xi-sources-credentials]], see that spec).

## 4. Activation (P1)

- [ ] 4.1 System head: an activation script creates
      `/var/lib/xi/{sources,state}` with mode `0700` and renders
      `/etc/xi/sources.toml` from the evaluated options.
- [ ] 4.2 User head: a home-manager activation step creates
      `$XDG_DATA_HOME/xi/sources` and `$XDG_STATE_HOME/xi` with mode `0700` and
      renders `$XDG_CONFIG_HOME/xi/sources.toml`.
- [ ] 4.3 Flake-parts head: contributes the config to any downstream head that
      imports it; no runtime activation of its own.

## 5. Assertions (P1)

- [ ] 5.1 `xi.sources.working-dir` at system scope asserts false (user scope
      only).
- [ ] 5.2 Same remote URL enabled at both system and user scopes with
      `gitops.enable = true` triggers an eval assertion naming the conflict.
- [ ] 5.3 `require-signed = true` without `allowed-signers` or `gpg-public-keys`
      triggers an eval assertion.

## 6. Tests (P1)

- [ ] 6.1 NixOS VM test: boot a VM with `xi.sources.enable = true`, assert state
      dir exists before any login prompt, assert `/etc/xi/sources.toml` matches
      the module config.
- [ ] 6.2 home-manager fixture test: activate with `xi.sources.enable = true`,
      assert XDG paths exist with correct mode.
- [ ] 6.3 Parity test: diff the option sets rendered by the three heads; fail on
      any delta.

## 7. Follow-ups (P4)

- [ ] 7.1 Darwin-specific head (`modules/darwin/sources.nix`) if nix-darwin
      consumers want a distinct activation phase.
- [ ] 7.2 NixOS-tests fixture library so other xi-sources specs can spin up VMs
      with less boilerplate.
