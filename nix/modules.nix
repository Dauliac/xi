{
  inputs,
  self,
  ...
}:
{
  flake.nixosModules.default = {
    imports = [
      (inputs.import-tree ../modules/shared)
      (inputs.import-tree ../modules/nixos)
    ];
    _module.args.xiFlake = self;
  };

  flake.homeManagerModules.default = {
    imports = [
      (inputs.import-tree ../modules/shared)
      (inputs.import-tree ../modules/home-manager)
    ];
    _module.args.xiFlake = self;
  };

  flake.flakeModule = {
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
