{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.xi;

  # The user-scope path table, per the xi-sources-config spec. Resolved
  # here from home-manager's own XDG options so the module and the CLI
  # agree even when the user has moved an XDG root.
  sourcesDir = "${config.xdg.dataHome}/xi/sources";
  stateDir = "${config.xdg.stateHome}/xi";
in
{
  options.programs.xi.sources = lib.mkOption {
    type = config._xi.types.sources;
    default = { };
    description = ''
      User-scope config-repo sources.

      Writes `$XDG_CONFIG_HOME/xi/sources.toml` and creates
      `$XDG_DATA_HOME/xi/sources` and `$XDG_STATE_HOME/xi` at activation.

      This is the fourth rung of the precedence ladder: a CLI flag, an
      `XI_SOURCES_*` variable, or a project `.xi.toml [sources]` key all
      still override what is set here.

      Example:
        programs.xi.sources = {
          enable = true;
          remote = "git@github.com:you/dotfiles.git";
          workingDir = "/home/you/src/dotfiles";
        };
    '';
  };

  config = lib.mkIf (cfg.enable && cfg.sources.enable) {
    assertions = [
      {
        assertion = cfg.sources.requireSigned != true || cfg.sources.allowedSigners != null;
        message = ''
          programs.xi.sources.requireSigned requires
          programs.xi.sources.allowedSigners to point at an SSH
          allowed_signers file. Verifying against an empty trust root is
          worse than not verifying, because it looks safe.
        '';
      }
    ];

    xdg.configFile."xi/sources.toml".source = config.lib.mkSourcesFile {
      inherit pkgs;
      inherit (cfg) sources;
    };

    # A home-manager activation block rather than systemd.user.tmpfiles,
    # because this module also has to work on nix-darwin, where there is
    # no systemd to run the rules.
    home.activation.xiSourcesDirs = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
      run mkdir -p ${lib.escapeShellArg sourcesDir} ${lib.escapeShellArg stateDir}
      run chmod 700 ${lib.escapeShellArg sourcesDir} ${lib.escapeShellArg stateDir}
    '';
  };
}
