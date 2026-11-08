{ pkgs, ... }:
{
  fileSystems."/" = {
    device = "none";
    fsType = "tmpfs";
    options = [ "size=1G" ];
  };

  boot.loader.grub.devices = [ "nodev" ];
  system.stateVersion = "24.05";

  environment.systemPackages = [ pkgs.hello ];
}
