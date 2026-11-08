{
  description = "xi flake-parts module integration test";

  inputs = {
    get-flake.url = "github:ursi/get-flake";
    nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    # Only this test fixture's own flake, not xi's: a `kind = "distro"`
    # entry needs a `nix-vm-test` backend (see modules/flake-parts/vm.nix's
    # `xiVmTestInput` perSystem arg), which xi's own flake.nix does not add
    # (adding a new third-party input to the shipped flake is a different,
    # bigger call than exercising it in a disposable test fixture).
    nix-vm-test.url = "github:numtide/nix-vm-test";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    let
      xi = inputs.get-flake ../..;
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        xi.flakeModule
        ./config.nix
      ];

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem = {
        _module.args.xiVmTestInput = inputs.nix-vm-test;
      };
    };
}
