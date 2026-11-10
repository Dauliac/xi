# Tasks

Live state in beads under `change:xi-nixos-module-system-manager-clash`. Fixes
`xi-9m39`.

## 1. The narrower module

- [ ] 1.1 Add `flake.nixosModules.sources` to `nix/modules.nix`: the same import
      list as `.default` minus `inputs.nix-lib.nixosModules.default`,
      `modules/nixos/shell-hook.nix`, and `modules/nixos/completion.nix`; doc
      comment states the nix-lib-head precondition explicitly.

## 2. Verification

- [ ] 2.1 `lib.evalModules`-based check (alongside the existing checks in
      `nix/sources-checks.nix`) proving a fixture importing
      `inputs.nix-lib.systemManagerModules.default` and
      `self.nixosModules.sources` together, with
      `programs.xi.sources.enable
      = true`, evaluates `.config` without
      error (SC-NMC-001), and that the same fixture with `.default` substituted
      in still reproduces both of `xi-9m39`'s original errors via
      `builtins.tryEval` (SC-NMC-002) - proving the fixture is a faithful
      reproduction, not vacuous.
- [ ] 2.2 Extend the existing `xi-sources-nixos` VM test (or add a sibling)
      proving `.sources` activates `programs.xi.sources` identically to
      `.default` on a plain NixOS fixture (SC-NMC-003).

## Workflow follow-up

- Archive this change once `Dauliac/dotfiles` has switched from its documented
  hand-picked-files workaround to importing `inputs.xi.nixosModules.sources`
  directly and a real system-manager `xi system switch` succeeds with it.
