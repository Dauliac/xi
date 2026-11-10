# Capability: xi-nixos-module-system-manager-clash

## ADDED Requirements

### Requirement: `nixosModules.sources` is importable alongside a system-manager host's own nix-lib head

`flake.nixosModules.sources` MUST NOT import
`inputs.nix-lib.nixosModules.default`, and MUST NOT import `shell-hook.nix` or
`completion.nix` (the two `modules/nixos/*.nix` files that assign
`environment.interactiveShellInit`/`programs.fish.interactiveShellInit`, option
paths system-manager's reduced NixOS surface does not declare). Every other file
`.default` imports - the remaining `modules/nixos/*.nix` files and all of
`modules/shared/` - MUST be imported by `.sources` too.

#### Scenario: Importable alongside system-manager's own nix-lib head

- **GIVEN** a fixture module set importing
  `inputs.nlib.systemManagerModules.default` and
  `inputs.xi.nixosModules.sources` together
- **WHEN** `programs.xi.enable = true; programs.xi.sources.enable = true;` is
  set and the module set is evaluated
- **THEN** evaluation succeeds with no module-merge error

#### Scenario: `.default` still fails the same fixture

- **GIVEN** the same fixture, with `flake.nixosModules.default` substituted for
  `.sources`
- **WHEN** the module set is evaluated
- **THEN** it fails with the option-path-missing error from
  `shell-hook.nix`/`completion.nix`, or the `nix-lib.scanNestedSystems`
  declaration-clash error, confirming the fixture is a faithful reproduction of
  `xi-9m39`

### Requirement: `nixosModules.sources` is a true subset of `.default`'s behavior

Every option and config `.default` contributes for `programs.xi.sources` (and
the other non-excluded `modules/nixos/*.nix` files) MUST behave identically
under `.sources` on a plain NixOS fixture that does not need the system-manager
precondition. `.sources` MUST NOT add, remove, or change any option or config
path relative to what `.default` already provides for the files both export.

#### Scenario: `programs.xi.sources` activates identically under `.sources`

- **GIVEN** a plain NixOS fixture with
  `programs.xi.enable = true;
  programs.xi.sources = { enable = true; remote = "git@example:x.git"; };`
  evaluated once under `flake.nixosModules.default` and once under
  `flake.nixosModules.sources`
- **WHEN** both evaluations resolve `environment.etc."xi/sources.toml"` and
  `systemd.services.xi-sources-daemon`
- **THEN** both produce the same result for each
