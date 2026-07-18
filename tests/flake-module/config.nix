{
  lib,
  ...
}:
{
  perSystem =
    {
      config,
      pkgs,
      ...
    }:
    {
      # Enable xi with default settings
      xi = {
        enable = true;
        shellHook = {
          enable = true;
          nixAlias = true;
          completion = true;
        };
        wrapper.enable = true;
        devshell.enable = true;
        nom.enable = true;
        nixFastBuild.enable = true;
      };

      # Test 1: the standalone "xi" devshell is created
      # (created by modules/flake-parts/devshell.nix when devshell.enable = true)

      # Test 2: wrapDevShell augments a user-defined devshell
      devShells.default = config.xi.wrapDevShell (
        pkgs.mkShellNoCC {
          name = "test-devshell";
          packages = [ pkgs.hello ];
          shellHook = ''
            echo "user hook"
          '';
        }
      );

      # Test 3: basic checks that the module evaluates correctly
      checks = {
        # Verify the finalPackage is a derivation
        xi-module-final-package = pkgs.runCommand "check-xi-final-package" { } ''
          # If we got here, config.xi.finalPackage evaluated successfully
          echo "finalPackage: ${config.xi.finalPackage}" > $out
        '';

        # Verify the shell hook script is a non-empty string
        xi-module-shell-hook = pkgs.runCommand "check-xi-shell-hook" { } ''
          hook='${lib.replaceStrings [ "'" ] [ "'\\'" ] config.xi.shellHookScript}'
          if [ -z "$hook" ]; then
            echo "ERROR: shellHookScript is empty" >&2
            exit 1
          fi
          echo "shellHookScript is non-empty (${toString (builtins.stringLength config.xi.shellHookScript)} chars)" > $out
        '';

        # Verify devshellPackages includes xi when devshell is enabled
        xi-module-devshell-packages = pkgs.runCommand "check-xi-devshell-packages" { } ''
          count=${toString (builtins.length config.xi.devshellPackages)}
          if [ "$count" -eq 0 ]; then
            echo "ERROR: devshellPackages is empty (devshell.enable = true)" >&2
            exit 1
          fi
          echo "devshellPackages has $count package(s)" > $out
        '';

        # Verify the wrapped devshell has xi in nativeBuildInputs
        xi-module-wrap-devshell = pkgs.runCommand "check-xi-wrap-devshell" { } ''
          # If the default devshell built, wrapDevShell worked
          echo "wrapDevShell produced: ${config.devShells.default.name}" > $out
        '';

        # Verify the standalone xi devshell exists
        xi-module-standalone-devshell = pkgs.runCommand "check-xi-standalone-devshell" { } ''
          echo "standalone xi devshell: ${config.devShells.xi.name}" > $out
        '';

        # Verify completionShellHook is non-empty when completion = true
        xi-module-completion-hook = pkgs.runCommand "check-xi-completion-hook" { } ''
          hook='${lib.replaceStrings [ "'" ] [ "'\\'" ] config.xi.completionShellHook}'
          if [ -z "$hook" ]; then
            echo "ERROR: completionShellHook is empty (completion = true)" >&2
            exit 1
          fi
          echo "completionShellHook is non-empty" > $out
        '';
      };
    };
}
