{ config, lib, ... }:
{
  nix-lib.lib.mkNixWrapperPackage = {
    type = lib.types.raw;
    fn =
      {
        pkgs,
        package,
        nixPackage,
        binPath ? null,
      }:
      let
        storeBin = lib.getExe package;
        nixBin = "${nixPackage}/bin/nix";
        body =
          if binPath != null then
            ''
              export XI_NIX_BIN="${nixBin}"
              exec ${binPath} nix "$@"
            ''
          else
            ''
              export XI_NIX_BIN="${nixBin}"
              exec ${storeBin} nix "$@"
            '';
      in
      pkgs.writeShellScriptBin "nix" body;
    description = "Build a store-path nix wrapper script that execs xi nix.";
  };

  nix-lib.lib.mkWrappedNixPackage = {
    type = lib.types.raw;
    fn =
      {
        pkgs,
        package,
        nixPackage,
        binPath ? null,
      }:
      let
        wrapper = config.lib.mkNixWrapperPackage {
          inherit
            pkgs
            package
            nixPackage
            binPath
            ;
        };
      in
      pkgs.symlinkJoin {
        name = "nix-xi-${nixPackage.version or "unknown"}";
        paths = [
          wrapper
          nixPackage
        ];
        passthru = (nixPackage.passthru or { }) // {
          inherit (nixPackage) version;
        };
        meta = nixPackage.meta // {
          mainProgram = "nix";
        };
      };
    description = ''
      Build a full nix package where only bin/nix is replaced by an xi wrapper.
      All other binaries (nix-build, nix-env, nix-daemon, …), libs, and share
      are preserved via symlinkJoin. Suitable for nix.package on NixOS.
    '';
  };
}
