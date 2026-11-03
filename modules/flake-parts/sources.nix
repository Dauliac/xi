{
  config,
  lib,
  ...
}:
let
  cfg = config.xi;

  # flake-parts' perSystem has no `assertions` option, so the same
  # invariant the other two heads express as an assertion is a throw
  # here. The message is kept identical in substance so a user who
  # moves a config between scopes reads the same explanation.
  missingTrustRoot =
    cfg.sources.requireSigned == true
    && cfg.sources.allowedSigners == null
    && cfg.sources.gpgPublicKeys == [ ];
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
    # Only the keys xi's closed `[sources]` key set accepts; the rest of
    # the option tree is declared for parity but has no project scope
    # surface yet. See modules/shared/lib/sources.nix.
    #
    # The throw sits on the value, not on the attrset: forcing a
    # condition that reads `config` while the module system is still
    # collecting config keys is an infinite recursion.
    xi.settings.sources =
      lib.throwIf missingTrustRoot ''
        xi.sources.requireSigned needs a trust root: set
        xi.sources.allowedSigners to an SSH allowed_signers file, or
        xi.sources.gpgPublicKeys to a non-empty list of GPG public keys.
        Verifying against an empty trust root is worse than not
        verifying, because it looks safe.
      ''
        (
          lib.filterAttrs (_: v: v != null) {
            inherit (cfg.sources) remote branch;
            installer = cfg.sources.bootstrap.installer;
            working-dir =
              if cfg.sources.workingDir == null then null else toString cfg.sources.workingDir;
            poll-interval = cfg.sources.pollInterval;
            require-signed = cfg.sources.requireSigned;
            allowed-signers =
              if cfg.sources.allowedSigners == null then null else toString cfg.sources.allowedSigners;
            sync-mode = cfg.sources.syncMode;
          }
        );
  };
}
