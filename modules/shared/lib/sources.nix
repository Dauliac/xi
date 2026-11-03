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
        #   credentials.*         (xi-sources-credentials)
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

        # Null means "this module does not pin the key", which is not the
        # same as pinning a default: dropping it keeps the rung empty so
        # the built-in default still answers and the provenance map says
        # `default` rather than `nix-module`.
        set = lib.filterAttrs (_: v: v != null) (
          lib.mapAttrs (_: v: if lib.isPath v then toString v else v) keys
        );
      in
      tomlFormat.generate "xi-sources.toml" { sources = set; };
    description = "Generate a sources.toml store path from a sources attrset.";
  };
}
