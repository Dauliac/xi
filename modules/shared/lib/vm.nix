{ lib, ... }:
{
  # Deliberately *not* wired automatically from `modules/flake-parts/vm.nix`
  # itself: a module that both reads its own option (`config.xi.vm`) and
  # writes flake-parts' `packages`/`checks` perSystem options from that
  # read sends this repo's `nix-lib`/flake-parts stack into infinite
  # recursion (confirmed empirically - an unrelated custom option read
  # +  written back under a *different* option name in the very same
  # module evaluates fine; only the combination with `packages`/`checks`
  # specifically recurses). Splitting the read (`vm.nix`, into
  # `config._xi.vm.rendered`) from the write (here, called from the
  # *consuming* flake's own `perSystem`) avoids it, at the cost of one
  # explicit `packages = config.lib.mkVmPackages { ... };` line in the
  # consumer - the same shape `xi.wrapper.enable`'s wrapping already asks
  # of `devShells.default = config.xi.wrapDevShell (...)`.
  nix-lib.lib.mkVmPackages = {
    type = lib.types.raw;
    fn =
      {
        pkgs,
        rendered,
        manifest,
      }:
      (lib.foldl' (acc: v: acc // v.packages) { } (lib.attrValues rendered))
      // {
        xi-vm-manifest = (pkgs.formats.json { }).generate "xi-vm-manifest.json" manifest;
      };
    description = ''
      Flatten `config._xi.vm.rendered` (from `modules/flake-parts/vm.nix`)
      into the `packages` attrset a consumer assigns to `perSystem.packages`,
      plus the `xi-vm-manifest` JSON `crates/xi-vm` reads at runtime.
    '';
  };

  nix-lib.lib.mkVmChecks = {
    type = lib.types.raw;
    fn = { rendered }: lib.foldl' (acc: v: acc // v.checks) { } (lib.attrValues rendered);
    description = ''
      Flatten `config._xi.vm.rendered`'s `checks` into the attrset a
      consumer assigns to `perSystem.checks`.
    '';
  };
}
