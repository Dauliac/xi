{
  description = "Documentation-only inputs for xi (not inherited by consumers)";

  inputs = {
    github-actions-nix = {
      url = "github:synapdeck/github-actions-nix";
    };
    flake-parts-website = {
      url = "github:Dauliac/flake.parts-website";
    };
  };

  outputs = _: { };
}
