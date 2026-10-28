# Configuration Reference

Xi has two configuration files:

- **`config.toml`** — user-level settings (build defaults, cache, develop
  daemon)
- **`.xi.toml`** — project-level settings (CI pipeline, doctor thresholds, test
  backends, materialization targets)

---

## config.toml — user configuration

Location (in priority order):

1. `$XI_CONFIG` (if set)
2. `$XDG_CONFIG_HOME/xi/config.toml`
3. `~/.config/xi/config.toml`

When using the NixOS/Home Manager/flake-parts module with wrapper enabled, the
config file is generated as a store path and `XI_CONFIG` is set automatically.

### Priority cascade

For any setting, the effective value follows this priority:

1. CLI flag (highest)
2. Environment variable
3. `config.toml`
4. Built-in default (lowest)

### `[build]` — build behaviour

| Key                   | Type        | Default          | Description                                          |
| --------------------- | ----------- | ---------------- | ---------------------------------------------------- |
| `nom`                 | bool        | `true`           | Use nix-output-monitor for build output              |
| `ci_backend`          | string      | `"devour-flake"` | CI backend: `devour-flake`, `nix-fast-build`, `auto` |
| `show_trace`          | bool        | `false`          | Pass `--show-trace` to Nix                           |
| `keep_going`          | bool        | `false`          | Pass `--keep-going` to Nix                           |
| `impure`              | bool        | `false`          | Allow impure evaluation                              |
| `accept_flake_config` | bool        | `false`          | Trust flake nixConfig                                |
| `offline`             | bool        | `false`          | No network access                                    |
| `max_jobs`            | int or null | `null`           | Max parallel build jobs                              |
| `connect_timeout`     | int         | `5`              | Nix connect timeout in seconds (0 = no timeout)      |

### `[cache]` — top-level cache settings

| Key                    | Type | Default | Description                      |
| ---------------------- | ---- | ------- | -------------------------------- |
| `async_push`           | bool | `false` | Push in background (via daemon)  |
| `queue_max_size`       | int  | `100`   | Maximum pending queue entries    |
| `queue_expiry_days`    | int  | `7`     | Days before queue entries expire |
| `queue_drain_interval` | int  | `300`   | Daemon drain interval in seconds |

### `[cache.<name>]` — named cache targets

Each named sub-table defines a push target.

| Key            | Type            | Description                                                  |
| -------------- | --------------- | ------------------------------------------------------------ |
| `push_url`     | string          | S3, SSH, or file URI                                         |
| `signing_key`  | string          | Path to signing key file                                     |
| `push_command` | list of strings | External push command (e.g. `["cachix", "push", "mycache"]`) |

Example:

```toml
[cache.my-s3]
push_url = "s3://my-bucket?region=eu-west-1"
signing_key = "/etc/nix/signing-key"

[cache.cachix]
push_command = ["cachix", "push", "mycache"]
```

### `[develop]` — devshell daemon

| Key             | Type            | Default | Description                                                    |
| --------------- | --------------- | ------- | -------------------------------------------------------------- |
| `eval_interval` | int             | `5`     | Seconds between re-evaluations                                 |
| `watch_extra`   | list of strings | `[]`    | Extra file patterns to watch (e.g. `["*.yaml", "Cargo.lock"]`) |

### `[locate]` — `xi run --locate` cache

| Key           | Type | Default | Description                                                         |
| ------------- | ---- | ------- | ------------------------------------------------------------------- |
| `cache_level` | int  | `2`     | `0` disabled, `1` remembers your prior choice per command, `2` full |

### `[fmt]` — formatting

| Key       | Type   | Default  | Description                                                  |
| --------- | ------ | -------- | ------------------------------------------------------------ |
| `backend` | string | `"auto"` | Formatter: `flake`, `nixfmt`, `alejandra`, `treefmt`, `auto` |

### `[test]` — testing

| Key        | Type            | Default | Description                                         |
| ---------- | --------------- | ------- | --------------------------------------------------- |
| `backends` | list of strings | `[]`    | Enabled test backends: `nix-unit`, `nixt`, `namaka` |

### Validation

Xi validates the config file on load. Unknown top-level keys or malformed
sub-tables produce an error with a hint about valid keys.

### Example

```toml
[build]
nom = true
ci_backend = "nix-fast-build"
keep_going = true
connect_timeout = 10

[cache]
async_push = true

[cache.my-cache]
push_url = "s3://nix-cache?region=eu-west-1"
signing_key = "/etc/nix/signing-key"

[develop]
eval_interval = 3
watch_extra = ["*.yaml", "Cargo.lock"]

[fmt]
backend = "alejandra"

[test]
backends = ["nix-unit", "namaka"]
```

---

## .xi.toml — project configuration

Located at the root of your flake directory. Controls per-project behaviour for
CI, doctor, formatting, testing, and materialization.

If `.xi.toml` is missing or fails to parse, xi uses defaults silently.

### `[ci]` — CI pipeline

| Key             | Type            | Default  | Description                                                |
| --------------- | --------------- | -------- | ---------------------------------------------------------- |
| `backend`       | string          | `"auto"` | CI build backend: `auto`, `devour-flake`, `nix-fast-build` |
| `extra-outputs` | list of strings | `[]`     | Extra flake output paths to discover and build             |

```toml
[ci]
backend = "nix-fast-build"
extra-outputs = ["containers", "images"]
```

### `[doctor]` — health check thresholds

| Key                        | Type            | Default | Description                                |
| -------------------------- | --------------- | ------- | ------------------------------------------ |
| `max-input-age-days`       | int             | `30`    | Warn if any flake input is older than this |
| `require-official-nixpkgs` | bool            | `true`  | Fail on unofficial nixpkgs forks           |
| `supported-branches`       | list of strings | `[]`    | Allowed nixpkgs branches (empty = any)     |

```toml
[doctor]
max-input-age-days = 14
require-official-nixpkgs = true
supported-branches = ["nixos-unstable", "master"]
```

### `[fmt]` — formatting

| Key       | Type   | Default  | Description                                    |
| --------- | ------ | -------- | ---------------------------------------------- |
| `backend` | string | `"auto"` | Formatter backend (same values as config.toml) |

### `[test]` — testing

| Key                 | Type            | Default    | Description                               |
| ------------------- | --------------- | ---------- | ----------------------------------------- |
| `backends`          | list of strings | `[]`       | Backends to run (empty = auto-detect all) |
| `runTests.attr`     | string          | `"tests"`  | Attribute path for eval-time tests        |
| `checks.filter`     | string          | `""`       | Glob filter on check derivation names     |
| `nix-unit.test-dir` | string          | `"tests/"` | Directory for nix-unit CLI                |
| `nixt.test-dir`     | string          | `"tests/"` | Directory for nixt CLI                    |

Custom test backends:

```toml
[[test.custom]]
name = "integration"
command = "nix-unit"
args = ["--flake", ".#integrationTests"]
```

### `[consumer]` — output aggregation

| Key               | Type            | Default              | Description                   |
| ----------------- | --------------- | -------------------- | ----------------------------- |
| `exclude-outputs` | list of strings | `["legacyPackages"]` | Outputs to skip               |
| `include-configs` | bool            | `true`               | Include system configurations |

### `[materialize]` — materialization

| Key                   | Type            | Default              | Description                                   |
| --------------------- | --------------- | -------------------- | --------------------------------------------- |
| `commit-path`         | string          | `"nix/materialized"` | Directory for committed files                 |
| `check-in-ci`         | bool            | `false`              | Verify freshness during `xi ci`               |
| `pre-build`           | bool            | `false`              | Run stale targets before build/CI             |
| `git-hide`            | bool            | `true`               | Apply git skip-worktree                       |
| `auto-stage`          | bool            | `false`              | Run `git add` after commit                    |
| `auto-stage-branches` | list of strings | `[]`                 | Restrict auto-stage to branches (empty = all) |

### `[[materialize.target]]` — materialization targets

Each target is an array entry:

| Key       | Type            | Required | Description                                  |
| --------- | --------------- | -------- | -------------------------------------------- |
| `name`    | string          | yes      | Human-readable identifier                    |
| `command` | string          | yes      | Shell command (stdout captured)              |
| `output`  | string          | yes      | Output path relative to commit-path          |
| `sources` | list of strings | no       | Glob patterns for SHA-256 cache invalidation |

```toml
[[materialize.target]]
name = "cargo-hash"
command = "nix eval .#cargoHash --json"
output = "cargo-hash.json"
sources = ["Cargo.lock", "Cargo.toml"]

[[materialize.target]]
name = "generated-nix"
command = "nix eval .#generateNix --json"
output = "generated.nix"
sources = ["src/**/*.rs"]
```

### `[sources]` - config-repo sources

Points xi at the git repo that holds your NixOS / home-manager / darwin
configuration, so `xi <scope> switch` works from any directory on the machine.
All keys are optional; a `.xi.toml` without a `[sources]` block keeps working
and `xi <scope> sources status` reports `Missing`.

| Key               | Type   | Default    | Description                                                     |
| ----------------- | ------ | ---------- | --------------------------------------------------------------- |
| `remote`          | string | none       | Remote git URL (`https://`, `ssh://`, scp-style, or local path) |
| `branch`          | string | `"main"`   | Branch to track                                                 |
| `working-dir`     | string | none       | Absolute path to your editable clone                            |
| `poll-interval`   | string | `"60s"`    | GitOps poll cadence, e.g. `60s`, `5m`                           |
| `require-signed`  | bool   | `false`    | Refuse to advance past unsigned commits                         |
| `allowed-signers` | string | none       | Absolute path to an SSH `allowed_signers` file                  |
| `sync-mode`       | string | `"prompt"` | `prompt`, `auto-copy`, or `never`                               |
| `installer`       | string | none       | `detsys`, `lix`, or `none`                                      |

The key set is closed: an unrecognised key is a load-time error naming the file
and line, so a typo surfaces immediately instead of silently doing nothing. URLs
and paths are validated at the same moment, not at first fetch. `working-dir`
and `allowed-signers` must be absolute, because sources config is read by
daemons and by `xi switch` runs started from arbitrary directories.

Point a machine at this repo with `xi <scope> switch <remote-url>`. The same
command works on a bare machine and on one already managed: it probes for Nix
first, clones or fast-forwards the repo into the sources dir, records it, then
builds. A later bare `xi <scope> switch` reuses what was recorded. Re-pointing a
scope at a different remote needs `--force`.

Scope is not a key: it comes from the verb you invoke (`xi home ...` is user
scope, `xi nixos|darwin|system ...` is system scope).

Precedence per key is CLI flag → `XI_SOURCES_*` → `.xi.toml [sources]` → Nix
module (`/etc/xi/sources.toml` or `$XDG_CONFIG_HOME/xi/sources.toml`) → built-in
default. `xi <scope> sources status --explain` reports which rung answered.

Resolved paths per scope:

| Concept | System (NixOS / nix-darwin) | User scope                         |
| ------- | --------------------------- | ---------------------------------- |
| sources | `/var/lib/xi/sources`       | `$XDG_DATA_HOME/xi/sources`        |
| state   | `/var/lib/xi/state`         | `$XDG_STATE_HOME/xi`               |
| config  | `/etc/xi/sources.toml`      | `$XDG_CONFIG_HOME/xi/sources.toml` |

Unset `XDG_*` variables fall back to the XDG Base Directory defaults
(`~/.local/share`, `~/.local/state`, `~/.config`).

```toml
[sources]
remote = "git@github.com:you/dotfiles.git"
branch = "main"
working-dir = "/home/you/src/dotfiles"
poll-interval = "5m"
```

### Full example

```toml
[ci]
backend = "nix-fast-build"
extra-outputs = ["containers"]

[doctor]
max-input-age-days = 14
supported-branches = ["nixos-unstable"]

[fmt]
backend = "alejandra"

[test]
backends = ["runTests", "checks", "nix-unit"]
runTests.attr = "tests"
checks.filter = "test-*"

[[test.custom]]
name = "integration"
command = "nix-unit"
args = ["--flake", ".#integrationTests"]

[materialize]
check-in-ci = true
pre-build = true
auto-stage = true

[[materialize.target]]
name = "cargo-hash"
command = "nix eval .#cargoHash --json"
output = "cargo-hash.json"
sources = ["Cargo.lock"]
```
