{
  config,
  lib,
  pkgs,
  xiFlake,
  system,
  ...
}:
let
  cfg = config.xi;
in
{
  options.xi = {
    package = lib.mkOption {
      type = lib.types.package;
      default = xiFlake.packages.${system}.default;
      defaultText = lib.literalExpression "inputs.xi.packages.\${system}.default";
      description = "The xi package to use.";
    };

    nix.package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.nixUnwrapped or pkgs.nix;
      defaultText = lib.literalExpression "pkgs.nixUnwrapped or pkgs.nix";
      description = ''
        The nix-compatible package whose binary xi will invoke internally.
        Defaults to `pkgs.nixUnwrapped` (set by the xi nixWrapper overlay)
        or `pkgs.nix`. Set to `pkgs.lix` to use Lix as the backend.
      '';
    };

    nom = lib.mkOption {
      type = config._xi.types.tool;
      default = {
        enable = true;
        package = pkgs.nix-output-monitor;
      };
      description = "nix-output-monitor — pretty build output for xi.";
    };

    nixFastBuild = lib.mkOption {
      type = config._xi.types.tool;
      default = {
        enable = true;
        package = pkgs.nix-fast-build;
      };
      description = ''
        nix-fast-build — parallel eval + pipelined builds for `xi ci` and
        `xi build --all`. When enabled, xi auto-detects nix-fast-build in
        PATH and uses it instead of devour-flake.
      '';
    };

    binPath = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "./target/debug/nh";
      description = ''
        Override the xi binary path. When set, the wrapper and shell hooks
        will try this path first and fall back to the store binary if it
        does not exist or is not executable.
      '';
    };

    finalPackage = lib.mkOption {
      type = lib.types.package;
      internal = true;
      description = "The xi package after applying wrapper and configuration.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        xi.finalPackage = config._xi.lib.mkFinalPackage { inherit pkgs cfg; };
      }

      (lib.mkIf (!cfg.nom.enable) {
        xi.settings.build.nom = false;
      })
      (lib.mkIf cfg.nixFastBuild.enable {
        xi.settings.build.ci_backend = "nix-fast-build";
      })
      (lib.mkIf (!cfg.nixFastBuild.enable) {
        xi.settings.build.ci_backend = "devour-flake";
      })
    ]
  );
}
