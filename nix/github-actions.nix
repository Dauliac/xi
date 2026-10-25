{
  inputs,
  ...
}:
{
  imports = [
    inputs.github-actions-nix.flakeModules.default
  ];

  perSystem =
    { config, ... }:
    {
      # Expose the generated workflows derivation as a legacyPackages entry
      # so `just workflows-sync` (or any other caller) can copy the YAML
      # into .github/workflows/ without touching other outputs.
      legacyPackages = {
        github-workflows = config.githubActions.workflowsDir;
      };

      githubActions = {
        enable = true;

        # Lint docs: PR-scoped Vale gate on prose changes. Mirrors the
        # nix-oci lint-docs workflow one-for-one so docs-only PRs still
        # get a Vale pass.
        workflows.lint-docs = {
          name = "Lint docs";

          on = {
            pullRequest = {
              paths = [
                "**/*.md"
                ".vale.ini"
                ".vale/**"
                "nix/lint-vale.nix"
                ".github/workflows/lint-docs.yml"
              ];
            };
            push.branches = [ "main" ];
          };

          jobs.vale = {
            runsOn = "ubuntu-latest";
            steps = [
              {
                name = "Checkout";
                uses = "actions/checkout@v4";
              }
              {
                name = "Install Nix";
                uses = "DeterminateSystems/nix-installer-action@main";
              }
              {
                name = "Cache Nix store";
                uses = "DeterminateSystems/magic-nix-cache-action@main";
              }
              {
                name = "Lint prose (Vale)";
                run = "nix build .#checks.x86_64-linux.lint-vale --print-build-logs";
              }
            ];
          };
        };
      };
    };
}
