{
  inputs,
  lib,
  ...
}:
{
  systems = [
    "x86_64-linux"
    "aarch64-linux"
    "x86_64-darwin"
    "aarch64-darwin"
  ];

  imports = [
    inputs.treefmt-nix.flakeModule
    inputs.flake-parts.flakeModules.easyOverlay
    inputs.nix-lib.flakeModules.default
    # Auto-discover all .nix modules in this directory using import-tree.
    # Files prefixed with _ are excluded by default (dendritic pattern).
    # Filter out default.nix itself to avoid infinite recursion.
    ((inputs.import-tree.filterNot (path: lib.hasSuffix "/default.nix" path)) ./.)
  ];
}
