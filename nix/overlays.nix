{
  self,
  ...
}:
{
  # --- overlays.default (via easyOverlay) ---
  # Adds `pkgs.xi` to nixpkgs. Use `overlayAttrs` so flake-parts'
  # easyOverlay composes `overlays.default` automatically.
  perSystem =
    { config, ... }:
    {
      overlayAttrs = {
        xi = config.packages.xi;
      };
    };

  # --- overlays.nixWrapper ---
  # Replaces `pkgs.nix` with an xi-wrapped version.
  #
  # The `nix` binary goes through `xi nix` (enhanced UX, nom, etc.),
  # while all other binaries (nix-build, nix-env, nix-daemon, …) and
  # libraries are preserved from the original package.
  #
  # The original, unwrapped nix package is kept as `pkgs.nixUnwrapped`
  # so that xi modules can reference the real binary via XI_NIX_BIN.
  #
  # Usage:
  #   nixpkgs.overlays = [
  #     inputs.xi.overlays.nixWrapper
  #   ];
  #
  # For Lix: apply a lix overlay *before* this one so that `prev.nix`
  # is already lix, and xi wraps lix transparently:
  #   nixpkgs.overlays = [
  #     inputs.lix.overlays.default   # nix → lix
  #     inputs.xi.overlays.nixWrapper # lix → xi-wrapped lix
  #   ];
  flake.overlays.nixWrapper =
    final: prev:
    let
      system = final.stdenv.hostPlatform.system;
      xi = self.packages.${system}.xi;
      realNix = prev.nix;
      wrapper = final.writeShellScriptBin "nix" ''
        export XI_NIX_BIN="${realNix}/bin/nix"
        exec ${final.lib.getExe xi} nix "$@"
      '';
    in
    {
      # Preserve the unwrapped nix so modules / users can reference it.
      nixUnwrapped = realNix;

      nix = final.symlinkJoin {
        name = "nix-xi-${realNix.version or "unknown"}";
        paths = [
          wrapper # shadows bin/nix
          realNix # keeps bin/nix-build, bin/nix-env, lib/, share/, etc.
        ];
        # Preserve passthru so NixOS modules that inspect nix.package
        # (e.g. version checks) keep working.
        passthru = (realNix.passthru or { }) // {
          inherit (realNix) version;
        };
        meta = realNix.meta // {
          mainProgram = "nix";
        };
      };
    };
}
