{ config, lib, ... }:
let
  cfg = config.xi;
in
{
  # Completions are controlled via shellHook.completion (enabled by default).
  # This read-only option exposes the standalone completion hook for cases
  # where the user composes shell hooks manually.
  options.xi.completionShellHook = lib.mkOption {
    type = lib.types.str;
    readOnly = true;
    description = "Generated completion shell hook (composable into devShells).";
  };

  config.xi.completionShellHook =
    if cfg.enable && cfg.shellHook.completion then
      config._xi.lib.mkCompletionShellHook {
        inherit (cfg) package;
        inherit (cfg) binPath;
      }
    else
      "";
}
