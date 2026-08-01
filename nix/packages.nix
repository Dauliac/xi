{
  lib,
  ...
}:
{
  perSystem =
    {
      config,
      pkgs,
      inputs',
      ...
    }:
    let
      inherit (config._xi)
        craneLib
        commonArgs
        xiBinaryArtifacts
        ;
    in
    {
      packages = {
        xi = craneLib.buildPackage (
          commonArgs
          // {
            cargoArtifacts = xiBinaryArtifacts;
            doCheck = false;

            postInstall =
              lib.optionalString (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
                $out/bin/xtask dist
                installShellCompletion --cmd xi ./comp/*.{bash,fish,zsh,nu}
                installManPage ./man/xi.1
              ''
              + ''
                rm $out/bin/xtask
              '';

            postFixup = ''
              wrapProgram $out/bin/xi \
                --prefix PATH : ${
                  lib.makeBinPath [
                    pkgs.nix-output-monitor
                    pkgs.nix-fast-build
                    inputs'.nix-auth.packages.default
                  ]
                }
            '';

            meta = {
              description = "Yet another nix cli helper";
              homepage = "https://github.com/Dauliac/xi";
              license = lib.licenses.eupl12;
              mainProgram = "xi";
              maintainers = with lib.maintainers; [
                drupol
                faukah
                NotAShelf
                viperML
              ];
            };
          }
        );
        default = config.packages.xi;
      };
    };
}
