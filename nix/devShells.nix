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
            # The devshell real-shell test layer evaluates xi's own generated
            # shell code in an actual zsh (crates/xi/tests/e2e_devshell_*).
            # Without zsh on PATH those tests skip themselves, so the suite
            # stays green while the cd-out NO_CLOBBER regression anchor never
            # executes. CI runs `nix develop --command cargo test`, so this is
            # the surface that decides whether that anchor actually runs.
            pkgs.zsh
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
