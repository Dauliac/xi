{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.xi;
in
{
  options.programs.xi.test = {
    nixUnit = lib.mkOption {
      type = config._xi.types.tool;
      default = {
        enable = false;
        package = pkgs.nix-unit;
      };
      description = ''
        nix-unit — unit testing for Nix code. When enabled, xi auto-detects
        nix-unit in PATH and includes it as a test backend.
      '';
    };

    nixt = lib.mkOption {
      type = config._xi.types.tool;
      default = {
        enable = false;
        package = pkgs.nixt or null;
      };
      description = ''
        nixt — simple unit-testing for Nix with suite/case hierarchy.
        When enabled, xi auto-detects nixt in PATH and includes it
        as a test backend.
      '';
    };

    namaka = lib.mkOption {
      type = config._xi.types.tool;
      default = {
        enable = false;
        package = pkgs.namaka or null;
      };
      description = ''
        namaka — snapshot testing for Nix based on haumea.
        When enabled, xi auto-detects namaka in PATH and includes it
        as a test backend.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      (lib.mkIf cfg.test.nixUnit.enable {
        programs.xi.settings.test.backends = [ "nix-unit" ];
      })
      (lib.mkIf cfg.test.nixt.enable {
        programs.xi.settings.test.backends = [ "nixt" ];
      })
      (lib.mkIf cfg.test.namaka.enable {
        programs.xi.settings.test.backends = [ "namaka" ];
      })
    ]
  );
}
