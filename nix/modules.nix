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

  # A narrower `.default`, for a consumer that cannot import it as-is:
  # system-manager's reduced NixOS option surface does not declare
  # `environment.interactiveShellInit`/`programs.fish.interactiveShellInit`
  # (the two option paths `modules/nixos/shell-hook.nix` and
  # `modules/nixos/completion.nix` assign - a hard module-merge error
  # regardless of `shellHook.enable`'s value, since `lib.mkIf` gates the
  # value a config block contributes, not whether the option path it
  # assigns into has to exist), and a consumer who already imports a
  # *different* class head of the same underlying nix-lib flake (e.g.
  # `inputs.nlib.systemManagerModules.default`, via `nix-lib.follows`)
  # gets `option nix-lib.scanNestedSystems is already declared` if this
  # module also imports `inputs.nix-lib.nixosModules.default` itself -
  # two module-system classes, same option path, no declaration-clash
  # equivalent of `lib.mkForce` exists (xi-9m39).
  #
  # Precondition this module relies on instead of satisfying itself: the
  # consumer's own evaluation must already provide *some* nix-lib module
  # class (`nixosModules`, `homeModules`, or `systemManagerModules` - any
  # one) so the `nix-lib.lib` namespace `modules/shared/lib/sources.nix`
  # writes into, and the `nix-lib.enable` option this module sets, both
  # already exist. Every `nix-lib.follows`-based consumer already
  # arranges this for itself.
  flake.nixosModules.sources = {
    imports = [
      (inputs.import-tree ../modules/shared)
      ../modules/nixos/enable.nix
      ../modules/nixos/settings.nix
      ../modules/nixos/package.nix
      ../modules/nixos/sources.nix
      ../modules/nixos/agents.nix
      ../modules/nixos/wrapper.nix
      ../modules/nixos/fmt.nix
      ../modules/nixos/test.nix
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
