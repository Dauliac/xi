{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.xi;
in
{
  options.xi.fmt = {
    alejandra = lib.mkOption {
      type = config._xi.types.tool;
      default = {
        enable = false;
        package = pkgs.alejandra;
      };
      description = ''
        alejandra — opinionated Nix formatter. When enabled and set as
        the fmt backend, xi fmt uses alejandra instead of nixfmt.
      '';
    };

    treefmt = lib.mkOption {
      type = config._xi.types.tool;
      default = {
        enable = false;
        package = pkgs.treefmt;
      };
      description = ''
        treefmt — multi-language formatter using treefmt.toml. When
        enabled and set as the fmt backend, xi fmt uses treefmt.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      (lib.mkIf cfg.fmt.alejandra.enable {
        xi.settings.fmt.backend = "alejandra";
      })
      (lib.mkIf cfg.fmt.treefmt.enable {
        xi.settings.fmt.backend = "treefmt";
      })
    ]
  );
}
