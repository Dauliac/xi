{
  ...
}:
{
  perSystem =
    { config, ... }:
    let
      inherit (config._xi)
        craneLib
        commonArgs
        cargoArtifactsCheck
        ;
    in
    {
      checks = {
        xi = config.packages.xi;

        xi-clippy = craneLib.cargoClippy (
          commonArgs
          // {
            cargoArtifacts = cargoArtifactsCheck;
            cargoClippyExtraArgs = "--workspace -- --deny warnings";
          }
        );

        xi-doc = craneLib.cargoDoc (
          commonArgs
          // {
            cargoArtifacts = cargoArtifactsCheck;
            RUSTDOCFLAGS = "-D warnings";
          }
        );
      };
    };
}
