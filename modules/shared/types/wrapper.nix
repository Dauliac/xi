{ lib, ... }:
{
  options._xi.types.wrapper = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Wrapper configuration submodule type.";
  };

  config._xi.types.wrapper = lib.types.submodule {
    options.enable = lib.mkEnableOption "wrapped xi with baked-in configuration";
  };
}
