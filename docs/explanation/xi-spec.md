# The Xi Specification

This document codifies the conventions and guarantees that xi provides to its
users. These are not arbitrary — they are consistent across the entire tool and
you can rely on them.

## 1. Naming conventions

### Environment variables

All xi-specific variables use the `XI_` prefix:

```
XI_FLAKE, XI_OS_FLAKE, XI_LOG, XI_CONFIG, XI_NO_NOM, ...
```

Standard Nix variables (`NIX_SSHOPTS`, `NIX_CONFIG`, etc.) are forwarded but
never renamed. When xi provides its own variant (e.g. `XI_SSHOPTS`), it takes
precedence over the `NIX_` equivalent.

Deprecated variables (`FLAKE`, `XI_ELEVATION_PROGRAM`) are auto-migrated with a
deprecation warning. They will be removed in a future major version.

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

Categories are nouns. Actions are verbs.

### Config keys

`config.toml` uses `snake_case` for keys:

```toml
[build]
ci_backend = "devour-flake"
connect_timeout = 5
keep_going = false
```

Section names match the command category where possible (`[build]`, `[cache]`,
`[develop]`, `[fmt]`, `[test]`).

## 2. Configuration cascade

Every setting follows the same priority, always:

```
CLI flag  >  Environment variable  >  config.toml  >  Built-in default
```

No exceptions. The mapping is predictable:

| CLI flag       | Env var         | Config key         |
| -------------- | --------------- | ------------------ |
| `--show-trace` | `XI_SHOW_TRACE` | `build.show_trace` |
| `--no-nom`     | `XI_NO_NOM`     | `build.nom`        |
| `--keep-going` | `XI_KEEP_GOING` | `build.keep_going` |
| `--impure`     | `XI_IMPURE`     | `build.impure`     |
| `--offline`    | `XI_OFFLINE`    | `build.offline`    |
| `--max-jobs N` | `XI_MAX_JOBS`   | `build.max_jobs`   |

## 3. Privilege elevation

Xi never assumes `sudo` is available. The strategy is:

1. Check `XI_ELEVATION_STRATEGY` env var
2. Auto-detect: `doas` > `sudo` > `run0` > `pkexec`

The `none` strategy skips elevation entirely. The `passwordless` strategy uses
`-n` flags. Password caching works per-host for remote operations.

## 4. Installable resolution

Installables are resolved in a fixed order:

1. Store path (`/nix/store/...`) — used directly
2. File mode (`--file <path>`) — Nix file evaluation
3. Expression mode (`--expr '<expr>'`) — inline evaluation
4. Flake reference — parsed as `path#attribute.path`

For flake references, the environment variable chain is:

```
XI_OS_FLAKE > XI_HOME_FLAKE > XI_DARWIN_FLAKE > XI_SYSTEM_FLAKE > XI_FLAKE
```

Local flake references must point at a directory containing `flake.nix`. Empty
references are rejected.

## 5. Shell integration

Bash, zsh, and fish behave identically at the feature level. Shell differences
are confined to syntax:

| Aspect     | bash/zsh                        | fish                            |
| ---------- | ------------------------------- | ------------------------------- |
| Export     | `export KEY='value'`            | `set -gx KEY 'value'`           |
| Init       | `--rcfile` / `ZDOTDIR`          | `--init-command`                |
| Completion | `eval "$(xi completions bash)"` | `xi completions fish \| source` |

## 6. Trust model

Trust is per-flake-path:

- `xi develop trust` creates a marker at
  `$XDG_CONFIG_HOME/xi/develop/trusted/<flake_id>`
- The flake ID is derived from the absolute flake path
- Trust and untrust are idempotent
- Untrust while active causes immediate subshell exit
- No daemon is spawned for untrusted flakes

## 7. Logging

| Flag    | Level |
| ------- | ----- |
| (none)  | ERROR |
| `-v`    | WARN  |
| `-vv`   | INFO  |
| `-vvv`  | DEBUG |
| `-vvvv` | TRACE |

`XI_LOG` accepts filter directives for fine-grained control (e.g.
`xi=trace,dix=warn`).

## 8. Flake output standardisation

`xi flake show` recognises implicit output types by naming convention:

| Pattern                                                             | Recognised as                |
| ------------------------------------------------------------------- | ---------------------------- |
| `packages`, `devShells`, `checks`, `apps`, `formatter`              | Per-system outputs           |
| `lib`, `*Lib`, `*libs`                                              | Library outputs              |
| `*Module`, `*Modules`, `*modules`                                   | Module outputs               |
| `*Configuration`, `*Configurations`, `*Config`                      | Configuration outputs        |
| `nixosConfigurations`, `homeConfigurations`, `darwinConfigurations` | System configurations        |
| `overlays`, `templates`                                             | Standard outputs             |
| `debug`, `allSystems`                                               | Internal (hidden by default) |

Rendering rules:

- All outputs named `default` get a `[default]` flag
- Lib outputs show as `lib :: lib (N attrs)` with a hint to use `xi lib`
- Test-only trees collapse to a summary line
- Type annotations use the `:: type` syntax (e.g. `:: module`, `:: lib`)
- Hidden outputs require `--all` to display

## 9. Materialization

Materialization caches expensive evaluations:

- Runs shell commands, captures stdout, writes to cache
- Uses SHA-256 hashes of source file contents for invalidation
- Supports a git skip-worktree lifecycle for committed files
- Integrates with `xi flake ci` (Phase 1 freshness check) and `xi flake build`
  (pre-build hook)

Configuration lives in `.xi.toml` under `[materialize]` and
`[[materialize.target]]`.

## 10. Two configuration layers

| File          | Scope           | Controls                                                       |
| ------------- | --------------- | -------------------------------------------------------------- |
| `config.toml` | User / machine  | Build defaults, cache targets, daemon, formatters              |
| `.xi.toml`    | Project / flake | CI pipeline, doctor thresholds, test backends, materialization |

`config.toml` follows the user (via `$XI_CONFIG` or `$XDG_CONFIG_HOME`).
`.xi.toml` lives at the flake root and is committed to version control. Both are
optional.

## 11. Platform support

First-class targets:

- `x86_64-linux`
- `aarch64-linux`
- `aarch64-darwin`

`x86_64-darwin` is no longer supported, following Nixpkgs' decision.

## 12. Versioning

Xi uses semantic versioning. The license is EUPL-1.2.
