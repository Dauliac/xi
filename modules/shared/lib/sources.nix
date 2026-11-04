{ lib, ... }:
{
  nix-lib.lib.mkSourcesFile = {
    type = lib.types.raw;
    fn =
      { pkgs, sources }:
      let
        tomlFormat = pkgs.formats.toml { };

        # camelCase option -> the kebab-case key xi's parser accepts.
        # The key set is closed on the Rust side (`SOURCES_KEYS` in
        # crates/xi-core/src/sources/config.rs), so a name invented here
        # would be a load-time error rather than a silent no-op. That
        # cuts both ways: the option tree in
        # modules/shared/types/sources.nix is deliberately wider than
        # this map, and an option only joins it once the change that
        # owns it has added the matching key on the Rust side.
        #
        # Declared in the tree but NOT renderable yet:
        #   gpgPublicKeys         (xi-sources-gitops)
        #   gitops.*              (xi-sources-gitops)
        #   cachePrestage.*       (xi-sources-cache-prestage)
        #   secrets.*             (xi-sources-push)
        keys = {
          remote = sources.remote;
          branch = sources.branch;
          "working-dir" = sources.workingDir;
          "poll-interval" = sources.pollInterval;
          "require-signed" = sources.requireSigned;
          "allowed-signers" = sources.allowedSigners;
          "sync-mode" = sources.syncMode;
          installer = sources.bootstrap.installer;
        };

        # `[sources.credentials.<kind>]` - crates/xi-core/src/sources/credentials.rs.
        # Only the genuinely nullable, secret-path-shaped fields are
        # rendered: `identityFile`/`tokenFile`/`credentialHelper` are
        # already `str` (never `path`), so, like the keys above, they
        # never enter the Nix store. `useAgent`/`username` are left out
        # on purpose: they carry concrete, non-null defaults that already
        # match xi's own built-in defaults, so rendering them here would
        # only flip their provenance from `default` to `nix-module` with
        # no behavioural effect.
        sshCredentials = lib.filterAttrs (_: v: v != null) {
          "identity-file" = sources.credentials.ssh.identityFile;
        };
        httpsCredentials = lib.filterAttrs (_: v: v != null) {
          "token-file" = sources.credentials.https.tokenFile;
          "credential-helper" = sources.credentials.https.credentialHelper;
        };
        credentials = lib.filterAttrs (_: v: v != { }) {
          ssh = sshCredentials;
          https = httpsCredentials;
        };

        # Null means "this module does not pin the key", which is not the
        # same as pinning a default: dropping it keeps the rung empty so
        # the built-in default still answers and the provenance map says
        # `default` rather than `nix-module`.
        set =
          lib.filterAttrs (_: v: v != null) (
            lib.mapAttrs (_: v: if lib.isPath v then toString v else v) keys
          )
          // lib.optionalAttrs (credentials != { }) { inherit credentials; };

        # The build-location ladder's Nix-module rung: a sibling
        # `[push]` table in the same file, read by
        # `xi_deploy::push::config::PushConfig::from_toml_file` pointed
        # at this same per-scope path. `{ }` renders no `[push]` table
        # at all (`tomlFormat.generate` drops an empty attrset), which
        # is what keeps the rung silent when `remoteBuild` is null.
        pushKeys = lib.filterAttrs (_: v: v != null) {
          "remote-build" = sources.push.remoteBuild;
        };
      in
      tomlFormat.generate "xi-sources.toml" (
        { sources = set; } // lib.optionalAttrs (pushKeys != { }) { push = pushKeys; }
      );
    description = "Generate a sources.toml store path from a sources attrset.";
  };
}
