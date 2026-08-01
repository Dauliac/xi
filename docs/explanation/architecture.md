# Architecture and Design

## Philosophy

Xi is a unified CLI that reimplements Nix ecosystem tools from scratch in Rust.
It does not wrap `nixos-rebuild` or `home-manager` — it calls `nix` directly and
handles evaluation, building, diffing, and activation itself.

The design serves three goals:

1. **Cohesion** — one tool for NixOS, Home Manager, Darwin, search, clean,
   develop, and CI.
2. **Safety** — strict error handling, configuration validation, privilege
   elevation done correctly.
3. **Polish** — pretty output (nom), fast diffs (dix), shell integration that
   feels invisible.

## How xi runs commands

Xi does not shell out to `nixos-rebuild` or `home-manager`. It builds `nix`
commands directly and handles:

- Privilege elevation (auto-detecting sudo/doas/run0/pkexec)
- Piping through nix-output-monitor for pretty output
- Dry-run mode
- Streaming output to the terminal
- Password caching for remote SSH operations

## Installable resolution

When you pass a target to xi, it resolves in this order:

1. `/nix/store/...` — store path, used directly
2. `--file <path>` — classical Nix file evaluation
3. `--expr '<expr>'` — inline Nix expression
4. Everything else — flake reference (`path#attribute`)

For flake references, context-specific environment variables take priority:

```
XI_OS_FLAKE > XI_HOME_FLAKE > XI_DARWIN_FLAKE > XI_SYSTEM_FLAKE > XI_FLAKE
```

Local flake references must point at a directory containing `flake.nix`.

## Configuration cascade

Every setting follows the same priority, always:

```
CLI flag  >  Environment variable  >  config.toml  >  Built-in default
```

This applies uniformly to `--show-trace`, `--no-nom`, `--keep-going`, and every
other option.

## Two configuration layers

| File          | Scope           | Controls                                                       |
| ------------- | --------------- | -------------------------------------------------------------- |
| `config.toml` | User / machine  | Build defaults, cache targets, daemon, formatters              |
| `.xi.toml`    | Project / flake | CI pipeline, doctor thresholds, test backends, materialization |

`config.toml` follows the user (via `$XI_CONFIG` or `$XDG_CONFIG_HOME`).
`.xi.toml` lives at the flake root and is committed to version control. Both are
optional.

## Privilege elevation

Xi never assumes `sudo` is available. The auto-detection order is:

1. `XI_ELEVATION_STRATEGY` environment variable (if set)
2. `doas` > `sudo` > `run0` > `pkexec` (first found)

The `none` strategy skips elevation. The `passwordless` strategy uses `-n`
flags. Password caching works per-host for remote operations.

## Module system

The NixOS, Home Manager, and flake-parts modules share a common wrapper system
that:

1. Generates `config.toml` from your `settings` options
2. Creates a bash wrapper that exports `XI_CONFIG=/nix/store/...`
3. Collects enabled tool packages (nom, formatters, test frameworks) into `PATH`
4. Combines everything with `symlinkJoin`, preserving completions and man pages

The wrapper is transparent: `xi --version` still works, completions are
preserved, and the configuration is baked into the derivation. Turning
`wrapper.enable` off falls back to the plain xi binary.

## Nix proxy

`xi nix` intercepts a small allow-list of subcommands (`build`, `develop`,
`fmt`, `run`, `flake check/init/update/show`) and routes them into xi's own
implementations. Everything else is passed through to the real `nix` binary,
resolved from `XI_NIX_BIN` (set by the wrapper) or `$PATH`.

The `XI_NIX_BIN` indirection matters because when xi is aliased to `nix` via the
wrapper, calling `nix` recursively would loop. `XI_NIX_BIN` gives xi a stable
pointer to the underlying binary. Setting `XI_UNWRAP=1` or passing `--unwrap`
bypasses xi entirely for one call.

## Platform support

First-class targets:

- `x86_64-linux`
- `aarch64-linux`
- `aarch64-darwin`
