{
  inputs,
  self,
  ...
}:
{
  flake.nixosModules.default = {
    imports = [
      inputs.nix-lib.nixosModules.default
      (inputs.import-tree ../modules/shared)
      (inputs.import-tree ../modules/nixos)
    ];
    nix-lib.enable = true;
    _module.args.xiFlake = self;
  };

  # A standalone export of the `xi-vm-lifecycle` guest-provisioning
  # module, for a consumer who wants it without the rest of xi's
  # `nixosModules.default` (e.g. composing it onto a `nixosConfiguration`
  # by hand rather than through `flake.flakeModule`'s `extendModules`
  # wiring in `modules/flake-parts/vm.nix`).
  flake.nixosModules.vmGuest = {
    imports = [ ../modules/nixos/vm-guest.nix ];
  };

  flake.homeManagerModules.default = {
    imports = [
      inputs.nix-lib.homeModules.default
      (inputs.import-tree ../modules/shared)
      (inputs.import-tree ../modules/home-manager)
    ];
    nix-lib.enable = true;
    _module.args.xiFlake = self;
  };

  flake.flakeModule = {
    imports = [
      inputs.nix-lib.flakeModules.default
    ];
    perSystem =
      { ... }:
      {
        imports = [
          (inputs.import-tree ../modules/shared)
          (inputs.import-tree ../modules/flake-parts)
        ];
        _module.args.xiFlake = self;
      };
  };
}
