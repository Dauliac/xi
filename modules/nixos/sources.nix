{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.xi;

  # Present only when home-manager is imported as a NixOS module, which
  # is the one arrangement where both scopes land in a single
  # evaluation and the cross-scope conflict is therefore detectable.
  # With a standalone home-manager there is no shared eval, so the
  # daemons discover each other at runtime instead (see
  # xi-sources-race).
  hmUsers = config.home-manager.users or { };

  gitopsEnabledAt =
    userCfg:
    let
      sources = userCfg.programs.xi.sources or null;
    in
    sources != null && sources.enable && sources.gitops.enable;

  # Any embedded user with their own gitops daemon on, regardless of
  # which remote they track. Broader than a same-remote check on
  # purpose: when home-manager is embedded like this, system-manager/
  # NixOS already owns activation for every one of `hmUsers` (it is
  # what builds and switches the combined generation they are part
  # of), so a user-scope daemon independently re-activating
  # home-manager on its own schedule races that one generation no
  # matter whose remote it polls - two daemons touching the same
  # activation path is the conflict, not two daemons touching the same
  # git history.
  gitopsEnabledHmUsers = lib.filter (user: gitopsEnabledAt hmUsers.${user}) (
    lib.attrNames hmUsers
  );
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
        assertion =
          cfg.sources.requireSigned != true
          || cfg.sources.allowedSigners != null
          || cfg.sources.gpgPublicKeys != [ ];
        message = ''
          programs.xi.sources.requireSigned needs a trust root: set
          programs.xi.sources.allowedSigners to an SSH allowed_signers
          file, or programs.xi.sources.gpgPublicKeys to a non-empty list
          of GPG public keys. Verifying against an empty trust root is
          worse than not verifying, because it looks safe.
        '';
      }

      {
        assertion = cfg.sources.workingDir == null;
        message = ''
          programs.xi.sources.workingDir is user scope only, and this is
          the system-scope module. A system-wide option cannot name one
          user's editable checkout; the system sources dir is
          /var/lib/xi/sources and has no working dir of its own. Move
          the setting to home-manager's programs.xi.sources.workingDir.
        '';
      }

      {
        assertion = !cfg.sources.gitops.enable || gitopsEnabledHmUsers == [ ];
        message = ''
          programs.xi.sources.gitops.enable is true at system scope and
          at user scope for ${lib.concatStringsSep ", " gitopsEnabledHmUsers}
          (embedded home-manager on this same host). The gitops daemon
          can run at exactly one scope here: system-manager/NixOS already
          owns activation for every embedded user, so an embedded user's
          own daemon independently re-activating home-manager races that
          same generation regardless of which remote each scope tracks.
          Enable gitops at system scope only, or at exactly one embedded
          user's scope with system-scope gitops left off - never both.
        '';
      }
    ];

    environment.etc."xi/sources.toml".source = config.lib.mkSourcesFile {
      inherit pkgs;
      inherit (cfg) sources;
    };

    # root:xi-status, created at activation time, neither world-writable
    # nor world-readable. The top dir and the sources dir (a clone xi
    # trusts) are group-*read*-only, 0750: `xi system status`/`xi system
    # sources status` only ever read them, and gating that read behind
    # sudo made the read-only commands as privileged as `switch`/`build`.
    #
    # The state dir is group-*write*, 0770: `xi deploy`/`xi system switch`
    # run unprivileged and elevate internally only for the activation
    # itself (crates/xi/src/system/mod.rs refuses to even start as root),
    # so `record_activation`'s write of the new generation - bookkeeping
    # metadata, not the trusted source - happens back in the unprivileged
    # caller, after the elevated step returns. It is deliberately
    # best-effort (a failed write only warns, never fails the command -
    # see `record_activation`'s own doc comment) but should still
    # succeed on every normal run rather than silently degrade `xi
    # system status` to `Pending` forever.
    #
    # Every embedded home-manager user on this host (`hmUsers`, below) is
    # a member of `xi-status` by default; an operator can extend
    # `users.groups.xi-status.members` for anyone else who should be able
    # to run these without sudo.
    systemd.tmpfiles.rules = [
      "d /var/lib/xi 0750 root xi-status -"
      "d /var/lib/xi/sources 0750 root xi-status -"
      "d /var/lib/xi/state 0770 root xi-status -"
    ];

    users.groups.xi-status.members = lib.attrNames hmUsers;

    # The pull-GitOps loop (xi-sources-gitops). Unit name is fixed at
    # `xi-sources-daemon` - `xi_core::sources::log::SYSTEMD_UNIT` and
    # `xi sources log tail` both hard-code it to shell out to
    # `journalctl -u xi-sources-daemon`, so renaming this stanza's
    # attribute name would silently break that readout.
    systemd.services.xi-sources-daemon = lib.mkIf cfg.sources.gitops.enable {
      description = "xi sources gitops daemon (system scope)";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];

      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "xi-sources-daemon"} --scope system";
        Restart = "on-failure";
        RestartSec = "10s";

        # journald is the structured sink `xi_sources_observability::journald`
        # writes to over the native socket; routing stdout/stderr there too
        # keeps a crash's raw output next to the structured fields under
        # the same `journalctl -u xi-sources-daemon`.
        StandardOutput = "journal";
        StandardError = "journal";
      };
    };
  };
}
