# Design: nixos-wrap-nix-nh-compat

## The rename

`programs.xi.nix.wrapAlias` moves to `programs.xi.wrapNix` on NixOS. The old
option is deleted; no alias, no rename module, no warning shim.

Before (`modules/nixos/package.nix:32-42, 102-108`):

```nix
options.programs.xi.nix.wrapAlias = lib.mkEnableOption "…";
# …
(lib.mkIf cfg.nix.wrapAlias {
  nix.package = config.lib.mkWrappedNixPackage { … };
})
```

After (top-level, sibling of `enable` — no `nix.` sub-attr for this one):

```nix
options.programs.xi.wrapNix = lib.mkEnableOption ''
  replacing the system nix package with an xi-wrapped version.

  When enabled, `nix.package` is set to a symlinkJoin where only
  `bin/nix` is replaced by the xi proxy. All other binaries
  (nix-build, nix-env, nix-daemon, …) are preserved.

  This makes every `nix` invocation — not just interactive shells —
  go through xi. No separate overlay needed.
'';
# …
(lib.mkIf cfg.wrapNix {
  nix.package = config.lib.mkWrappedNixPackage { … };
})
```

`programs.xi.nix.package` (the "which nix binary to invoke" knob) stays where it
is — it is a legitimate sub-attribute, unaffected by the rename.

**Alternative rejected**: keeping `wrapAlias` and adding `wrapNix` via
`mkAliasOptionModule`. User directive: no back-compat.

## The `wrapper.nix` consolidation

Current state: `modules/nixos/wrapper.nix` and
`modules/home-manager/wrapper.nix` are byte-identical (8 lines each). Both
declare `programs.xi.wrapper` with type `config._xi.types.wrapper`.

Target state: single file at `modules/shared/wrapper.nix`, imported by both
NixOS and HM entry module lists. Contents copied from the current file verbatim
— no logic change.

```nix
# modules/shared/wrapper.nix
{ config, lib, ... }:
{
  options.programs.xi.wrapper = lib.mkOption {
    type = config._xi.types.wrapper;
    default = { };
    description = "Wrapper configuration that bakes the xi config into the binary.";
  };
}
```

Wiring: whichever `default.nix` / module aggregator lists `./wrapper.nix` on
each side switches to `../shared/wrapper.nix`.

**Alternative rejected**: leaving the duplication and adding a CI check to fail
when the two files diverge. Higher-effort, lower-benefit than just
consolidating.

## Scope boundary — `wrapper.enable` is NOT touched

The `programs.xi.wrapper.enable` option (defined by the shared type in
`modules/shared/types/wrapper.nix`, currently the only field in that submodule)
stays exactly as-is. Its purpose — bake xi's own config into a shell wrapper
around the xi binary — is unrelated to wrapping the system `nix` CLI. The two
features happened to have similarly-named toggles; this change stops conflating
them by name.

## Docs

Straight find-and-replace:

| File                                     | Change                                                              |
| ---------------------------------------- | ------------------------------------------------------------------- |
| `docs/guides/nix-proxy.md:21`            | `nix.wrapAlias = true;` → `wrapNix = true;`                         |
| `docs/guides/module-setup.md:61`         | `programs.xi.nix.wrapAlias = true;` → `programs.xi.wrapNix = true;` |
| `docs/reference/module-options.md:28,30` | `nix.wrapAlias` → `wrapNix`, update path in prose                   |

Plus a new `docs/migrating-from-nh.md` — one short page. Because the option now
shares nh's spelling, migration is a one-liner:

> Rename `programs.nh.wrapNix` → `programs.xi.wrapNix`. Value semantics are the
> same (wrap the system `nix` binary).

## Verification

- `nix flake check .` — evaluates without schema errors on both NixOS and HM
  test hosts under `modules/{nixos,home-manager}/test.nix`.
- Grep gate: `git grep -n wrapAlias` returns zero hits after the change.
- Downstream (dotfiles) — bump flake input, replace `nix.wrapAlias` with
  `wrapNix`, `xi check .` passes.

No unit test needed — the option is a plumbed `mkEnableOption` guarded by
`lib.mkIf`; nixpkgs option machinery already covers that.
