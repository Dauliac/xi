{ lib, ... }:
{
  options._xi.types.devshell = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Devshell configuration submodule type.";
  };

  config._xi.types.devshell = lib.types.submodule {
    options.enable = lib.mkEnableOption "xi in the development shell";
  };
}
