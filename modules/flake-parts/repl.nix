{
  config,
  lib,
  ...
}:
let
  cfg = config.xi;
in
{
  options.xi.repl = {
    theme = lib.mkOption {
      type = lib.types.enum [
        "default"
        "dracula"
        "catppuccin-mocha"
        "nord"
        "gruvbox-dark"
        "solarized-dark"
      ];
      default = "default";
      description = ''
        Color theme for `xi repl`'s rendered values and syntax highlighting.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      (lib.mkIf (cfg.repl.theme != "default") {
        xi.settings.repl.theme = cfg.repl.theme;
      })
    ]
  );
}
