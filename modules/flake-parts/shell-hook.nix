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
  options.xi.shellHook = lib.mkOption {
    type = config._xi.types.shellHook;
    default = { };
    description = ''
      Shell hook configuration for xi.
      Sub-options control which pieces are active: nix alias
      and completions.
    '';
  };

  options.xi.shellHookScript = lib.mkOption {
    type = lib.types.str;
    readOnly = true;
    description = "Generated composed shell hook script (posix, composable into devShells).";
  };

  config.xi.shellHookScript =
    if cfg.enable && cfg.shellHook.enable then
      config.lib.mkComposedShellHook {
        inherit pkgs;
        inherit (cfg)
          package
          shellHook
          binPath
          ;
        nixPackage = cfg.nix.package;
      }
    else
      "";
}
