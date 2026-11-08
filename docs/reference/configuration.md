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
| `override_input`      | table       | `{}`             | Flake input overrides, input name to flakeref        |

#### `build.override_input`

A table mapping flake input name to flakeref. Must be written as a TOML inline
table.

```toml
[build]
override_input = { nix-lib = "~/ghq/github.com/Dauliac/nix-lib" }
```

Read from three files, in increasing order of priority:

| File                       | Base for a relative value | Committed?     |
| -------------------------- | ------------------------- | -------------- |
| `~/.config/xi/config.toml` | not permitted             | n/a            |
| `.xi.toml`                 | the file's directory      | yes            |
| `.xi.local.toml`           | the file's directory      | no, gitignored |

The full cascade, lowest priority first:

```
~/.config/xi/config.toml  <  .xi.toml  <  .xi.local.toml  <  XI_OVERRIDE_INPUT  <  --override-input
```

Resolution is per input name. One flag is emitted per surviving name.

A key containing `/` addresses a transitive input and must be quoted:

```toml
override_input = { "nix-lib/flake-parts" = "path:/some/checkout" }
```

**Value handling.** A value starting with `path:`, `/`, `./`, `../` or `~` is
treated as a filesystem path: tilde-expanded, made absolute, cleaned lexically,
percent-encoded, and emitted as `path:<abs>`. Every other value, including a
scheme-prefixed URL and a bare indirect flakeref, is forwarded verbatim. A path
value must resolve to a directory containing `flake.nix`.

**Emission.** Overrides are dropped when the named input is absent from the
target flake, and are never emitted for `nix flake update`, `nix flake lock`, or
store-level commands. Each applied override is reported once per invocation with
its resolved absolute path. `flake.lock` is never written as a side effect.

See
[Why flake input overrides work this way](../explanation/flake-input-overrides.md)
for the reasoning, and
[How to build against a local clone](../guides/multi-flake-development.md) for
the task.

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

Controls per-project behaviour for CI, doctor, formatting, testing, and
materialization. Loaded from three tiers, merged in order (later tiers win,
key by key, recursing into sub-tables - e.g. a global `[doctor]` setting and
a repo-local `[doctor.rules]` entry both survive rather than one replacing
the other wholesale):

1. **Global** - every `*.toml` file (sorted by name) in the same directory
   `config.toml` lives in (`$XI_CONFIG`'s directory, or
   `$XDG_CONFIG_HOME/xi/`, or `~/.config/xi/`). If `$XI_CONFIG` is set, only
   that one file is read here, not its whole directory.
2. **Repo-shared** - `xi.toml` or `.xi.toml` at the flake root (first one
   found; they do not merge with each other), then every `*.toml` (sorted) in
   `.config/xi/` at the flake root. Meant to be committed.
3. **Repo-local** - `.xi.local.toml` at the flake root. Meant to be
   gitignored: personal overrides (e.g. silencing a doctor rule just for
   you) that should not land in the shared repo-level file.

If every layer is missing or fails to parse, xi uses defaults silently - this
section's settings are deliberately lenient (a typo here must not break
`xi build`). The one exception is `[sources]`, which only ever reads the
single repo-shared file (see its own section below) and is strict: a
malformed `[sources]` block is a load-time error.

A sibling `.xi.local.toml` is read after `.xi.toml` and takes priority over it,
per key. It is gitignored: it holds machine-local values, and `xi doctor` warns
if it has been committed.

### `[build]` - build behaviour

| Key              | Type  | Default | Description                                   |
| ---------------- | ----- | ------- | --------------------------------------------- |
| `override_input` | table | `{}`    | Flake input overrides, input name to flakeref |

```toml
[build]
override_input = { nix-lib = "../nix-lib" }
```

A relative value resolves against the directory holding the file it was written
in. Same semantics as [`build.override_input`](#buildoverride_input) in
`config.toml`, except that relative values are permitted here.

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

| Key                        | Type            | Default | Description                                   |
| -------------------------- | --------------- | ------- | ---------------------------------------------- |
| `max-input-age-days`       | int             | `30`    | Warn if any flake input is older than this     |
| `require-official-nixpkgs` | bool            | `true`  | Fail on unofficial nixpkgs forks                |
| `supported-branches`       | list of strings | `[]`    | Allowed nixpkgs branches (empty = any)          |
| `disable-structure-checks` | bool            | `false` | Skip the structure check group entirely         |
| `disable-metadata-checks`  | bool            | `false` | Skip the metadata check group entirely          |
| `disable-output-checks`    | bool            | `false` | Skip the output-hygiene check group entirely    |

```toml
[doctor]
max-input-age-days = 14
require-official-nixpkgs = true
supported-branches = ["nixos-unstable", "master"]
```

#### `[doctor.rules]` - per-check overrides

Finer-grained than the three `disable-*-checks` group flags above: one entry
per check, keyed by the check's stable code. A value of `false` or `"off"`
drops that check's result entirely (it neither prints nor counts toward the
pass/fail tally); `"warn"` or `"fail"` forces that severity whenever the
check fires, regardless of what its own logic would otherwise produce.
`true`/`"on"` is the default and never needs to be written down.

```toml
[doctor.rules]
dirty-tree = "off"          # same effect as `false`
package-version = "warn"    # never hard-fail on a stale package version
duplicate-inputs = "fail"   # escalate what is normally only a warning
```

A check whose own result is `Ok` is never affected by a `"warn"`/`"fail"`
override (there is nothing to escalate); `"off"` still drops it.

Available codes:

| Code                            | Check                                              |
| -------------------------------- | --------------------------------------------------- |
| `nixpkgs-branch`                 | nixpkgs input tracks an allowed branch             |
| `input-freshness`                | no flake input older than `max-input-age-days`     |
| `nixpkgs-owner`                  | nixpkgs input points at an official repo           |
| `formatter`                      | `formatter.<system>` output is present             |
| `floating-input-ref`             | inputs are pinned, not tracking a floating ref     |
| `duplicate-inputs`               | no duplicate transitive inputs                     |
| `flake-description`              | top-level flake `description` is set               |
| `dirty-tree`                     | working tree has no uncommitted changes            |
| `lock-tracked`                   | `flake.lock` is tracked by git, not gitignored     |
| `nixconfig-substituter-pairing`  | every substituter has a matching trusted key       |
| `package-meta-completeness`      | `packages.<system>.*` sets `meta.description`      |
| `package-version`                | `packages.<system>.*` sets a non-stale `version`   |
| `app-main-program`               | `apps.<system>.*` resolves to a real program       |
| `package-insecure`               | `packages.<system>.*` is not marked insecure       |
| `per-system-output-gap`          | an output present for one system exists for all    |
| `checks-output-missing`          | `checks.<system>.*` output is present              |
| `nonstandard-output-key`         | no unrecognised top-level flake output keys        |

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
```
