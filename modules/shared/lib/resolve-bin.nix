{ lib, ... }:
{
  nix-lib.lib.resolveXiBin = {
    type = lib.types.raw;
    fn =
      { package, binPath }:
      if binPath != null then binPath else lib.getExe package;
    description = "Resolve xi binary path with optional override and fallback.";
  };
}
