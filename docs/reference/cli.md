# CLI Reference

Every xi command, subcommand, and flag. Reference — describes what exists, does
not instruct. For task-oriented instructions, see the
[how-to guides](../README.md#how-to-guides--achieve-a-specific-goal).

## Command groups

| Group             | Commands                                                                                    |
| ----------------- | ------------------------------------------------------------------------------------------- |
| Configuration     | `os`, `home`, `darwin`, `system`                                                            |
| Flake operations  | `build`, `check`, `run`, `fmt`, `show`, `init`, `update`, `ci`, `lib`, `test`, `doctor`, `materialize` |
| Deployment        | `deploy`                                                                                    |
| Development       | `develop`, `search`                                                                         |
| Maintenance       | `cache`, `clean`, `nix`, `completions`                                                      |

## Global options

```
xi [OPTIONS] <COMMAND>
```

| Option                                | Description                                                                                |
| ------------------------------------- | ------------------------------------------------------------------------------------------ |
| `-v, --verbose`                       | Increase log verbosity (repeat: `-vv`, `-vvv`, `-vvvv`)                                    |
| `-e, --elevation-strategy <STRATEGY>` | Privilege elevation: `auto`, `none`, `passwordless`, or `program:<path>` (doas/sudo/run0)  |

## Configuration management

### xi os — NixOS system management

```
xi os <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand    | Description                                           |
| ------------- | ----------------------------------------------------- |
| `switch`      | Build and activate configuration, set as boot default |
| `boot`        | Build and set as boot default without activation      |
| `test`        | Build and activate without setting boot default       |
| `build`       | Build only, leave result in `./result`                |
| `repl`        | Load configuration in interactive REPL                |
| `info`        | List system generations                               |
| `rollback`    | Revert to previous generation                         |
| `build-vm`    | Build a VM activation script                          |
| `build-image` | Build a disk image                                    |

Flags for `switch` / `boot` / `test` / `build`:

| Option                       | Description                             |
| ---------------------------- | --------------------------------------- |
| `-H, --hostname <NAME>`      | NixOS configuration hostname            |
| `-s, --specialisation <NAME>`| Activate a named specialisation         |
| `-S, --no-specialisation`    | Ignore any active specialisation        |
| `--install-bootloader`       | Force bootloader installation           |
| `--build-host <HOST>`        | Build on remote host (SSH)              |
| `--target-host <HOST>`       | Deploy to remote host (SSH)             |
| `-R, --bypass-root-check`    | Allow running as root (advanced)        |
| `--no-validate`              | Skip pre-activation validation          |
| `--show-activation-logs`     | Show activation output                  |

Plus the [common build options](#common-build-options) and
[cache push options](#cache-push-options).

`xi os info`:

| Option              | Description        |
| ------------------- | ------------------ |
| `--fields <FIELDS>` | Columns to display |

`xi os rollback`:

| Option     | Description                            |
| ---------- | -------------------------------------- |
| `--to <N>` | Rollback to specific generation number |

`xi os build-vm`:

| Option                  | Description              |
| ----------------------- | ------------------------ |
| `-B, --with-bootloader` | Include bootloader in VM |
| `--run`                 | Launch VM after build    |

`xi os build-image`:

| Option                      | Description                                     |
| --------------------------- | ----------------------------------------------- |
| `--image-variant <VARIANT>` | Image variant from `config.system.build.images` |

### xi home — Home Manager

```
xi home <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand | Description        |
| ---------- | ------------------ |
| `switch`   | Build and activate |
| `build`    | Build only         |
| `repl`     | Load in REPL       |

| Option                        | Description                                                |
| ----------------------------- | ---------------------------------------------------------- |
| `-c, --configuration <NAME>`  | Home configuration name (defaults to `$USER`)              |
| `-s, --specialisation <NAME>` | Activate a named specialisation                            |
| `-S, --no-specialisation`     | Ignore any active specialisation                           |
| `-b, --backup-extension <EXT>`| Suffix to append when backing up conflicting files         |
| `--build-host <HOST>`         | Build on remote host (SSH)                                 |
| `--show-activation-logs`      | Show activation output                                     |

Plus the [common build options](#common-build-options) and
[cache push options](#cache-push-options).

### xi darwin — nix-darwin

```
xi darwin <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand | Description        |
| ---------- | ------------------ |
| `switch`   | Build and activate |
| `build`    | Build only         |
| `repl`     | Load in REPL       |

| Option                    | Description                                                |
| ------------------------- | ---------------------------------------------------------- |
| `-H, --hostname <NAME>`   | Darwin configuration hostname (defaults to system name)    |
| `--build-host <HOST>`     | Build on remote host (SSH)                                 |
| `-R, --bypass-root-check` | Allow running as root                                      |
| `--show-activation-logs`  | Show activation output                                     |

Plus the [common build options](#common-build-options) and
[cache push options](#cache-push-options).

### xi system — system-manager

For non-NixOS Linux managed by
[system-manager](https://github.com/numtide/system-manager).

```
xi system <SUBCOMMAND> [INSTALLABLE] [OPTIONS]
```

| Subcommand | Description        |
| ---------- | ------------------ |
| `switch`   | Build and activate |
| `build`    | Build only         |

| Option                    | Description                              |
| ------------------------- | ---------------------------------------- |
| `-H, --hostname <NAME>`   | system-manager configuration hostname    |
| `-R, --bypass-root-check` | Allow running as root                    |
| `--show-activation-logs`  | Show activation output                   |

Plus the [common build options](#common-build-options) and
[cache push options](#cache-push-options).

## Flake operations

Each command below is a **top-level** xi subcommand. There is no `xi flake ...`
group.

### xi build — build a flake output

```
xi build [INSTALLABLE] [OPTIONS]
```

| Option                  | Description                                     |
| ----------------------- | ----------------------------------------------- |
| `--all`                 | Build every buildable output via devour-flake   |
| `--recursive`           | Build subflakes recursively (implies `--all`)   |
| `--backend <BACKEND>`   | `auto` (default), `devour-flake`, `nix-fast-build` |
| `-o, --out-link <PATH>` | Result symlink location                         |
| `--no-link`             | Don't create result symlink                     |

Plus the [common build options](#common-build-options) and
[cache push options](#cache-push-options).

### xi check — run flake checks

```
xi check [TARGET] [OPTIONS]
```

Positional `TARGET` selects a specific check attribute. Without it, runs every
check in `checks.<system>`.

Plus the [common build options](#common-build-options).

### xi run — run a flake app or nixpkgs package

```
xi run [INSTALLABLE] [OPTIONS]
```

Runs the app at `apps.<system>.<name>` (or `packages.<system>.<name>.exePath`).

| Option                 | Description                                             |
| ---------------------- | ------------------------------------------------------- |
| `-l, --locate`         | Locate mode: treat argument as a command name and search nixpkgs |
| `--shell`              | Locate mode: open a shell with the resolved package in `PATH` |
| `--install`            | Locate mode: install the resolved package to your user profile |
| `--cache-level <0-2>`  | Locate mode cache: `0` disable, `1` choice only, `2` full |

Locate mode approximates the `comma` workflow: `xi run --locate ffmpeg -- -i in.mp4 out.mp4`
finds `ffmpeg` in nixpkgs and runs it once. See also `XI_RUN_LOCATE` and the
`[locate]` config section.

Plus the [common build options](#common-build-options).

### xi fmt — format the tree

```
xi fmt [FLAKE_REF] [OPTIONS]
```

| Option                | Description                                                       |
| --------------------- | ----------------------------------------------------------------- |
| `--backend <BACKEND>` | `auto` (default), `flake` (use the flake's own `formatter`), or a custom formatter binary name |

Plus the [common build options](#common-build-options).

### xi show — display flake outputs

```
xi show [FLAKE_REF] [OPTIONS]
```

| Option         | Description                                              |
| -------------- | -------------------------------------------------------- |
| `-j, --json`   | JSON output                                              |
| `--raw`        | Pass through `nix flake show` output verbatim            |
| `-a, --all`    | Include internal outputs (`debug`, `allSystems`, etc.)   |
| `-t, --show-trace` | Display error tracebacks                             |

Empty per-system categories (e.g. `legacyPackages`) are hidden unless `--all` is
given.

### xi init — initialise a flake

```
xi init [OPTIONS]
```

| Option              | Description                            |
| ------------------- | -------------------------------------- |
| `-T, --template <REF>` | Template flake ref (e.g. `templates#full`) |

### xi update — update flake inputs

```
xi update [INPUTS...] [OPTIONS]
```

Without inputs, updates every input. With one or more input names, updates only
those.

| Option              | Description                          |
| ------------------- | ------------------------------------ |
| `-f, --flake <REF>` | Flake to update (defaults to `.`)    |
| `--commit-lock-file`| Git-commit the resulting `flake.lock`|

Plus the [common build options](#common-build-options).

### xi ci — CI pipeline

Multi-phase validation and build. Suitable for CI systems and pre-merge checks.

```
xi ci [FLAKE_REF] [OPTIONS]
```

| Option                  | Description                                        |
| ----------------------- | -------------------------------------------------- |
| `-n, --dry-run`         | Print the plan without executing                   |
| `--backend <BACKEND>`   | `auto` (default), `devour-flake`, `nix-fast-build` |
| `--no-lock-check`       | Skip `flake.lock` sync verification                |
| `--no-eval`             | Skip all-systems evaluation                        |
| `--no-build`            | Skip building (validation-only run)                |
| `--no-ifd`              | Disallow import-from-derivation                    |
| `--current-system-only` | Only run against the current system                |
| `--recursive`           | Validate and build subflakes                       |
| `--no-health-check`     | Skip the `xi doctor` phase                         |
| `--no-test`             | Skip `runTests` evaluation                         |
| `--no-lib-eval`         | Skip `lib` deep-eval                               |
| `--continue-on-error`   | Don't stop on first failure                        |

Plus the [common build options](#common-build-options).

### xi lib — inspect lib outputs

```
xi lib [FLAKE_REF] [OPTIONS]
```

| Option             | Description                                            |
| ------------------ | ------------------------------------------------------ |
| `-E, --eval`       | Deep-evaluate lib outputs with `builtins.deepSeq`      |
| `-t, --show-trace` | Display error tracebacks                               |

### xi test — run flake tests

```
xi test [FLAKE_REF] [OPTIONS]
```

| Option                    | Description                                              |
| ------------------------- | -------------------------------------------------------- |
| `--backend <BACKEND>...`  | Restrict to specific backends (repeatable). Choices: `run-tests`, `checks`, `nix-unit`, `nixt`, `namaka` |
| `-F, --filter <PATTERN>`  | Glob filter on test names                                |
| `-l, --list`              | List detected tests without running                      |
| `--format <FORMAT>`       | `pretty` (default) or `json`                             |
| `--review`                | Interactive snapshot review (namaka backend)             |
| `-w, --watch`             | Re-run on file changes                                   |

Plus the [common build options](#common-build-options).

### xi doctor — diagnose the flake

```
xi doctor [FLAKE_REF]
```

Checks flake input freshness, nixpkgs branch age, IFD, and other health
signals. Thresholds are configured in the `[doctor]` section of `.xi.toml`.

### xi materialize — cache eval outputs

Persist expensive evaluation outputs (e.g. Cargo hashes, prefetch results) under
`nix/materialized/`, controlled by `.xi.toml`.

```
xi materialize [TARGETS...] [OPTIONS]
```

| Option     | Description                                     |
| ---------- | ----------------------------------------------- |
| `--commit` | Write results with git skip-worktree lifecycle  |
| `--check`  | Verify freshness — exit non-zero if stale       |
| `-l, --list` | Show targets and their staleness              |
| `--force`  | Ignore cache, re-run every target               |
| `--setup`  | Apply git `skip-worktree` and merge driver      |
| `--clean`  | Remove the cache directory                      |

## Deployment

### xi deploy — deploy configurations to remote machines

```
xi deploy [FLAKE_REF] [TARGETS...] [OPTIONS]
```

`xi deploy` selects a deployment backend based on what your flake exposes:

- `deploy` output → **deploy-rs** backend (native)
- `colmenaHive` output → **colmena** backend (shell-out)
- otherwise → **builtin** backend (xi's own SSH deploy loop)

Use `--backend` to force a specific one.

| Option                    | Description                                              |
| ------------------------- | -------------------------------------------------------- |
| `--backend <BACKEND>`     | `auto` (default), `deploy-rs`, `colmena`, `builtin`      |
| `--on <TAG>`              | Filter targets by tag/label (repeatable)                 |
| `-n, --dry-run`           | Build and show the plan without applying                 |
| `--skip-checks`           | Skip pre-deploy flake checks                             |
| `--no-magic-rollback`     | Disable auto-rollback (deploy-rs backend)                |
| `--confirm-timeout <SEC>` | Seconds to wait for user confirmation before rollback    |
| `-t, --show-trace`        | Display Nix error tracebacks                             |
| `--no-nom`                | Disable nix-output-monitor                               |

## Development

### xi develop — devshell management

```
xi develop [SUBCOMMAND] [OPTIONS]
```

Without a subcommand, enters the devshell for the current directory.

| Subcommand         | Description                                         |
| ------------------ | --------------------------------------------------- |
| _(none)_           | Enter devshell for the current flake                |
| `trust [TARGET]`   | Trust this flake for auto-activation on `cd`        |
| `untrust [TARGET]` | Revoke trust                                        |
| `activate <SHELL>` | Emit shell activation script for `bash`/`zsh`/`fish`|
| `switch <TARGET>`  | Switch the active async devshell                    |
| `clean`            | Remove cached state for the current flake           |
| `daemon`           | Manage the background daemon: `start`/`stop`/`status` |
| `exec -- <CMD>`    | Run a command inside the devshell then exit         |
| `list`             | List packages in the active devshell                |
| `status`           | Show active devshells                               |

| Option                  | Description                                          |
| ----------------------- | ---------------------------------------------------- |
| `-c, --command <CMD>`   | Run command then exit (when entering a shell)        |
| `--flake <REF>`         | Target a different flake                             |
| `-s, --shell <SHELL>`   | Force shell type (bash / zsh / fish)                 |
| `--all`                 | Apply to all cached flakes (`clean`)                 |
| `--paths`               | Show store paths (`list`)                            |
| `--json`                | JSON output (`list`)                                 |

Plus the [common build options](#common-build-options).

### xi search — package and option search

```
xi search [MODE] [OPTIONS] <QUERY>
```

If no mode is given, uses `--default-search` (or `XI_DEFAULT_SEARCH`, or
`packages`).

| Mode       | Description                                                       |
| ---------- | ----------------------------------------------------------------- |
| `packages` | Search Nixpkgs packages (default)                                 |
| `options`  | Search NixOS / Home Manager / darwin module options               |
| `offline`  | Search local [spam-db](https://github.com/feel-co/spam) databases |
| `prs`      | Search Nixpkgs pull requests                                      |
| `issues`   | Search Nixpkgs issues                                             |

| Option                    | Modes                          | Description                              |
| ------------------------- | ------------------------------ | ---------------------------------------- |
| `--json`                  | all                            | JSON output                              |
| `--limit <N>`             | packages, options, prs, issues | Result limit (default: 30)               |
| `-c, --channel <CH>`      | packages, options              | Nixpkgs channel (default: `nixos-unstable`) |
| `-P, --platforms`         | packages                       | Show supported platforms                 |
| `--scope <SCOPE>`         | options                        | `nixpkgs`, `home-manager`, or `all`      |
| `-D, --db <PATH>`         | offline                        | SPAM database path (repeatable, required)|
| `-d, --days <N>`          | prs, issues                    | Time window (default: 15)                |
| `--default-search <MODE>` | (top-level)                    | Default when no mode is given            |

`xi search prs` reads a GitHub token from, in order: `GH_TOKEN`,
`$XDG_STATE_HOME/xi/github-token`, `~/.local/state/xi/github-token`.

## Maintenance

### xi cache — binary cache push queue

```
xi cache <SUBCOMMAND>
```

| Subcommand | Description              |
| ---------- | ------------------------ |
| `status`   | Show pending pushes      |
| `retry`    | Drain the push queue     |
| `clear`    | Delete pending entries   |

`xi cache retry`:

| Option               | Description                          |
| -------------------- | ------------------------------------ |
| `--clear-on-failure` | Discard entries that fail            |
| `--max-age-days <N>` | Only retry entries newer than N days |
| `--max-size <N>`     | Limit queue size                     |

### xi clean — garbage collection

```
xi clean <TARGET> [OPTIONS]
```

| Target           | Description                  |
| ---------------- | ---------------------------- |
| `all`            | All profiles (system + user) |
| `user`           | Current user's profiles      |
| `profile <PATH>` | Specific profile path        |

| Option                        | Description                              |
| ----------------------------- | ---------------------------------------- |
| `-k, --keep <N>`              | Minimum generations to keep (default: 1) |
| `-K, --keep-since <DURATION>` | Keep entries within duration             |
| `-n, --dry-run`               | Dry run                                  |
| `-a, --ask`                   | Confirm before deleting                  |
| `--no-gc`                     | Skip `nix store gc`                      |
| `--no-gcroots`                | Skip GC-root cleanup                     |
| `--no-direnv`                 | Preserve direnv GC roots                 |
| `--optimise`                  | Run `nix-store --optimise` afterwards    |
| `--max <BYTES>`               | Limit to `nix store gc --max`            |
| `--keep-one`                  | Keep one active direnv GC root           |
| `-x, --cross-filesystems`     | Cross filesystem boundaries              |

### xi nix — transparent nix proxy

```
xi nix [--unwrap] <ARGS...>
```

Intercepts and routes to enhanced xi implementations:

| nix invocation      | Handled by       |
| ------------------- | ---------------- |
| `nix build`         | `xi build`       |
| `nix flake check`   | `xi check`       |
| `nix fmt`           | `xi fmt`         |
| `nix flake show`    | `xi show`        |
| `nix develop`       | `xi develop`     |
| `nix run`           | `xi run`         |
| `nix flake init`    | `xi init`        |
| `nix flake update`  | `xi update`      |

Anything else is passed through to `nix` verbatim.

| Option              | Description                                    |
| ------------------- | ---------------------------------------------- |
| `--unwrap`, `--raw` | Bypass xi enhancements — call the real nix     |

Set `XI_UNWRAP=1` for the same effect environment-wide.

### xi completions — generate shell completions

```
xi completions <SHELL>
```

Shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`, `nushell`.

## Common build options

Applied to every command that ends up invoking `nix build` (`os`, `home`,
`darwin`, `system`, `build`, `check`, `run`, `fmt`, `ci`, `test`, `update`,
`develop`).

| Option                          | Env                   | Description                        |
| ------------------------------- | --------------------- | ---------------------------------- |
| `-j, --max-jobs <N>`            | `XI_MAX_JOBS`         | Parallel build jobs                |
| `-k, --keep-going`              | `XI_KEEP_GOING`       | Continue on failure                |
| `-t, --show-trace`              | `XI_SHOW_TRACE`       | Detailed Nix error traces          |
| `--impure`                      | `XI_IMPURE`           | Allow impure evaluation            |
| `--offline`                     | `XI_OFFLINE`          | No network access                  |
| `--accept-flake-config`         | `XI_ACCEPT_FLAKE_CONFIG` | Trust flake `nixConfig`         |
| `--refresh`                     | —                     | Force refresh of flake inputs      |
| `--override-input <INPUT> <REF>`| —                     | Override a flake input (repeatable)|
| `--option <NAME> <VALUE>`       | —                     | Nix configuration option (repeatable) |
| `--no-nom`                      | `XI_NO_NOM`           | Disable nix-output-monitor         |

## Cache push options

Applied to every command that builds outputs (`os`, `home`, `darwin`, `system`,
`build`).

| Option                | Env                | Description                                  |
| --------------------- | ------------------ | -------------------------------------------- |
| `--push-to <URL>`     | `XI_CACHE_URL`     | Nix store URI (`s3://`, `ssh://`, `file://`) |
| `--push-cmd <CMD>...` | —                  | External push command (e.g. `cachix push mycache`) |
| `--sign-key <PATH>`   | `XI_SIGNING_KEY`   | Path to secret signing key                   |
| `--no-push`           | —                  | Disable cache push for this run              |
| `--async-push`        | `XI_CACHE_ASYNC`   | Queue push and return immediately            |

## Installable resolution

When a positional argument is given, xi resolves it in this order:

1. `/nix/store/...` — store path (used directly)
2. `--file <PATH>` — file-mode evaluation
3. `--expr <EXPR>` — expression-mode evaluation
4. Everything else — flake reference with optional `#attribute.path`

Environment fallback for flake references (in priority order):

`XI_OS_FLAKE` → `XI_HOME_FLAKE` → `XI_DARWIN_FLAKE` → `XI_SYSTEM_FLAKE` →
`XI_FLAKE`.
