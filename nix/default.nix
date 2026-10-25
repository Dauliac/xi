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
    inputs.flake-parts.flakeModules.partitions
    inputs.nix-lib.flakeModules.default
    # Auto-discover all .nix modules in this directory using import-tree.
    # Files prefixed with _ are excluded by default (dendritic pattern).
    # Filter out default.nix itself and docs-partition-only modules so
    # they don't leak into consumer flakes.
    (
      (inputs.import-tree.filterNot (
        path:
        lib.hasSuffix "/default.nix" path
        || lib.hasSuffix "/docs.nix" path
        || lib.hasSuffix "/github-actions.nix" path
      ))
      ./.
    )
  ];

  # Doc-only outputs live in the `docs` partition so flake-parts-website
  # and github-actions-nix don't leak into consumers of xi's flake outputs.
  partitionedAttrs.legacyPackages = "docs";

  partitions.docs = {
    extraInputsFlake = ../docs;
    module =
      { inputs, ... }:
      {
        imports = [
          ../nix/docs.nix
          ../nix/github-actions.nix
        ];
        _module.args.docsInputs = inputs;
      };
  };
}
