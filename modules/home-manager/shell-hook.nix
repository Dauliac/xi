{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.xi;
  hookArgs = {
    inherit pkgs;
    inherit (cfg)
      package
      shellHook
      binPath
      ;
    nixPackage = cfg.nix.package;
  };
in
{
  options.programs.xi.shellHook = lib.mkOption {
    type = config._xi.types.shellHook;
    default = { };
    description = ''
      Shell hook configuration for xi.
      Sub-options control which pieces are active: nix alias
      and completions.
    '';
  };

  config = lib.mkIf (cfg.enable && cfg.shellHook.enable) {
    programs.bash.initExtra = config._xi.lib.mkComposedShellHook hookArgs;
    programs.zsh.initContent = config._xi.lib.mkComposedShellHook hookArgs;
    programs.fish.interactiveShellInit = config._xi.lib.mkComposedFishShellHook hookArgs;
  };
}
