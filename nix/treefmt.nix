{
  ...
}:
{
  perSystem = {
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
