# Environment Variables Reference

Every environment variable xi reads. Priority throughout: CLI flag → environment
variable → config file → default.

## Forwarded Nix variables

Standard Nix variables that xi forwards to underlying `nix` invocations when
running with a clean environment (e.g. under `sudo`):

| Variable                   | Description                                    |
| -------------------------- | ---------------------------------------------- |
| `NIX_SSHOPTS`              | SSH options for remote operations              |
| `NIX_CONFIG`               | Inline Nix configuration                       |
| `NIX_REMOTE`               | Nix daemon connection URI                      |
| `NIX_SSL_CERT_FILE`        | Custom SSL certificate file                    |
| `NIX_USER_CONF_FILES`      | Extra Nix config files                         |
| `NIX_SUDOOPTS`             | Extra sudo arguments (nixos-rebuild compat)    |
| `NIXOS_INSTALL_BOOTLOADER` | Force bootloader install when `true`           |
| `NIXOS_NO_CHECK`           | Inhibit NixOS service checks during activation |

## Flake references

Fallback order for a missing installable: `XI_OS_FLAKE` → `XI_HOME_FLAKE` →
`XI_DARWIN_FLAKE` → `XI_SYSTEM_FLAKE` → `XI_FLAKE`.

| Variable          | Description                                   |
| ----------------- | --------------------------------------------- |
| `XI_FLAKE`        | Default flake path for all commands           |
| `XI_OS_FLAKE`     | Flake for `xi os` (overrides `XI_FLAKE`)      |
| `XI_HOME_FLAKE`   | Flake for `xi home` (overrides `XI_FLAKE`)    |
| `XI_DARWIN_FLAKE` | Flake for `xi darwin` (overrides `XI_FLAKE`)  |
| `XI_SYSTEM_FLAKE` | Flake for `xi system` (overrides `XI_FLAKE`)  |
| `XI_FILE`         | Non-flake Nix file for `-f <PATH>` evaluation |
| `XI_ATTRP`        | Attribute path for non-flake evaluation       |

## Elevation and privilege

| Variable                | Description                                                 |
| ----------------------- | ----------------------------------------------------------- |
| `XI_ELEVATION_STRATEGY` | Elevation: `auto`, `none`, `passwordless`, `program:<path>` |
| `XI_ELEVATION_PROGRAM`  | Deprecated; auto-converted to `XI_ELEVATION_STRATEGY`       |
| `XI_SUDO_ASKPASS`       | Program used for `SUDO_ASKPASS` during self-elevation       |
| `XI_SUDOOPTS`           | Extra sudo arguments (takes precedence over `NIX_SUDOOPTS`) |
| `XI_SSHOPTS`            | SSH options (takes precedence over `NIX_SSHOPTS`)           |
| `XI_BYPASS_ROOT_CHECK`  | Skip the "don't run xi as root" guard                       |

## Build behaviour

| Variable                  | Description                                                                              |
| ------------------------- | ---------------------------------------------------------------------------------------- |
| `XI_NO_NOM`               | Disable nix-output-monitor                                                               |
| `XI_SHOW_TRACE`           | Enable `--show-trace`                                                                    |
| `XI_KEEP_GOING`           | Enable `--keep-going`                                                                    |
| `XI_IMPURE`               | Enable `--impure`                                                                        |
| `XI_ACCEPT_FLAKE_CONFIG`  | Enable `--accept-flake-config`                                                           |
| `XI_OFFLINE`              | Enable `--offline`                                                                       |
| `XI_MAX_JOBS`             | Set `--max-jobs`                                                                         |
| `XI_CONNECT_TIMEOUT`      | Substituter connection timeout in seconds (0 = off)                                      |
| `XI_CI_BACKEND`           | Default backend for `xi ci` / `xi build --all`: `auto`, `devour-flake`, `nix-fast-build` |
| `XI_NO_CHECKS`            | Skip startup checks (Nix version, experimental features)                                 |
| `XI_NO_VALIDATE`          | Skip pre-activation validation                                                           |
| `XI_SHOW_ACTIVATION_LOGS` | Show activation output (`1` to enable)                                                   |
| `XI_DIFF`                 | Activation diff mode: `auto`, `always`, `never`                                          |

## Cache push

| Variable                     | Description                        |
| ---------------------------- | ---------------------------------- |
| `XI_CACHE_URL`               | Default push URL                   |
| `XI_SIGNING_KEY`             | Default signing key path           |
| `XI_CACHE_ASYNC`             | Enable async push (`1` to enable)  |
| `XI_CACHE_QUEUE_EXPIRY_DAYS` | Days before queued entries expire  |
| `XI_CACHE_QUEUE_MAX_SIZE`    | Maximum queued entries before drop |

## Search

| Variable             | Description                                          |
| -------------------- | ---------------------------------------------------- |
| `XI_DEFAULT_SEARCH`  | Default search mode: `packages` or `options`         |
| `XI_SEARCH_CHANNEL`  | Default Nixpkgs channel (e.g. `nixos-unstable`)      |
| `XI_SEARCH_JSON`     | Truthy value to emit JSON                            |
| `XI_SEARCH_PLATFORM` | Truthy value to include platform lists (packages)    |
| `XI_OFFLINE_DB`      | Colon-separated SPAM database paths for offline mode |

## Run + locate

Locate mode for `xi run`. See the
[run and locate guide](../guides/run-and-locate.md).

| Variable          | Description                                                           |
| ----------------- | --------------------------------------------------------------------- |
| `XI_RUN_LOCATE`   | Truthy value to enable locate mode by default                         |
| `XI_LOCATE_CACHE` | Cache level for locate: `0` disabled, `1` remembers choices, `2` full |

## Develop

Consumed by `xi develop` and its background daemon.

| Variable           | Description                                                          |
| ------------------ | -------------------------------------------------------------------- |
| `XI_EVAL_INTERVAL` | Seconds between file-watch evaluations (default `5`)                 |
| `XI_WATCH_EXTRA`   | Colon-separated glob patterns to watch beyond `*.nix` / `flake.lock` |
| `XI_EVAL_CACHE`    | Eval cache mode: `none`, `lock` (default), `inputs`                  |

## GitHub

| Variable               | Description                                 |
| ---------------------- | ------------------------------------------- |
| `GH_TOKEN`             | GitHub token for `xi search prs` / `issues` |
| `XI_GITHUB_TOKEN_FILE` | Override the on-disk token file path        |

## Remote

| Variable            | Description                                   |
| ------------------- | --------------------------------------------- |
| `XI_REMOTE_CLEANUP` | Attempt to kill remote processes on interrupt |

## Sources (`xi sources`)

Every `[sources]` key in `.xi.toml` has a matching variable: uppercase the key
and replace hyphens with underscores.

| Variable                     | `[sources]` key   | Description                                                             |
| ---------------------------- | ----------------- | ----------------------------------------------------------------------- |
| `XI_SOURCES_REMOTE`          | `remote`          | Remote git URL of the config repo                                       |
| `XI_SOURCES_BRANCH`          | `branch`          | Branch to track (default `main`)                                        |
| `XI_SOURCES_WORKING_DIR`     | `working-dir`     | Absolute path to your editable clone, so `xi switch` works from any CWD |
| `XI_SOURCES_POLL_INTERVAL`   | `poll-interval`   | GitOps poll cadence, e.g. `60s`, `5m` (default `60s`)                   |
| `XI_SOURCES_REQUIRE_SIGNED`  | `require-signed`  | Refuse to advance past unsigned commits                                 |
| `XI_SOURCES_ALLOWED_SIGNERS` | `allowed-signers` | Absolute path to an SSH `allowed_signers` file                          |
| `XI_SOURCES_SYNC_MODE`       | `sync-mode`       | `prompt`, `auto-copy`, or `never` (default `prompt`)                    |
| `XI_SOURCES_INSTALLER`       | `installer`       | `detsys`, `lix`, or `none`                                              |
| `XI_SOURCES_STATE_DIR`       | none (env only)   | Override the resolved state dir; absolute paths only                    |

`XI_SOURCES_STATE_DIR` has no `.xi.toml` counterpart on purpose. The state dir
records what xi already deployed, so letting a repo-committed config redirect it
would let a config repo rewrite the history of its own deployment.

Values are validated when the config loads, not when the remote is first
contacted: a malformed URL, a relative `working-dir`, or an unparseable
`poll-interval` fails immediately and names the variable.

### Precedence

Sources config resolves through five rungs, highest first:

1. CLI flag (e.g. `--sources-remote <url>`)
2. Environment variable (`XI_SOURCES_*`)
3. `.xi.toml` `[sources]` key
4. Nix module default, written to `/etc/xi/sources.toml` (system scope) or
   `$XDG_CONFIG_HOME/xi/sources.toml` (user scope)
5. Built-in default (`branch = "main"`, `poll-interval = 60s`,
   `sync-mode = "prompt"`)

Each key walks the ladder independently, so a `.xi.toml` `branch` and an
`XI_SOURCES_REMOTE` combine rather than one shadowing the other wholesale. Run
`xi <scope> sources status --explain` to see which rung answered for each key.

## Configuration and diagnostics

| Variable          | Description                                                |
| ----------------- | ---------------------------------------------------------- |
| `XI_CONFIG`       | Override config file path                                  |
| `XI_LOG`          | Tracing filter directive (e.g. `xi=trace`)                 |
| `XI_NIX_BIN`      | Path to real nix binary (default: `nix`)                   |
| `XI_UNWRAP`       | Bypass `xi nix` enhancements (`1` to enable)               |
| `XI_PRESERVE_ENV` | `1` to force, `0` to disable env preservation on elevation |

## Backwards compatibility

| Old variable           | Replacement             | Behaviour                              |
| ---------------------- | ----------------------- | -------------------------------------- |
| `FLAKE`                | `XI_FLAKE`              | Auto-migrated with deprecation warning |
| `XI_ELEVATION_PROGRAM` | `XI_ELEVATION_STRATEGY` | Auto-converted                         |

## Propagation rules

- `XI_*` variables are explicitly propagated to subprocesses during environment
  isolation (e.g. under `sudo`, over SSH).
- `NIX_*` variables listed above are forwarded to underlying Nix commands.
- Any other environment variables are dropped from elevated or remote calls
  unless `XI_PRESERVE_ENV=1` is set.
