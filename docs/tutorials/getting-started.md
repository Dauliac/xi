# Getting Started with Xi

This tutorial takes you from zero to your first NixOS system switch with xi. By
the end you will have xi installed, understand the basic command structure, and
have switched your running NixOS system using xi instead of `nixos-rebuild`.

## Prerequisites

- A running NixOS system with flakes enabled
- A flake-based NixOS configuration (a directory containing `flake.nix` with
  `nixosConfigurations.<hostname>`)

## Install xi

Run xi directly from its flake without installing anything permanently:

```sh
nix shell github:Dauliac/xi
```

Verify it works:

```sh
xi --version
```

You should see the current xi version printed.

## Explore the CLI

Xi organises commands by platform. Run `xi --help` to see the top-level
structure:

```sh
xi --help
```

Notice the main subcommands: `os`, `home`, `darwin`, `search`, `clean`. Each has
its own `--help` page.

## Set your flake path

Xi needs to know where your NixOS configuration lives. Set the `XI_FLAKE`
variable so you do not need to pass the path every time:

```sh
export XI_FLAKE="$HOME/nixos-config"
```

You can make this permanent later by setting `programs.xi.flake` in your NixOS
module (see [Module Setup](../guides/module-setup.md)).

## Switch your system

With `XI_FLAKE` set, switch your NixOS system:

```sh
xi os switch
```

Xi will:

1. Evaluate your NixOS configuration
2. Build derivations, showing progress through **nix-output-monitor** (nom)
3. Display a diff of changed packages via **dix**
4. Ask for confirmation (press `y`)
5. Activate the new system

You have just completed your first xi switch.

## See what changed

Xi shows a package diff automatically. If you want to see it again without
switching, build first and compare:

```sh
xi os build
```

## Search for a package

Try the built-in search — it queries search.nixos.org directly:

```sh
xi search hello
```

Results appear instantly with package names, versions, and descriptions.

## Clean up old generations

Remove old system generations and reclaim disk space:

```sh
xi clean all --keep 3 --keep-since 7d
```

This keeps the 3 most recent generations and anything less than 7 days old, then
runs garbage collection.

## Next steps

- Read the [NixOS guide](../guides/nixos.md) for advanced switch/boot/test
  workflows
- Set up xi permanently with the [Module Setup](../guides/module-setup.md) guide
- Explore [xi develop](./develop-workflow.md) for daemon-driven devshells
- See the full [CLI Reference](../reference/cli.md) for every flag
