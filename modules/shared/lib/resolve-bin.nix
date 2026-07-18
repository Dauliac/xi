{ lib, ... }:
{
  options._xi.lib.resolveXiBin = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Resolve xi binary path with optional override and fallback.";
  };

  config._xi.lib.resolveXiBin =
    { package, binPath }:
    if binPath != null then binPath else lib.getExe package;
}
