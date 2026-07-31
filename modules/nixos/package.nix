{
  config,
  lib,
  pkgs,
  xiFlake,
  ...
}:
let
  cfg = config.programs.xi;
in
{
  options.programs.xi = {
    package = lib.mkOption {
      type = lib.types.package;
      default = xiFlake.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "inputs.xi.packages.\${system}.default";
      description = "The xi package to use.";
    };

    nix = {
      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.nixUnwrapped or pkgs.nix;
        defaultText = lib.literalExpression "pkgs.nixUnwrapped or pkgs.nix";
        description = ''
          The nix-compatible package whose binary xi will invoke internally.
          Defaults to `pkgs.nixUnwrapped` (set by the xi nixWrapper overlay)
          or `pkgs.nix`. Set to `pkgs.lix` to use Lix as the backend.
        '';
      };

      wrapAlias = lib.mkEnableOption ''
        replacing the system nix package with an xi-wrapped version.

        When enabled, `nix.package` is set to a symlinkJoin where only
        `bin/nix` is replaced by the xi proxy. All other binaries
        (nix-build, nix-env, nix-daemon, …) are preserved.

        This makes every `nix` invocation — not just interactive shells —
        go throughxi. No separate overlay needed.
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
      example = "./target/debug/xi";
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
        programs.xi.finalPackage = config.lib.mkFinalPackage { inherit pkgs cfg; };
        environment.systemPackages = [ cfg.finalPackage ];
      }

      # Auto-set settings from tool enable flags
      (lib.mkIf (!cfg.nom.enable) {
        programs.xi.settings.build.nom = false;
      })
      (lib.mkIf cfg.nixFastBuild.enable {
        programs.xi.settings.build.ci_backend = "nix-fast-build";
      })
      (lib.mkIf (!cfg.nixFastBuild.enable) {
        programs.xi.settings.build.ci_backend = "devour-flake";
      })

      (lib.mkIf cfg.nix.wrapAlias {
        nix.package = config.lib.mkWrappedNixPackage {
          inherit pkgs;
          inherit (cfg) package binPath;
          nixPackage = cfg.nix.package;
        };
      })
    ]
  );
}
