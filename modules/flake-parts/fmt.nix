{
  config,
  lib,
  ...
}:
let
  cfg = config.xi;
in
{
  options.xi.fmt = {
    backend = lib.mkOption {
      type = lib.types.str;
      default = "auto";
      description = ''
        Formatter backend command for `xi fmt`.

        Well-known values: "auto" (detect flake formatter, else nixfmt),
        "flake" (use `nix fmt`), or any command on PATH (e.g. "nixfmt",
        "alejandra", "pedantix").

        When set to a tool name, ensure the tool is available on PATH
        either via `xi.fmt.tools` or your devShell packages.
      '';
    };

    tools = lib.mkOption {
      type = lib.types.attrsOf config._xi.types.tool;
      default = { };
      description = ''
        Formatter tools to make available on PATH for `xi fmt`.

        Each entry adds a package to the xi wrapper's PATH. The tool
        matching `xi.fmt.backend` will be invoked by `xi fmt`.

        Example:
          xi.fmt.tools.alejandra.package = pkgs.alejandra;
          xi.fmt.tools.pedantix.package = inputs.pedantix.packages.''${system}.pedantix-wrapped;
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      (lib.mkIf (cfg.fmt.backend != "auto") {
        xi.settings.fmt.backend = cfg.fmt.backend;
      })
    ]
  );
}
