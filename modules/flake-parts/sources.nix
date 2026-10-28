{
  config,
  lib,
  ...
}:
let
  cfg = config.xi;
in
{
  options.xi.sources = lib.mkOption {
    type = config._xi.types.sources;
    default = { };
    description = ''
      Project-scope config-repo sources.

      A flake-parts facade over the same option tree the NixOS and
      home-manager modules expose. There is no activation step at this
      scope and no `/etc` to write to, so the values are folded into the
      wrapper's settings instead: entering the devShell gets a baked-in
      `[sources]` block without the repo having to commit a `.xi.toml`.

      Because this lands in the wrapper settings rather than in a
      per-scope `sources.toml`, a project `.xi.toml [sources]` key, an
      `XI_SOURCES_*` variable, and a CLI flag all still override it.

      Example:
        xi.sources = {
          enable = true;
          remote = "git@github.com:you/nix-config.git";
        };
    '';
  };

  config = lib.mkIf (cfg.enable && cfg.sources.enable) {
    xi.settings.sources = lib.filterAttrs (_: v: v != null) {
      inherit (cfg.sources) remote branch scope installer;
      working-dir = if cfg.sources.workingDir == null then null else toString cfg.sources.workingDir;
      poll-interval = cfg.sources.pollInterval;
      require-signed = cfg.sources.requireSigned;
      allowed-signers =
        if cfg.sources.allowedSigners == null then null else toString cfg.sources.allowedSigners;
      sync-mode = cfg.sources.syncMode;
    };
  };
}
