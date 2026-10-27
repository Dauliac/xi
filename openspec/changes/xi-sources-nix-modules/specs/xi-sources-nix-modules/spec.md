# Capability: xi-sources-nix-modules

## ADDED Requirements

### Requirement: Three-head option-tree parity

The capability MUST expose the `xi.sources.*` option tree from three parallel
Nix modules:

- `modules/nixos/sources.nix`
- `modules/home-manager/sources.nix`
- `modules/flake-parts/sources.nix`

All three MUST import the shared library at `modules/shared/lib/sources.nix`
which carries the single source of truth for option types, defaults,
descriptions, and examples. The three heads MUST NOT redeclare any option that
lives in the shared library.

#### Scenario: Parity test blocks drift

- **WHEN** a change edits any xi.sources option in only one head
- **THEN** the parity test fails naming the diverging path

#### Scenario: Shared library carries types

- **WHEN** a user reads the option description via
  `nixos-option xi.sources.poll-interval` on a NixOS build
- **THEN** the description matches the one shown by home-manager's option
  browser for the same path, byte for byte

### Requirement: Complete option enumeration

Every option referenced by any other `xi-sources-*` spec MUST exist in the
shared library with a declared type and default. The complete set at this
version of the spec is:

- `xi.sources.enable` (bool, default false)
- `xi.sources.remote` (str, nullable)
- `xi.sources.branch` (str, default "main")
- `xi.sources.working-dir` (path, nullable, user scope only)
- `xi.sources.poll-interval` (str, default "60s")
- `xi.sources.require-signed` (bool, default false)
- `xi.sources.allowed-signers` (path, nullable)
- `xi.sources.gpg-public-keys` (listOf path, default [])
- `xi.sources.sync-mode` (enum prompt|auto-copy|never, default "prompt")
- `xi.sources.gitops.enable` (bool, default false)
- `xi.sources.gitops.reboot-window` (str, default "always")
- `xi.sources.gitops.hooks.{on-advance,on-build,on-switch}` (str, nullable)
- `xi.sources.bootstrap.installer` (enum detsys|lix|none, nullable)
- `xi.sources.cache-prestage.enable` (bool, default true)
- `xi.sources.cache-prestage.trust-declared-keys` (bool, default false)
- `xi.sources.secrets.<name>.{source,destination,owner,group,mode,upload-at}`
- `xi.sources.credentials.<kind>.*` (details in [[xi-sources-credentials]])

Adding a new option to any downstream spec MUST amend this enumeration in the
same change.

#### Scenario: Unknown option refused

- **GIVEN** a user sets `xi.sources.frobnicate = true` in their config
- **WHEN** the system evaluates
- **THEN** evaluation fails with the standard Nix "The option
  'xi.sources.frobnicate' does not exist" error

### Requirement: Activation materialises the config file

System scope: an activation script MUST create `/var/lib/xi/sources` and
`/var/lib/xi/state` with mode `0700`, owned by `root:root`, and render
`/etc/xi/sources.toml` from the evaluated options BEFORE any other xi-related
service starts.

User scope: a home-manager activation step MUST create
`$XDG_DATA_HOME/xi/sources` and `$XDG_STATE_HOME/xi` with mode `0700`, owned by
the user, and render `$XDG_CONFIG_HOME/xi/sources.toml`.

Flake-parts: contributes options to downstream heads; has no runtime activation
of its own.

#### Scenario: System dirs exist before xi runs

- **GIVEN** a NixOS build with `xi.sources.enable = true`
- **WHEN** the system reaches multi-user target
- **THEN** `/var/lib/xi/sources` and `/var/lib/xi/state` already exist with mode
  `0700` owned by `root:root`
- **AND** `/etc/xi/sources.toml` is a readable file whose content reflects the
  module options

#### Scenario: Rendered config carries nix-module provenance

- **GIVEN** `xi.sources.poll-interval = "5m"` set via the NixOS module
- **WHEN** the user runs `xi sources status --explain`
- **THEN** the `poll-interval` field reports provenance `nix-module`

### Requirement: Scope assertions

Working dir is user-scope only. Setting `xi.sources.working-dir` at system scope
MUST raise an eval assertion naming the key and the scope mismatch.

Signing requires at least one key file. Setting
`xi.sources.require-signed = true` with both `allowed-signers = null` AND
`gpg-public-keys = []` MUST raise an eval assertion.

Mutual exclusion for gitops across scopes. Enabling
`xi.sources.gitops.enable = true` at BOTH system and user scope with the same
`xi.sources.remote` on the same host MUST raise an eval assertion naming the
conflicting scope pair.

#### Scenario: Working dir at system refused

- **GIVEN** `nixos` config with `xi.sources.working-dir = "/root/
  dotfiles"`
- **WHEN** the system evaluates
- **THEN** the build fails with an assertion naming `xi.sources.working-dir` and
  the "user scope only" rule

#### Scenario: Dual-scope gitops conflict

- **GIVEN** a system-scope config AND a home-manager config both setting
  `xi.sources.gitops.enable = true` with the same `xi.sources.remote`
- **WHEN** the system+hm evaluation runs
- **THEN** the build fails with an assertion naming both scopes
