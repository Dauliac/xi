{ lib, ... }:
{
  options._xi.types.settings = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Freeform TOML settings type for xi config.toml.";
  };

  # The type is a function that takes pkgs and returns the submodule type,
  # because pkgs.formats.toml needs pkgs at call time.
  config._xi.types.settings =
    pkgs:
    let
      tomlFormat = pkgs.formats.toml { };
    in
    lib.types.submodule {
      freeformType = tomlFormat.type;
    };
}
