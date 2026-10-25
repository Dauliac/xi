{
  inputs,
  self,
  ...
}:
{
  perSystem =
    {
      config,
      pkgs,
      ...
    }:
    let
      inherit (config._xi) craneLib;
    in
    {
      imports = [
        (inputs.import-tree ../modules/shared)
        (inputs.import-tree ../modules/flake-parts)
      ];

      _module.args.xiFlake = self;

      xi = {
        enable = true;
        binPath = "./target/debug/xi";
        nom.enable = true;
        nixFastBuild.enable = true;
        shellHook = {
          enable = true;
          nixAlias = true;
          completion = true;
        };
        wrapper.enable = true;
      };

      devShells.default =
        let
          rustfmtNightly = pkgs.rustfmt.override { asNightly = true; };
        in
        craneLib.devShell {
          checks = config.checks;

          packages = [
            rustfmtNightly
            pkgs.rust-analyzer-unwrapped
            pkgs.clippy
            pkgs.taplo
            pkgs.lldb
            pkgs.yaml-language-server
            pkgs.cargo-nextest
            pkgs.cargo-insta
            pkgs.just
            pkgs.deno
          ];

          env = {
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            # cargo fmt uses `<cargo-sysroot>/bin/rustfmt` by default, which
            # is the stable rustfmt from pkgs.cargo. Point it at the
            # asNightly binary so unstable rustfmt options take effect.
            RUSTFMT = "${rustfmtNightly}/bin/rustfmt";
          };

          shellHook = config.xi.shellHookScript;
        };
    };
}
