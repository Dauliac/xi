# Change: nixos-wrap-nix-nh-compat

> Directory name is stale under the no-compat direction — the change is now a
> straight rename + consolidation, not an nh-compat shim. Rename the directory
> to `nixos-wrapnix-rename` in a follow-up.

## Why

Two problems in the current NixOS module surface:

1. **`programs.xi.nix.wrapAlias` is undiscoverable.** Users looking for a "wrap
   the nix CLI" toggle grep for `wrapNix` (nh's spelling, and the intuitive one)
   and find nothing. The current name is xi-internal jargon.
2. **`modules/nixos/wrapper.nix` and `modules/home-manager/wrapper.nix` are
   byte-identical duplicates.** Every future option added to the wrapper
   submodule has to be added twice — drift is a matter of time.

The trigger was an nh-to-xi migration: a downstream config carrying
`programs.xi.wrapNix = true;` (inherited option name from `programs.nh.wrapNix`)
fails schema evaluation on xi because xi has no such option. Rather than paper
over that with an alias, we align the xi option name with the intuitive one and
consolidate the duplicated wrapper modules.

## What Changes

Three concrete edits, all breaking, no compat shims:

1. **Rename `programs.xi.nix.wrapAlias` → `programs.xi.wrapNix`** (top-level, on
   NixOS only — HM has no `nix.package` to wrap). Semantics unchanged: when
   true, `nix.package` is replaced by a `symlinkJoin` whose `bin/nix` is an xi
   proxy. Old option is deleted outright; downstream configs setting
   `nix.wrapAlias` will fail evaluation with the standard nixpkgs "option does
   not exist" error.
2. **Consolidate `wrapper.nix` into `modules/shared/wrapper.nix`**, imported by
   both NixOS and HM module lists. The two current files vanish.
3. **Docs sweep**: `docs/guides/nix-proxy.md`, `docs/guides/module-setup.md`,
   `docs/reference/module-options.md` — replace every `wrapAlias` occurrence
   with `wrapNix`. Add `docs/migrating-from-nh.md` with a one-line note that
   `programs.nh.wrapNix` maps 1:1 to `programs.xi.wrapNix` (no rename needed on
   the downstream side — the point of aligning names).

## Not in scope

- No `mkAliasOptionModule` / `mkRenamedOptionModule` shims. Breaking rename is
  intentional — the old name has no meaningful install base beyond this repo's
  own dotfiles.
- No new options on the `wrapper` submodule. Current shared type exposes only
  `.enable`; that stays. The earlier design draft listed `paths` and `package`
  as options to "copy verbatim from HM" — they never existed. Removed.
- No unification of `wrapNix` (wraps the _nix_ CLI) with `wrapper.enable` (bakes
  xi config into the _xi_ binary). Those are different features and will keep
  different names.

## Alternatives considered

- **Keep `wrapAlias`, add `wrapNix` as an alias.** Rejected: the user explicitly
  asked for no back-compat. Two names for the same option is worse than one
  breaking rename.
- **Also rename `wrapper.enable` → something.** Rejected: `wrapper` is a
  distinct concept (bakes xi config into xi's own binary) and its current name
  is fine. Only `wrapAlias` was the outlier.
- **Delete `wrapAlias` entirely, force users onto `nix.package = mkWrappedNix …`
  directly.** Rejected: the `mkEnableOption` boolean is the discoverable
  surface. Removing it pushes the wrapping logic into every downstream config.
