# How to Use Xi as a Nix Proxy

Xi can intercept and enhance standard `nix` commands transparently. When you run
`xi nix build`, xi adds nom output, enhanced error messages, and xi's
configuration cascade.

## Set up the alias

### Shell alias

```bash
alias nix="xi nix"
```

### System-wide via NixOS module

```nix
{
  programs.xi = {
    enable = true;
    nix.wrapAlias = true;
  };
}
```

This replaces the system `nix` binary with an xi-wrapped version. All other nix
binaries (`nix-build`, `nix-env`, `nix-daemon`) are preserved unchanged.

### Via shell hooks

```nix
{
  programs.xi.shellHook = {
    enable = true;
    nixAlias = true;  # adds the alias in interactive shells
  };
}
```

## Enhanced commands

The proxy intercepts and enhances these commands:

| Command            | Enhancement                                       |
| ------------------ | ------------------------------------------------- |
| `nix build`        | Nom output, xi config cascade                     |
| `nix develop`      | Daemon-driven devshell instead of raw nix develop |
| `nix fmt`          | Config-driven formatter backends                  |
| `nix run`          | Nom output                                        |
| `nix flake check`  | Nom output                                        |
| `nix flake init`   | Passthrough                                       |
| `nix flake update` | Passthrough                                       |
| `nix flake show`   | Enhanced output display                           |

All other commands (e.g. `nix store`, `nix path-info`, `nix repl`) are passed
through to the real nix binary unchanged.

## Bypass the proxy

If you need the raw nix behaviour for a single command:

```sh
XI_UNWRAP=1 nix build .#hello
```

Or use the `--unwrap` flag:

```sh
xi nix --unwrap build .#hello
```

## See also

- [Explanation: Nix proxy](../explanation/architecture.md#nix-proxy) — why
  `XI_NIX_BIN` exists and how the routing works
- [CLI Reference: `xi nix`](../reference/cli.md#xi-nix--transparent-nix-proxy)
