{
  ...
}:
{
  perSystem =
    { pkgs, ... }:
    {
      treefmt = {
        projectRootFile = "flake.nix";

        programs = {
          # Nix
          nixfmt.enable = true;
          deadnix.enable = true;

          # Rust
          rustfmt = {
            enable = true;
            edition = "2024";
            # .rustfmt.toml sets `unstable_features = true` and four
            # options that only nightly honours (struct field alignment,
            # import layout, comment wrapping, string formatting). Stable
            # rustfmt ignores them and reformats every file the devshell
            # just formatted, so the two formatters would fight over most
            # of the tree and `treefmt --check` could never agree with
            # `just check`. Same binary as nix/devShells.nix's RUSTFMT.
            package = pkgs.rustfmt.override { asNightly = true; };
          };

          # TOML
          taplo.enable = true;

          # Markdown
          deno.enable = true;

          # Spell checker
          typos.enable = true;

          # Shell scripts
          shellcheck.enable = true;
          shfmt.enable = true;
        };
      };
    };
}
