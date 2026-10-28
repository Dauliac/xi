{ lib, ... }:
{
  nix-lib.lib.mkSourcesFile = {
    type = lib.types.raw;
    fn =
      { pkgs, sources }:
      let
        tomlFormat = pkgs.formats.toml { };

        # camelCase option -> the kebab-case key xi's parser accepts.
        # The key set is closed on the Rust side, so a name invented
        # here would be a load-time error rather than a silent no-op.
        keys = {
          remote = sources.remote;
          branch = sources.branch;
          "working-dir" = sources.workingDir;
          "poll-interval" = sources.pollInterval;
          "require-signed" = sources.requireSigned;
          "allowed-signers" = sources.allowedSigners;
          scope = sources.scope;
          "sync-mode" = sources.syncMode;
          installer = sources.installer;
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
