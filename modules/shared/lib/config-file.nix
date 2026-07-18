{ lib, ... }:
{
  nix-lib.lib.mkConfigFile = {
    type = lib.types.raw;
    fn =
      { pkgs, settings }:
      let
        tomlFormat = pkgs.formats.toml { };
        filtered = lib.filterAttrsRecursive (_: v: v != null) settings;
      in
      if filtered == { } then null else tomlFormat.generate "xi-config.toml" filtered;
    description = "Generate a config.toml store path from settings attrset.";
  };
}
