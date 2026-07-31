{
  config,
  lib,
  ...
}:
let
  cfg = config.programs.xi;
in
{
  options.programs.xi.fmt = {
    backend = lib.mkOption {
      type = lib.types.str;
      default = "auto";
      description = ''
        Formatter backend command for `xi fmt`.

        Well-known values: "auto" (detect flake formatter, else nixfmt),
        "flake" (use `nix fmt`), or any command on PATH (e.g. "nixfmt",
        "alejandra", "pedantix").
      '';
    };

    tools = lib.mkOption {
      type = lib.types.attrsOf config._xi.types.tool;
      default = { };
      description = ''
        Formatter tools to make available on PATH for `xi fmt`.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      (lib.mkIf (cfg.fmt.backend != "auto") {
        programs.xi.settings.fmt.backend = cfg.fmt.backend;
      })
    ]
  );
}
