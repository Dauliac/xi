# Setting Up xi develop

This tutorial walks you through setting up daemon-driven development shells with
xi. By the end you will have a project where entering the directory
automatically activates a devshell with live reload on flake changes.

## Prerequisites

- xi installed (see [Getting Started](./getting-started.md))
- A flake-based project with a `devShells` output
- Shell hooks enabled (bash or zsh)

## Create a minimal flake

If you do not already have a flake with a devshell, create one:

```sh
mkdir my-project && cd my-project
```

Create `flake.nix`:

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.${system}.default = pkgs.mkShellNoCC {
        packages = [ pkgs.hello pkgs.jq ];
        shellHook = ''
          echo "Welcome to my-project"
        '';
      };
    };
}
```

## Enter the devshell manually

First, try entering the devshell directly:

```sh
xi develop
```

Xi evaluates the flake, builds the devshell with nom, writes environment and
hook files, and drops you into a subshell with `hello` and `jq` available.

Verify:

```sh
which hello
which jq
```

Exit the devshell with `exit` or `Ctrl+D`.

## Trust the flake for auto-activation

Xi requires explicit trust before auto-activating devshells. Trust this project:

```sh
xi develop trust
```

This creates a marker file at `$XDG_CONFIG_HOME/xi/develop/trusted/<flake_id>`.
The ID is deterministic and derived from the flake path.

## Install the shell hook

For auto-activation to work, your shell needs the xi develop prompt hook. Add it
to your shell configuration.

**bash** (`~/.bashrc`):

```bash
eval "$(xi develop activate bash)"
```

**zsh** (`~/.zshrc`):

```zsh
eval "$(xi develop activate zsh)"
```

**fish** (`~/.config/fish/config.fish`):

```fish
xi develop activate fish | source
```

Or, if you use the NixOS/Home Manager module, set
`programs.xi.shellHook.develop = true` and it handles this for you (see
[Module Setup](../guides/module-setup.md)).

Start a new shell (or source your config) for the hook to take effect.

## Experience auto-activation

Now `cd` into your project directory:

```sh
cd ~/my-project
```

The prompt hook detects `flake.nix`, checks the trust database, and spawns a
devshell subshell. You will see a notification:

```
[xi] Loading my-project devshell...
[xi] Activated (hello, jq)
```

You are now in the devshell. The daemon started in the background and is
watching for file changes.

## Live reload in action

Edit `flake.nix` to add a package:

```nix
packages = [ pkgs.hello pkgs.jq pkgs.ripgrep ];
```

Within a few seconds (controlled by `eval_interval`, default 5s), the daemon
detects the change, re-evaluates, and updates your environment. You will see:

```
[xi] Updating devshell...
[xi] Added: ripgrep
```

On your next prompt, the new environment is sourced. Verify:

```sh
which rg
```

No manual reload needed.

## Leave the devshell

There are three ways to leave:

1. **`exit` or `Ctrl+D`**: Exits the subshell, returns to your parent shell
2. **`cd ..`** (out of the flake directory): Graceful exit, parent shell resumes
3. **`Ctrl+C`**: Interrupts the current command but does not exit the subshell

When you leave, the daemon stays running for 60 seconds in case you come back.
After the grace period with no consumers, it shuts down automatically.

## Nested flakes

If your project has nested flakes (e.g. a monorepo), xi handles them:

```
my-monorepo/
├── flake.nix          # outer devshell
└── services/
    └── api/
        └── flake.nix  # inner devshell
```

When you `cd services/api/`, xi spawns a nested subshell. The inner devshell's
PATH is prepended to the outer one. When you leave the inner directory, you
return to the outer devshell.

## Untrust a flake

To stop auto-activation for a project:

```sh
xi develop untrust
```

If you are currently in the devshell, it exits immediately.

## Configure the daemon

You can tune daemon behaviour in `config.toml`:

```toml
[develop]
eval_interval = 5            # seconds between re-evaluations

[develop]
watch_extra = ["*.yaml"]     # extra file patterns to watch
```

See [Configuration Reference](../reference/configuration.md) for all options.

## Next steps

- Read [The Develop Model](../explanation/develop-model.md) to understand the
  daemon architecture
- See the [CLI Reference](../reference/cli.md) for all develop subcommands
- Set up auto-activation system-wide via
  [Module Setup](../guides/module-setup.md)
