{ lib, ... }:
{
  options.programs.xi.enable = lib.mkEnableOption "xi, yet another nix cli helper";
}
