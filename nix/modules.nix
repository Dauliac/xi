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
