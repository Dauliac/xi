{ lib, ... }:
{
  options._xi.types.tool = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Build tool configuration submodule type (enable + package).";
  };

  config._xi.types.tool = lib.types.submodule {
    options = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Whether to make this tool available to xi.";
      };

      package = lib.mkOption {
        type = lib.types.nullOr lib.types.package;
        default = null;
        description = "The package providing this tool. Set to null to disable.";
      };
    };
  };
}
