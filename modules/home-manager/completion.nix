{ config, lib, ... }:
let
  cfg = config.programs.xi;
  binArgs = {
    inherit (cfg) package;
    inherit (cfg) binPath;
  };
in
{
  # Completions are controlled via shellHook.completion (enabled by default).
  # This handles the case where shellHook is disabled but the user still
  # wants completions registered per-shell.
  config = lib.mkIf (cfg.enable && !cfg.shellHook.enable && cfg.shellHook.completion) {
    programs.bash.initExtra = config.lib.mkBashCompletionScript binArgs;
    programs.zsh.initContent = config.lib.mkZshCompletionScript binArgs;
    programs.fish.interactiveShellInit = config.lib.mkFishCompletionScript binArgs;
  };
}
