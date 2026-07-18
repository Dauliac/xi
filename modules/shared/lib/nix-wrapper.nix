{ config, lib, ... }:
{
  options._xi.lib = {
    mkNixWrapperPackage = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Build a store-path nix wrapper script that execs xi nix.";
    };

    mkWrappedNixPackage = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = ''
        Build a full nix package where only bin/nix is replaced by an xi wrapper.
        All other binaries (nix-build, nix-env, nix-daemon, …), libs, and share
        are preserved via symlinkJoin. Suitable for nix.package on NixOS.
      '';
    };
  };

  config._xi.lib = {
    mkNixWrapperPackage =
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

    mkWrappedNixPackage =
      {
        pkgs,
        package,
        nixPackage,
        binPath ? null,
      }:
      let
        wrapper = config._xi.lib.mkNixWrapperPackage {
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
          wrapper # shadows bin/nix
          nixPackage # keeps bin/nix-build, bin/nix-env, lib/, share/, etc.
        ];
        passthru = (nixPackage.passthru or { }) // {
          inherit (nixPackage) version;
        };
        meta = nixPackage.meta // {
          mainProgram = "nix";
        };
      };
  };
}
