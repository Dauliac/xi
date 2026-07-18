{ lib, ... }:
{
  options._xi.lib.mkConfigFile = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Generate a config.toml store path from settings attrset.";
  };

  config._xi.lib.mkConfigFile =
    { pkgs, settings }:
    let
      tomlFormat = pkgs.formats.toml { };
      filtered = lib.filterAttrsRecursive (_: v: v != null) settings;
    in
    if filtered == { } then null else tomlFormat.generate "xi-config.toml" filtered;
}
