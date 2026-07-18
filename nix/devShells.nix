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

      devShells.default = craneLib.devShell {
        checks = config.checks;

        packages = [
          (pkgs.rustfmt.override { asNightly = true; })
          pkgs.rust-analyzer-unwrapped
          pkgs.clippy
          pkgs.taplo
          pkgs.lldb
          pkgs.yaml-language-server
          pkgs.cargo-nextest
          pkgs.just
          pkgs.deno
        ];

        env = {
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };

        shellHook = config.xi.shellHookScript;
      };
    };
}
