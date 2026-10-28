{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.xi;
in
{
  options.programs.xi.sources = lib.mkOption {
    type = config._xi.types.sources;
    default = { };
    description = ''
      System-scope config-repo sources.

      Writes `/etc/xi/sources.toml` and pre-creates `/var/lib/xi/sources`
      and `/var/lib/xi/state` at activation, so the directories exist
      before any `xi` command or gitops unit runs.

      This is the fourth rung of the precedence ladder: a CLI flag, an
      `XI_SOURCES_*` variable, or a project `.xi.toml [sources]` key all
      still override what is set here.

      Example:
        programs.xi.sources = {
          enable = true;
          remote = "git@github.com:you/nix-config.git";
          pollInterval = "5m";
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

    environment.etc."xi/sources.toml".source = config.lib.mkSourcesFile {
      inherit pkgs;
      inherit (cfg) sources;
    };

    # 0700 root:root, created at activation time. The state dir records
    # what the host already deployed, and the sources dir is a clone xi
    # trusts; neither should be world-readable, and both must exist
    # before the first xi invocation rather than being created by it.
    systemd.tmpfiles.rules = [
      "d /var/lib/xi 0700 root root -"
      "d /var/lib/xi/sources 0700 root root -"
      "d /var/lib/xi/state 0700 root root -"
    ];
  };
}
