xi on  feat/xi-sources-switch-entrypoint [$✘!+?⇡] is 📦 v4.4.0 via 🦕 via 🦀
v1.97.1 on ☁️ (eu-west-3) [55m48s] 23:40:38 ❯ ./target/debug/xi xi 4.4.0 Yet
Another Nix Helper

Usage: xi [OPTIONS] <COMMAND>

Commands: [Config mgmt]: os `NixOS` functionality home Home-manager
functionality darwin Nix-darwin functionality system system-manager
functionality

[Flake]: build Build a flake output (defaults to current directory) check Check
a flake for errors (defaults to current directory) run Run a flake app, or a
bare command from nixpkgs (comma-style) fmt Format source code using the flake's
formatter (defaults to current directory) show Show the outputs of a flake
(defaults to current directory) init Initialize a flake in the current directory
from a template update Update flake inputs (defaults to current directory) ci
Run CI pipeline: validate, then build all flake outputs lib Inspect or evaluate
flake lib outputs test Run Nix code tests across multiple backends doctor Check
flake health: input freshness, nixpkgs branch, source, formatter materialize
Materialize cached outputs from .xi.toml targets

[Deployment]: deploy Deploy configurations to remote machines push Push the
resolved sources configuration to remote machines develop Enter or manage
development shells search Searches packages or NixOS/home-manager options via
search.nixos.org, or a local SPAM database

[Agentic]: agent Structured project context for AI coding agents auth Manage Nix
`access-tokens` via `nix-auth` (OAuth device flow)

[Other]: cache Manage the binary cache push queue clean Enhanced nix cleanup nix
Raw arguments captured from `xi nix <args...>`. Nix CLI proxy — enhanced UX for
known commands, passthrough for the rest completions Generate shell completions
for xi

Options: -v, --verbose... Increase logging verbosity

-q, --quiet... Decrease logging verbosity

-e, --elevation-strategy <ELEVATION_STRATEGY> Choose the privilege elevation
strategy.

          Can be a path to an elevation program (e.g., /usr/bin/sudo), or one of: 'none' (no elevation), 'passwordless' (use elevation without password prompt for remote hosts with NOPASSWD configured), or 'auto' (automatically detect available elevation programs in order: doas, sudo, run0, pkexec)

          [env: XI_ELEVATION_STRATEGY=pkexec]

-h, --help Print help (see a summary with '-h')

-V, --version Print version
