# nixos-module (delta)

## REMOVED Requirements

### Requirement: `programs.xi.nix.wrapAlias` option

**Reason**: renamed to `programs.xi.wrapNix` (top-level). No alias shim — the
old option is removed outright. **Migration**: downstream configs replace
`programs.xi.nix.wrapAlias = <bool>;` with `programs.xi.wrapNix = <bool>;`.
Semantics are unchanged.

## ADDED Requirements

### Requirement: `programs.xi.wrapNix` replaces `nix.package` with an xi proxy

The xi NixOS module MUST expose `programs.xi.wrapNix : bool` at the top of the
`programs.xi` attrset (not under `programs.xi.nix.`). When true, the module MUST
set `nix.package` to a `symlinkJoin` whose only `bin/nix` entry is an xi wrapper
script; all other binaries and paths from the underlying nix package MUST be
preserved.

The HM module MUST NOT expose `programs.xi.wrapNix` — HM has no equivalent of
NixOS's `nix.package`. Attempting to set it on the HM side is a schema error by
design.

#### Scenario: Host wraps the system nix CLI

- **WHEN** a NixOS configuration sets `programs.xi.wrapNix = true;`
- **THEN** `config.nix.package` resolves to the xi-wrapped `symlinkJoin`
- **AND** `${config.nix.package}/bin/nix` is the xi proxy shell script
- **AND** `${config.nix.package}/bin/nix-build` (and other original binaries)
  point to the same store paths as the underlying `programs.xi.nix.package`

#### Scenario: Default keeps the stock nix binary

- **WHEN** a NixOS configuration does not set `programs.xi.wrapNix`
- **THEN** `config.nix.package` is not touched by the xi module

#### Scenario: Legacy option name fails loudly

- **WHEN** a NixOS configuration sets `programs.xi.nix.wrapAlias = true;`
- **THEN** evaluation fails with the standard nixpkgs "The option
  `programs.xi.nix.wrapAlias' does not exist" error
- **AND** no silent fallback to the new option occurs

### Requirement: shared `programs.xi.wrapper` declaration

The `programs.xi.wrapper` option MUST be declared in a single shared module
(`modules/shared/wrapper.nix`) imported by both the NixOS and HM entry module
lists. Neither `modules/nixos/wrapper.nix` nor
`modules/home-manager/wrapper.nix` may exist after this change.

#### Scenario: Both platforms expose the same wrapper option

- **WHEN** `programs.xi.wrapper.enable = true;` is set on a NixOS host
- **AND** the same option is set on an HM configuration
- **THEN** both evaluate without schema error
- **AND** both resolve `programs.xi.wrapper.enable == true`

#### Scenario: Consolidation removes drift risk

- **WHEN** a new option is added to `_xi.types.wrapper` in
  `modules/shared/types/wrapper.nix`
- **THEN** the option is visible to both NixOS and HM without editing two
  separate `wrapper.nix` files
