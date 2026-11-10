# Proposal

## Why

`flake.nixosModules.default` (`nix/modules.nix`) cannot be imported into a
system-manager host config. Discovered wiring it into a real one
(`Dauliac/dotfiles`'s `modules/flake/system-configurations.nix`), filed as
`xi-9m39`: two of its ten constituent `modules/nixos/*.nix` files,
`shell-hook.nix` and `completion.nix`, assign
`environment.interactiveShellInit`/`programs.fish.interactiveShellInit` - option
paths system-manager's reduced NixOS surface does not declare at all. `lib.mkIf`
only gates the _value_ a config block contributes, not whether the _option path_
it assigns into has to exist, so this is a hard module-merge error regardless of
`shellHook.enable`'s value. Separately, `.default` also imports
`inputs.nix-lib.nixosModules.default` unconditionally; a consumer that already
imports a _different_ class head of the same underlying nix-lib flake
(`inputs.nlib.systemManagerModules.default`, via `nix-lib.follows`) gets
`option nix-lib.scanNestedSystems is already
declared` - two module-system
classes, same option path, no `lib.mkForce`-equivalent exists for a declaration
clash.

Every other file in `modules/nixos/` and the whole of `modules/shared/` (pure
`lib.mk*` helpers and type definitions, no `config` block touching a
host-class-specific option) is already system-manager-safe - confirmed by
reading each one. Neither of this change's two real blockers is inherent to what
`programs.xi.sources` itself needs: `sources.nix` only touches
`environment.etc`/`systemd.tmpfiles.rules`/`systemd.services`, all core options
system-manager does declare.

## What Changes

- A new flake output, `flake.nixosModules.sources` (`nix/modules.nix`),
  importing everything `.default` does _except_ `shell-hook.nix` and
  `completion.nix`, and _without_ importing
  `inputs.nix-lib.nixosModules.default` at all. Its doc comment states the one
  precondition this narrower module relies on instead of satisfying itself: the
  consumer's own evaluation must already provide some nix-lib module class (any
  one - `nixosModules`, `homeModules`, or `systemManagerModules`) so the
  `nix-lib.lib` namespace `modules/shared/lib/sources.nix` writes into already
  exists. This is exactly what a `nix-lib.follows`-based consumer (every current
  one, `Dauliac/dotfiles` included) already arranges for itself.
- `.default` itself is unchanged - still the complete aggregate, still NixOS
  -only, still the right choice for a consumer that is not also composing a
  different nix-lib class into the same evaluation.
- No change to `shell-hook.nix`, `completion.nix`, or anything in
  `modules/shared/`: the fix is in which files get imported together, not in any
  one file's own content.

## Capabilities

### New Capabilities

- `xi-nixos-module-system-manager-clash`: `flake.nixosModules.sources`, the
  narrower module export and the precondition contract it documents.

## Impact

- **Affected code**: `nix/modules.nix` (new `flake.nixosModules.sources`
  output).
- **Coordination**: none - purely additive; `.default` is untouched.
- **Breaking?**: No.
- **Migration**: A consumer currently working around this by hand-picking
  individual `modules/nixos/*.nix` files (as `Dauliac/dotfiles` does today, per
  its own documented comment in `modules/flake/system-configurations.nix`) can
  switch to importing `inputs.xi.nixosModules.sources` directly once this lands.

## Success Criteria

- **SC-NMC-001**: `nix eval` (or an equivalent `lib.evalModules` unit test) of a
  fixture system-manager-class module set that imports both
  `inputs.nlib.systemManagerModules.default` and
  `inputs.xi.nixosModules.sources`, with
  `programs.xi.enable = true;
  programs.xi.sources.enable = true;` set,
  evaluates without a module-merge error.
- **SC-NMC-002**: The same fixture with `flake.nixosModules.default` substituted
  in place of `.sources` still reproduces both original errors (the
  option-path-missing error from `shell-hook.nix`/`completion.nix`, and the
  declaration-clash error from the double nix-lib head) - proving the test
  fixture is a faithful reproduction of `xi-9m39`, not a vacuous pass.
- **SC-NMC-003**: `.sources`'s own module set, evaluated standalone against a
  plain NixOS fixture (no system-manager, no pre-existing nix-lib head needed
  since plain `nixosModules.default`-based fixtures normally provide their own),
  still activates `programs.xi.sources`'s `environment.etc."xi/sources.toml"`
  and `systemd.services.xi-sources-daemon` exactly as `.default` does today -
  proving the narrower module is a true subset, not a behavior change.
