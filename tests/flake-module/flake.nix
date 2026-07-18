{
  description = "xi flake-parts module integration test";

  inputs = {
    get-flake.url = "github:ursi/get-flake";
    nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
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
    };
}
