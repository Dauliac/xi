# The Xi Specification

This document codifies the implicit standards and conventions that xi follows.
These are not arbitrary choices — they emerged from real requirements and are
enforced across the codebase. Treat this as the contract between xi's
internals and its users.

## 1. Naming conventions

### Environment variables

All xi-specific variables use the `XI_` prefix:

```
XI_FLAKE, XI_OS_FLAKE, XI_LOG, XI_CONFIG, XI_NO_NOM, ...
```

Standard Nix variables (`NIX_SSHOPTS`, `NIX_CONFIG`, etc.) are forwarded
but never renamed. When xi provides its own variant (e.g. `XI_SSHOPTS`),
it takes precedence over the `NIX_` equivalent.

Deprecated variables (`FLAKE`, `XI_ELEVATION_PROGRAM`) are auto-migrated
with a deprecation warning. They will be removed in a future major version.

### CLI structure

Commands follow `xi <category> <action>`:

```
xi os switch
xi home build
xi search packages
xi cache status
xi flake ci
xi develop trust
```

Categories are nouns. Actions are verbs. This is consistent across all
platforms.

### Config keys

`config.toml` uses `snake_case` for keys:

```toml
[build]
ci_backend = "devour-flake"
connect_timeout = 5
keep_going = false
```

Section names match the command category where possible (`[build]`,
`[cache]`, `[develop]`, `[fmt]`, `[test]`).

### Crate names

Crates use the `xi-` prefix with the category name:

```
xi-core, xi-nixos, xi-home, xi-darwin, xi-flake, xi-develop, xi-search,
xi-diff, xi-remote
```

The standalone `nix-command` crate is the exception — it is published to
crates.io and has no xi prefix.

### Rust conventions

- `snake_case` for functions and variables
- `PascalCase` for types and enums
- `SCREAMING_SNAKE_CASE` for constants
- Rust 2024 edition

## 2. Configuration cascade

Every tuneable setting follows the same three-level priority, always:

```
CLI flag  >  Environment variable  >  config.toml  >  Built-in default
```

No exceptions. If a value can be set via CLI flag, it can also be set via
env var and config file. The mapping is predictable:

| CLI flag | Env var | Config key |
|----------|---------|------------|
| `--show-trace` | `XI_SHOW_TRACE` | `build.show_trace` |
| `--no-nom` | `XI_NO_NOM` | `build.nom` |
| `--keep-going` | `XI_KEEP_GOING` | `build.keep_going` |
| `--impure` | `XI_IMPURE` | `build.impure` |
| `--offline` | `XI_OFFLINE` | `build.offline` |
| `--max-jobs N` | `XI_MAX_JOBS` | `build.max_jobs` |

## 3. Error handling

### No panics

`panic!`, `unwrap()`, and `expect()` are banned via workspace-level Clippy
lints (`deny`). The only exception is test code, where they may be used with
explicit `#[expect]` annotations including a `reason` parameter.

### Error types

All functions return `color_eyre::Result<T>`. Errors carry context chains
via `.wrap_err()` and `.context()`. The final error report includes:

- Error message chain
- Source location
- Suggestion to file an issue

### Structured errors

Domain-specific errors use `thiserror::Error` enums:

```rust
#[derive(Error, Debug)]
pub enum MyError {
    #[error("something went wrong: {0}")]
    SomethingWentWrong(String),
}
```

### Early validation

Invalid input is rejected early with `bail!()`. Examples:

- Empty flake references
- Malformed quoted attribute paths
- Unknown config keys
- Missing `flake.nix` in specified directory
- Missing experimental features

Each validation error includes actionable guidance.

## 4. Privilege elevation

Xi never assumes `sudo` is available. The elevation strategy is:

1. Check `XI_ELEVATION_STRATEGY` env var
2. Auto-detect: `doas` > `sudo` > `run0` > `pkexec`
3. Each tool gets correct flags (e.g. `doas` does not accept `--stdin`)

The `none` strategy skips elevation entirely. The `passwordless` strategy
uses `-n` flags. Password caching works per-host for remote operations.

## 5. Installable resolution

Installables are resolved in a fixed order:

1. Store path (`/nix/store/...`) — used directly
2. File mode (`--file <path>`) — `nix-instantiate` evaluation
3. Expression mode (`--expr '<expr>'`) — inline evaluation
4. Flake reference — parsed as `path#attribute.path`

For flake references, the environment variable chain is:

```
XI_OS_FLAKE > XI_HOME_FLAKE > XI_DARWIN_FLAKE > XI_SYSTEM_FLAKE > XI_FLAKE
```

Context-specific variables take precedence. `XI_FLAKE` is the fallback.

Local flake references must point at a directory containing `flake.nix`.
Subdirectories of a flake are rejected. Empty references are rejected.

## 6. Shell integration

### Activation stub contract

Shell activation stubs (`xi develop activate <shell>`) must be:

- Under 10 lines
- Free of flake detection logic (that lives in Rust)
- Responsible only for: sourcing user config, sourcing env/hook files,
  installing EXIT trap, calling `xi develop prompt`

### Shell parity

Bash, zsh, and fish must behave identically at the feature level. Shell
differences are confined to syntax:

| Aspect | bash/zsh | fish |
|--------|----------|------|
| Export | `export KEY='value'` | `set -gx KEY 'value'` |
| Init | `--rcfile` / `ZDOTDIR` | `--init-command` |
| Completion | `eval "$(xi completions bash)"` | `xi completions fish \| source` |

### Environment file conventions

- Env files contain only exports, no hook code
- Hook files are separate from env files
- Certain variables are never overwritten: `HOME`, `USER`, `SHELL`
- A cleanup preamble unsets previously injected variables

## 7. Daemon protocol

### Wire format

```
[4 bytes: LE uint32 length][N bytes: UTF-8 JSON]
```

Maximum message size: 16 MB.

### Tagged unions

All messages use a `"type"` field for variant discrimination:

```json
{"type": "Prompt", "consumer_pid": 12345, ...}
{"type": "Status"}
{"type": "Shutdown"}
```

### Timeouts

| Operation | Value |
|-----------|-------|
| Connect | 500 ms |
| Read | 5 s |
| Write | 5 s |

### State machine

```
Starting → Evaluating → Ready
                      → BuildFailed (with backoff: 30s, 60s, 120s, 240s, 300s cap)
         → WatcherDegraded (file watcher failed, serve cached)
         → ConfigError
         → ShuttingDown
```

File changes reset the backoff timer immediately.

## 8. Generation counters

The daemon tracks two independent generation counters:

- **env_gen**: bumped when environment variables change
- **hook_gen**: bumped when shellHook changes

Each shell consumer tracks its own `last_env_gen` and `last_hook_gen`.
The `should_source_env` / `should_source_hook` flags are set when the
daemon's generation exceeds the consumer's.

Content-hash deduplication prevents spurious bumps: if the evaluation
produces identical output, the generation counter is not incremented.

## 9. A/B slot switching

Environment files use atomic A/B slot switching:

- Two files exist: `env-A` and `env-B`
- The active slot is tracked in metadata
- On update, the inactive slot is written, then the active pointer is
  swapped
- This prevents partial reads during updates

## 10. Trust model

Trust is per-flake-path with deterministic IDs:

- `xi develop trust` creates a marker at
  `$XDG_CONFIG_HOME/xi/develop/trusted/<flake_id>`
- The flake ID is derived from the absolute flake path
- Different paths to the same flake produce different IDs
- Trust and untrust are idempotent
- Untrust while active causes immediate subshell exit

## 11. Notification bus

The daemon maintains a notification bus:

- Global notifications are broadcast to all consumers
- Per-consumer cursors prevent duplicate delivery
- New consumers skip historical notifications
- Error deduplication: the same error message is not repeated, but a
  different error replaces the old one
- Notification kinds: Loading, Success, Info, Warn, Error

## 12. Logging

Xi uses the `tracing` crate with these levels:

| Flag | Level |
|------|-------|
| (none) | ERROR |
| `-v` | WARN |
| `-vv` | INFO |
| `-vvv` | DEBUG |
| `-vvvv` | TRACE |

`XI_LOG` accepts `tracing_subscriber` filter directives for fine-grained
control (e.g. `xi=trace,dix=warn`).

## 13. Lint standards

The workspace enforces strict lints via `Cargo.toml`:

- **Deny**: `panic`, `unwrap_used`
- **Warn**: `expect_used`, `todo`, `unimplemented`, `unreachable`
- **Clippy groups** (warn, priority -1): `complexity`, `nursery`, `pedantic`,
  `perf`, `style`

Suppressions require `#[expect]` (not `#[allow]`) with a `reason` parameter.
This ensures stale suppressions are caught when the lint no longer fires.

## 14. Flake output standardisation

`xi flake show` does not display raw Nix output. It applies a recognition
layer that classifies outputs by naming convention:

| Pattern | Recognised as |
|---------|--------------|
| `packages`, `devShells`, `checks`, `apps`, `formatter` | Per-system outputs |
| `lib`, `*Lib`, `*libs` | Library outputs |
| `*Module`, `*Modules`, `*modules` | Module outputs |
| `*Configuration`, `*Configurations`, `*Config` | Configuration outputs |
| `nixosConfigurations`, `homeConfigurations`, `darwinConfigurations` | System configurations |
| `overlays`, `templates` | Standard outputs |
| `debug`, `allSystems` | Internal (hidden by default) |

Rendering rules:

- All outputs named `default` get a `[default]` flag
- Lib outputs show as `lib :: lib (N attrs)` with a hint to use `xi lib`
- Test-only trees (`{expected, expr}` leaves) collapse to a summary line
- Type annotations use the `:: type` syntax (e.g. `:: module`, `:: lib`)
- Hidden outputs require `--all` to display

Display order follows a fixed category priority: packages first, then
devShells, checks, apps, formatter, overlays, modules, configurations,
templates, lib, legacyPackages.

## 15. Materialization

Materialization is a caching system for expensive evaluations. It:

- Runs shell commands, captures stdout, writes to cache
- Uses SHA-256 hashes of source file contents for invalidation
- Supports a git skip-worktree lifecycle for committed files
- Integrates with `xi flake ci` (Phase 1 freshness check) and
  `xi flake build` (pre-build hook)

Configuration lives in `.xi.toml` under `[materialize]` and
`[[materialize.target]]`. Each target declares: name, command, output path,
and source globs.

The `--commit` flag writes to a configurable commit path (default
`nix/materialized/`) with automatic git add and skip-worktree re-application.

## 16. Two configuration layers

Xi maintains two separate configuration files:

| File | Scope | Controls |
|------|-------|----------|
| `config.toml` | User / machine | Build defaults, cache targets, daemon, formatters |
| `.xi.toml` | Project / flake | CI pipeline, doctor thresholds, test backends, materialization |

`config.toml` follows the user (via `$XI_CONFIG` or `$XDG_CONFIG_HOME`).
`.xi.toml` lives at the flake root and is committed to version control.

Both are optional. Missing files use built-in defaults silently.

## 17. Daemon as unified async runtime

The develop daemon is not limited to devshell management. It also:

- Handles `CachePush` requests by spawning background push threads
- Periodically drains the persistent cache push queue (retrying failed
  pushes at the configured drain interval)

This makes the daemon the single long-lived process for all async
operations, avoiding separate background processes for cache pushing.

## 18. Platform support

First-class targets:

- `x86_64-linux`
- `aarch64-linux`
- `aarch64-darwin`

`x86_64-darwin` is no longer supported, following Nixpkgs' decision.

## 19. Versioning

Xi uses semantic versioning. The workspace version in `Cargo.toml` is the
single source of truth. The license is EUPL-1.2.
