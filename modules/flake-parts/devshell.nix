{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.xi;
  allHooks = if cfg.shellHook.enable then cfg.shellHookScript else cfg.completionShellHook;
in
{
  options.xi.devshell = lib.mkOption {
    type = config._xi.types.devshell;
    default = { };
    description = "Development shell integration for xi.";
  };

  options.xi.devshellPackages = lib.mkOption {
    type = lib.types.listOf lib.types.package;
    readOnly = true;
    description = "Packages to add to your devShell for xi integration.";
  };

  options.xi.wrapDevShell = lib.mkOption {
    type = lib.types.functionTo lib.types.package;
    readOnly = true;
    description = ''
      Convenience function that augments an existing devShell derivation
      with xi packages, shell hooks, and completions.

      Usage: devShells.default = config.xi.wrapDevShell (pkgs.mkShellNoCC { ... });
    '';
  };

  config.xi.devshellPackages =
    if cfg.enable && cfg.devshell.enable then [ cfg.finalPackage ] else [ ];

  config.xi.wrapDevShell =
    shell:
    shell.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ cfg.devshellPackages;
      shellHook = (old.shellHook or "") + allHooks;
    });

  config.devShells = lib.mkIf (cfg.enable && cfg.devshell.enable) {
    xi = pkgs.mkShellNoCC {
      packages = cfg.devshellPackages;
      shellHook = allHooks;
    };
  };
}
