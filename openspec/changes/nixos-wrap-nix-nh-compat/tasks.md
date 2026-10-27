# Tasks: nixos-wrap-nix-nh-compat

- [ ] `modules/nixos/package.nix` — rename option `programs.xi.nix.wrapAlias` to
      `programs.xi.wrapNix` (top-level). Move the declaration out of the
      `nix = { … };` sub-attrset. Update the `lib.mkIf cfg.nix.wrapAlias` guard
      to `lib.mkIf cfg.wrapNix`. Delete the old option — no alias.
- [ ] Create `modules/shared/wrapper.nix` — verbatim copy of the current
      `modules/nixos/wrapper.nix` body (8 lines).
- [ ] Delete `modules/nixos/wrapper.nix` and `modules/home-manager/wrapper.nix`.
- [ ] Update NixOS and HM entry module lists to import `../shared/wrapper.nix`
      instead of the deleted `./wrapper.nix`.
- [ ] `docs/guides/nix-proxy.md:21` — `nix.wrapAlias = true;` →
      `wrapNix = true;`.
- [ ] `docs/guides/module-setup.md:61` — `programs.xi.nix.wrapAlias = true;` →
      `programs.xi.wrapNix = true;`.
- [ ] `docs/reference/module-options.md:28,30` — row and prose reference
      `nix.wrapAlias` → `wrapNix`. Confirm the "NixOS only" note remains.
- [ ] Create `docs/migrating-from-nh.md` — single paragraph noting
      `programs.nh.wrapNix` → `programs.xi.wrapNix` is a namespace change only,
      semantics identical.
- [ ] `CHANGELOG.md` — under "Unreleased", add one line: "**Breaking**:
      `programs.xi.nix.wrapAlias` renamed to `programs.xi.wrapNix`. No alias —
      update downstream configs."
- [ ] Grep gate — `git grep -n wrapAlias` prints nothing.
- [ ] `nix flake check .` — passes on both NixOS and HM test hosts.
- [ ] `openspec validate nixos-wrap-nix-nh-compat` before archiving.
- [ ] After merge: bump flake input in
      `~/ghq/github.com/Dauliac/dotfiles/flake.lock`, rename `nix.wrapAlias` →
      `wrapNix` in the dotfiles, re-run `xi check .`.
- [ ] Follow-up housekeeping: rename this change directory
      `nixos-wrap-nix-nh-compat` → `nixos-wrapnix-rename`. Not blocking archive.
