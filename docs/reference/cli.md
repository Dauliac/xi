# CLI Reference

## Global options

```
xi [OPTIONS] <COMMAND>
```

| Option | Description |
|--------|-------------|
| `-v, --verbose` | Increase log verbosity (repeat for more: `-vv`, `-vvv`, `-vvvv`) |
| `-e, --elevation-strategy <STRATEGY>` | Privilege elevation: `auto`, `none`, `passwordless`, `program:<path>` |

## Commands

### xi os — NixOS system management

```
xi os <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand | Description |
|------------|-------------|
| `switch` | Build and activate configuration, set as boot default |
| `boot` | Build and set as boot default without activation |
| `test` | Build and activate without setting boot default |
| `build` | Build only, leave result in `./result` |
| `repl` | Load configuration in interactive REPL |
| `info` | List system generations |
| `rollback` | Revert to previous generation |
| `build-vm` | Build a VM activation script |
| `build-image` | Build a disk image |

#### xi os switch / boot / test options

| Option | Description |
|--------|-------------|
| `-H, --hostname <NAME>` | NixOS configuration hostname |
| `-n, --dry` | Dry run |
| `-a, --ask` | Confirm before activation |
| `--diff <MODE>` | Diff display: `auto`, `always`, `never` |
| `--no-nom` | Disable nix-output-monitor |
| `--show-activation-logs` | Show activation output |
| `--install-bootloader` | Force bootloader installation |
| `--build-host <HOST>` | Build on remote host |
| `--target-host <HOST>` | Deploy to remote host |
| `--update` | Update all flake inputs before building |
| `--update-input <NAME>` | Update specific flake input (repeatable) |
| `--no-validate` | Skip pre-activation validation |
| `--use-substitutes` | Use substitutes during remote copy |

#### xi os info options

| Option | Description |
|--------|-------------|
| `--fields <FIELDS>` | Columns to display |

#### xi os rollback options

| Option | Description |
|--------|-------------|
| `--to <N>` | Rollback to specific generation number |

#### xi os build-vm options

| Option | Description |
|--------|-------------|
| `-B, --with-bootloader` | Include bootloader in VM |
| `--run` | Launch VM after build |

#### xi os build-image options

| Option | Description |
|--------|-------------|
| `--image-variant <VARIANT>` | Image variant from `config.system.build.images` |

### xi home — Home Manager

```
xi home <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand | Description |
|------------|-------------|
| `switch` | Build and activate |
| `build` | Build only |
| `repl` | Load in REPL |

| Option | Description |
|--------|-------------|
| `-c, --configuration <NAME>` | Configuration name |

### xi darwin — nix-darwin

```
xi darwin <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand | Description |
|------------|-------------|
| `switch` | Build and activate |
| `build` | Build only |
| `repl` | Load in REPL |

| Option | Description |
|--------|-------------|
| `-H, --hostname <NAME>` | Darwin configuration hostname |

### xi system — system-manager

```
xi system <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand | Description |
|------------|-------------|
| `switch` | Build and activate |
| `build` | Build only |

### xi search — package and option search

```
xi search [MODE] [OPTIONS] <QUERY>
```

| Mode | Description |
|------|-------------|
| `packages` | Search Nixpkgs packages (default) |
| `options` | Search NixOS/Home Manager options |
| `offline` | Search local SPAM databases |
| `prs` | Search Nixpkgs pull requests |
| `issues` | Search Nixpkgs issues |

| Option | Modes | Description |
|--------|-------|-------------|
| `--json` | All | JSON output |
| `--limit <N>` | packages, options, prs, issues | Result limit (default: 30) |
| `--channel <CH>` | packages, options | Nixpkgs channel |
| `--platforms` | packages | Show supported platforms |
| `--scope <SCOPE>` | options | `nixpkgs`, `home-manager`, `all` |
| `--db <PATH>` | offline | SPAM database path (repeatable) |
| `--days <N>` | prs, issues | Time window (default: 15) |
| `--default-search <MODE>` | (global) | Default when no mode specified |

### xi clean — garbage collection

```
xi clean <TARGET> [OPTIONS]
```

| Target | Description |
|--------|-------------|
| `all` | All profiles (system + user) |
| `user` | Current user only |
| `profile <PATH>` | Specific profile path |

| Option | Description |
|--------|-------------|
| `-k, --keep <N>` | Minimum generations to keep (default: 1) |
| `-K, --keep-since <DURATION>` | Keep entries within duration |
| `-n, --dry` | Dry run |
| `-a, --ask` | Confirm |
| `--no-gc` | Skip `nix store gc` |
| `--no-gcroots` | Skip GC root cleanup |
| `--no-direnv` | Preserve direnv GC roots |
| `--optimise` | Run `nix-store --optimise` |
| `--max <BYTES>` | Limit to `nix store gc --max` |
| `--keep-one` | Keep one active direnv GC root |
| `-x, --cross-filesystems` | Cross filesystem boundaries |

### xi cache — binary cache management

```
xi cache <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `status` | Show pending pushes |
| `retry` | Flush the push queue |
| `clear` | Clear all pending pushes |

#### xi cache retry options

| Option | Description |
|--------|-------------|
| `--clear-on-failure` | Clear entries that fail |
| `--max-age-days <N>` | Only retry entries newer than N days |
| `--max-size <N>` | Limit queue size |

### xi flake — flake operations

```
xi flake <SUBCOMMAND> [OPTIONS]
```

| Subcommand | Description |
|------------|-------------|
| `build` | Build flake outputs |
| `check` | Run flake checks with nom |
| `run` | Run flake apps |
| `fmt` | Format with configurable backend |
| `show` | Display flake outputs |
| `develop` | Enter devshell (daemon-driven) |
| `init` | Create new flake from template |
| `update` | Update flake inputs |
| `ci` | Build all outputs (CI mode) |
| `test` | Run test backends |
| `doctor` | Diagnose flake issues |
| `materialize` | Cache eval outputs |
| `lib` | List/evaluate lib outputs |

#### xi flake build options

| Option | Description |
|--------|-------------|
| `--all` | Build all outputs via devour-flake |
| `--recursive` | Build subflakes (implies `--all`) |
| `--backend <BACKEND>` | `devour-flake`, `nix-fast-build`, `auto` |
| `--no-link` | Don't create result symlink |
| `-o, --out-link <PATH>` | Result symlink location |
| `--no-nom` | Disable nix-output-monitor |

#### xi flake ci options

| Option | Description |
|--------|-------------|
| `--backend <BACKEND>` | `devour-flake`, `nix-fast-build`, `auto` |
| `--no-lock-check` | Skip flake.lock sync verification |
| `--no-eval` | Skip all-systems evaluation |
| `--no-build` | Skip building (validation-only) |
| `--no-ifd` | Disallow import-from-derivation |
| `--current-system-only` | Only current system |
| `--recursive` | Validate/build subflakes |
| `--no-health-check` | Skip doctor checks |
| `--no-test` | Skip runTests eval |
| `--no-lib-eval` | Skip lib deepSeq |
| `--continue-on-error` | Don't stop on first failure |
| `--dry-run, -n` | Print without executing |
| `--no-nom` | Disable nix-output-monitor |

#### xi flake fmt options

| Option | Description |
|--------|-------------|
| `--backend <BACKEND>` | `flake`, `nixfmt`, `alejandra`, `treefmt`, `auto` |

#### xi flake show options

| Option | Description |
|--------|-------------|
| `--json, -j` | JSON output |
| `--raw` | Use raw `nix flake show` output |
| `--all, -a` | Show internal outputs (`debug`, `allSystems`) |
| `--show-trace` | Display error tracebacks |

#### xi flake test options

| Option | Description |
|--------|-------------|
| `--backend <BACKEND>...` | Run only specific backends (repeatable) |
| `--filter, -f <PATTERN>` | Glob filter on test names |
| `--list, -l` | List detected tests |
| `--format <FORMAT>` | `pretty` (default), `json` |
| `--review` | Interactive snapshot review (namaka) |
| `--watch, -w` | Re-run on file changes |
| `--no-nom` | Disable nix-output-monitor |

#### xi flake materialize options

| Option | Description |
|--------|-------------|
| `--commit` | Write to `nix/materialized/` with git lifecycle |
| `--check` | Verify freshness (exit 1 if stale) |
| `--list` | Show targets and staleness |
| `--force` | Ignore cache, re-run all |
| `--setup` | Apply git skip-worktree + .gitattributes |
| `--clean` | Remove cache directory |

#### xi flake doctor options

No flags. Thresholds configured in `.xi.toml` `[doctor]` section.

#### xi flake lib options

| Option | Description |
|--------|-------------|
| `--eval, -e` | Deep-evaluate lib with `builtins.deepSeq` |
| `--show-trace` | Display error tracebacks |

### xi develop — devshell management

```
xi develop [SUBCOMMAND] [OPTIONS]
```

| Subcommand | Description |
|------------|-------------|
| (none) | Enter devshell interactively |
| `trust` | Auto-activate on cd |
| `untrust` | Revoke auto-activation |
| `activate <SHELL>` | Generate activation script |
| `switch <TARGET>` | Switch active devshell |
| `clean` | Remove cached state |
| `daemon` | Manage daemon (start/stop/status) |
| `exec <CMD>` | Run command in devshell |
| `list` | List devshell packages |
| `status` | Show active devshells |

| Option | Description |
|--------|-------------|
| `--command <CMD>` | Run command then exit |
| `--all` | Clean all cached state |
| `--paths` | Show store paths (list) |
| `--json` | JSON output (list) |

### xi nix — nix proxy

```
xi nix [--unwrap] <ARGS...>
```

Enhanced commands: `build`, `develop`, `fmt`, `run`, `flake check/init/update/show`.
All others pass through to the real nix binary.

| Option | Description |
|--------|-------------|
| `--unwrap` | Bypass xi, run real nix directly |

## Common build options

These options are shared across `os`, `home`, `darwin`, `system`, and `flake`
commands:

| Option | Description |
|--------|-------------|
| `-n, --dry` | Dry run |
| `-a, --ask` | Confirm before activation |
| `-l, --out-link <PATH>` | Result symlink location |
| `--diff <MODE>` | `auto`, `always`, `never` |
| `--no-nom` | Disable nix-output-monitor |
| `-j, --max-jobs <N>` | Parallel build jobs |
| `--cores <N>` | CPU cores per build |
| `-k, --keep-going` | Continue on failure |
| `-K, --keep-failed` | Keep failed build outputs |
| `-L, --print-build-logs` | Stream build logs |
| `-t, --show-trace` | Detailed Nix error traces |
| `--accept-flake-config` | Trust flake nixConfig |
| `--impure` | Allow impure evaluation |
| `--offline` | No network access |
| `--refresh` | Force re-evaluation |
| `--no-net` | Prohibit all network |
| `-I, --include <PATH>` | Add to NIX_PATH |
| `--recreate-lock-file` | Fresh flake.lock |
| `--no-update-lock-file` | Don't update lock |
| `--no-write-lock-file` | Don't write lock |
| `--no-use-registries` | Ignore flake registries |
| `--commit-lock-file` | Git commit lock file |
| `--override-input <INPUT> <URL>` | Override flake input (repeatable) |
| `-Q, --no-build-output` | Suppress build output |
| `--use-substitutes` | Use substitutes for copy |
| `--option <NAME> <VALUE>` | Nix config option (repeatable) |
| `--repair` | Repair corrupt store |
| `--fallback` | Build locally if substituter fails |
| `--builders <EXPR>` | Custom remote builders |
| `--log-format <FORMAT>` | Log format |
| `--push-to <URL>` | Push to cache |
| `--push-cmd <CMD>...` | External push command |
| `--sign-key <PATH>` | Signing key |
| `--no-push` | Disable cache push |
| `--async-push` | Push in background |
| `-f, --file <PATH>` | Non-flake Nix file |
| `--expr <EXPR>` | Nix expression |

## Installable resolution

When a positional argument is given, xi resolves it in this order:

1. `/nix/store/...` — store path (used directly)
2. `--file <PATH>` — file mode evaluation
3. `--expr <EXPR>` — expression mode evaluation
4. Everything else — flake reference with optional `#attribute.path`

Environment variable fallback for flake references:

`XI_OS_FLAKE` > `XI_HOME_FLAKE` > `XI_DARWIN_FLAKE` > `XI_SYSTEM_FLAKE` > `XI_FLAKE`
