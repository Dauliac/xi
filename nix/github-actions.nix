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
        # Fallback flake.lock refresher. Opens a single PR with the full
        # updated `flake.lock` once a week; NOT auto-merged. Mirrors
        # nix-oci's update-flake-lock workflow.
        workflows.update-flake-lock = {
          name = "Update flake.lock";

          on = {
            schedule = [ { cron = "0 3 * * 0"; } ];
            workflowDispatch = { };
          };

          permissions = {
            contents = "write";
            "pull-requests" = "write";
          };

          jobs.update = {
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
                name = "Update flake.lock";
                uses = "DeterminateSystems/update-flake-lock@main";
                with_ = {
                  pr-title = "chore(deps): update flake.lock";
                  pr-labels = "dependencies,nix-flake";
                  pr-body = ''
                    Automated `nix flake update` sweep. Merge only after
                    reviewing per-input changes.
                  '';
                };
              }
            ];
          };
        };

        # Deploy docs: build the mdBook site via Nix and upload to Pages.
        # Vale is a hard gate so red prose never ships.
        workflows.deploy-docs = {
          name = "Deploy Documentation";

          on = {
            push.branches = [ "main" ];
            workflowDispatch = { };
          };

          permissions = {
            contents = "read";
            pages = "write";
            id-token = "write";
          };

          concurrency = {
            group = "pages";
            cancelInProgress = false;
          };

          jobs.build = {
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
              {
                name = "Build documentation";
                run = "nix build .#legacyPackages.x86_64-linux.docs";
              }
              {
                name = "Upload artifact";
                uses = "actions/upload-pages-artifact@v3";
                with_ = {
                  path = "result";
                };
              }
            ];
          };

          jobs.deploy = {
            runsOn = "ubuntu-latest";
            needs = [ "build" ];
            environment = {
              name = "github-pages";
              url = "\${{ steps.deployment.outputs.page_url }}";
            };
            steps = [
              {
                id = "deployment";
                name = "Deploy to GitHub Pages";
                uses = "actions/deploy-pages@v4";
              }
            ];
          };
        };

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
