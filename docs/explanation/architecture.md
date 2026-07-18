# Architecture and Design

## Philosophy

Xi is a unified CLI that reimplements Nix ecosystem tools from scratch in
Rust. It does not wrap `nixos-rebuild` or `home-manager` — it calls `nix`
directly and handles evaluation, building, diffing, and activation itself.

The design serves three goals:

1. **Cohesion** — one tool for NixOS, Home Manager, Darwin, search, clean,
   develop, and CI.
2. **Safety** — no panics, strict error handling, configuration validation,
   privilege elevation done correctly.
3. **Polish** — pretty output (nom), fast diffs (dix), shell integration that
   feels invisible.

## Crate structure

Xi is a Cargo workspace with each concern isolated into its own crate:

```
crates/
├── xi/              Main binary: CLI parsing, logging, config, subcommand dispatch
├── xi-core/         Shared foundation: command execution, installable resolution,
│                    checks, cache, completions, styling, progress
├── xi-nixos/        NixOS: switch, boot, test, build, rollback, generations, build-vm
├── xi-home/         Home Manager: switch, build
├── xi-darwin/       nix-darwin: switch, build
├── xi-flake/        Flake operations: build, check, run, fmt, show, ci, test,
│                    doctor, materialize, project config
├── xi-develop/      Devshell daemon: enter, trust, watcher, protocol, shell
│                    registry, notifications, env files, prompt hooks
├── xi-search/       Search: packages, options, offline (SPAM), PRs, issues,
│                    GitHub API, rendering
├── xi-diff/         Package diffing via dix
├── xi-remote/       Remote build/deploy: SSH, closure copy, remote diff
└── nix-command/     Typed Nix CLI wrapper with schema-driven flag generation
```

### Dependency flow

```
xi (binary)
├── xi-core
├── xi-nixos   → xi-core, xi-diff, xi-remote
├── xi-home    → xi-core, xi-diff, xi-remote
├── xi-darwin  → xi-core, xi-diff, xi-remote
├── xi-flake   → xi-core
├── xi-develop → xi-core
├── xi-search  → xi-core
├── xi-diff    → xi-core
├── xi-remote  → xi-core
└── nix-command (standalone, published to crates.io)
```

`xi-core` is the only shared dependency. Crates never depend on siblings
except through `xi-core`.

## Key architectural patterns

### Command builder

All subprocess execution goes through `xi-core`'s `Command` type:

```rust
Command::new("nix")
    .arg("build")
    .passthrough(&build_args)
    .elevate(strategy)
    .nom(use_nom)
    .dry(is_dry)
    .run()?;
```

This builder handles:
- Argument construction with proper quoting
- Privilege elevation (auto-detecting sudo/doas/run0/pkexec)
- Nom integration (piping through nix-output-monitor)
- Dry-run mode
- Streaming output to terminal
- Password caching for remote SSH

### Installable resolution

Xi supports four installable modes, resolved in order:

1. **Store path** — `/nix/store/...` used directly
2. **File mode** — `--file <path>` with optional attribute path
3. **Expression mode** — `--expr '<expr>'` with optional attribute path
4. **Flake reference** — `path#attribute` with environment variable fallback

The resolution chain for flake references checks context-specific variables
first (`XI_OS_FLAKE`, `XI_HOME_FLAKE`, etc.) before falling back to
`XI_FLAKE`.

### Configuration cascade

Every tuneable setting follows the same three-level priority:

```
CLI flag  >  Environment variable  >  config.toml  >  Built-in default
```

This applies to `--show-trace`/`XI_SHOW_TRACE`/`build.show_trace`, to
`--no-nom`/`XI_NO_NOM`/`build.nom`, and to every other option.

### Feature requirements

Before running a command, xi validates that the Nix installation has the
required experimental features:

- Flake commands need `nix-command` and `flakes`
- Lix below 2.93.0 also needs `repl-flake`

This check is skippable via `XI_NO_CHECKS=1`.

### Error handling

Xi uses `color_eyre::Result` throughout. Panics (`panic!`, `unwrap()`,
`expect()`) are banned via workspace-level Clippy lints. Errors include
context chains, location info, and a link to the issue tracker.

### Styling system

Icons and colours are standardised across all output:

| Icon | Colour | Meaning |
|------|--------|---------|
| `✓` | Green | Success |
| `✗` | Red | Error |
| `⟳` | Blue | Loading |
| `▲` | Yellow | Warning |
| `●` | White | Info |
| `+` | Green | Added (diff) |
| `-` | Red | Removed (diff) |
| `~` | Yellow | Changed (diff) |

## Module system

The Nix modules (NixOS, Home Manager, flake-parts) share types and library
functions from `modules/shared/`:

```
modules/
├── shared/
│   ├── types/    Type definitions: wrapper, settings, tool, devshell, shellHook
│   └── lib/      Functions: mkWrappedPackage, mkConfigFile, mkComposedShellHook
├── flake-parts/  xi.* options
├── nixos/        programs.xi.* options
└── home-manager/ programs.xi.* options
```

The wrapper system:
1. Generates `config.toml` from settings
2. Creates a bash script exporting `XI_CONFIG`
3. Collects tool packages into PATH
4. Combines via `symlinkJoin` preserving completions and man pages

## Testing

- **Unit tests** in each crate with `#[cfg(test)]`
- **BDD feature specs** in `tests/features/` (20 Gherkin files as source of
  truth for develop daemon behaviour)
- **BDD step implementations** in `crates/xi-develop/tests/` (12 test files)
- **Module integration tests** in `tests/flake-module/`
- **CI checks**: `cargo clippy --deny warnings`, `cargo doc -D warnings`,
  `cargo nextest run`
