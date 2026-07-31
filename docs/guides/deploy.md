# How to deploy configurations

`xi deploy` deploys NixOS (and nix-darwin) configurations to remote machines. It
auto-detects which deployment tool your flake is set up for and drives it —
[deploy-rs](https://github.com/serokell/deploy-rs),
[colmena](https://github.com/zhaofengli/colmena), or xi's built-in
`nixos-rebuild`-over-SSH. You don't need to remember which tool each project
uses; `xi deploy` figures it out.

## Deploy everything

From your flake root:

```sh
xi deploy
```

Xi probes the flake and picks a backend:

1. If `deploy` output exists → **deploy-rs** (magic rollback, per-profile deploys)
2. Else if `colmenaHive` output exists → **colmena** (parallel, tag-based selection)
3. Else if `nixosConfigurations` exists → **xi built-in** (nixos-rebuild switch over SSH)

## Deploy specific machines

Pass targets positionally:

```sh
xi deploy web-01 web-02
```

Or filter by tag (colmena-style):

```sh
xi deploy --on @web
xi deploy --on @edge
```

## Preview without applying

```sh
xi deploy --dry-run
```

Builds every affected configuration but doesn't push or activate. Right when
reviewing a diff or before touching production.

## Force a specific backend

```sh
xi deploy --backend deploy-rs
xi deploy --backend colmena
xi deploy --backend builtin
```

Use this when your flake exposes multiple deployment outputs and auto-detection
picks the wrong one, or when you want the plain built-in path even though a
`deploy` output is present.

## Skip pre-deploy checks

deploy-rs runs `deployChecks` before pushing. To bypass them (e.g. a stuck
health check blocking a rollback deploy):

```sh
xi deploy --skip-checks
```

## Disable magic rollback (deploy-rs)

By default deploy-rs waits for a confirmation heartbeat after activation and
rolls back if you don't confirm. This is safe for interactive deploys but
inconvenient for unattended pipelines:

```sh
xi deploy --no-magic-rollback
```

To keep magic rollback but wait longer for confirmation:

```sh
xi deploy --confirm-timeout 120     # seconds; default is 30
```

## Pass tool-specific flags

Anything after `--` is forwarded to the underlying tool verbatim:

```sh
xi deploy -- --checksum-cache=/tmp/xi-cache      # colmena flag
xi deploy -- --auto-rollback true                # deploy-rs flag
```

## When to use `xi deploy` vs `xi os switch --target-host`

| Situation                                              | Use                                |
| ------------------------------------------------------ | ---------------------------------- |
| One-off remote rebuild of a single host                | `xi os switch --target-host host`  |
| Fleet, tag selection, parallel activation              | `xi deploy` (colmena backend)      |
| Rollback-on-failure needed                             | `xi deploy` (deploy-rs backend)    |
| Non-flake config or ad-hoc target                      | `xi os switch --target-host host`  |
| Coordinated multi-host push from CI                    | `xi deploy`                        |

`xi os switch --target-host` is one host, one shot. `xi deploy` is a fleet
operator.

## Show trace on failure

```sh
xi deploy --show-trace
```

Prints Nix evaluation tracebacks when a build or eval fails.

## See also

- [CLI Reference: `xi deploy`](../reference/cli.md#xi-deploy--deploy-configurations-to-remote-machines)
- [Remote Build](remote-build.md) — building on one host and deploying from another
- [Binary Cache](binary-cache.md) — pushing built closures to a shared cache before deploy
