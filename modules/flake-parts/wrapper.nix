{ config, lib, ... }:
{
  options.xi.wrapper = lib.mkOption {
    type = config._xi.types.wrapper;
    default = { };
    description = "Wrapper configuration that bakes the xi config into the binary.";
  };
}
