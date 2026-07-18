{ config, lib, ... }:
let
  cfg = config.programs.xi;
in
{
  # Completions are controlled via shellHook.completion (enabled by default).
  # This standalone path handles the case where shellHook is disabled but
  # the user still wants completions registered.
  config = lib.mkIf (cfg.enable && !cfg.shellHook.enable && cfg.shellHook.completion) {
    environment.interactiveShellInit = config._xi.lib.mkCompletionShellHook {
      inherit (cfg) package;
      inherit (cfg) binPath;
    };
  };
}
