# Capability: xi-sources-config

## Relationship to other capabilities

This capability is the single config surface for the whole `xi-sources` family.
It takes the OS-neutral concepts named by [[xi-sources-model]]

- sources dir, state dir, working dir, and the `Scope` resolver - and binds them
  to concrete filesystem paths, a `.xi.toml [sources]` schema, an env surface
  and three Nix-module option trees.

Every downstream capability reads its configuration through this ladder rather
than parsing config itself:

- [[xi-sources-gitops]] - `remote`, `branch`, `poll-interval`, `require-signed`,
  `allowed-signers`
- [[xi-sources-push]] - `remote`, `branch`, the resolved sources dir
- [[xi-sources-bootstrap]] - `installer`, plus the path table used to seed a
  fresh host
- [[xi-sources-cache-prestage]] - the resolved sources and state dirs
- [[xi-sources-conflict]] - `sync-mode`, `working-dir`

A downstream capability that needs a new key amends the `.xi.toml [sources]`
schema requirement below rather than inventing a parallel surface.

## ADDED Requirements

### Requirement: Precedence ladder

Every sources config key MUST resolve through the following precedence (highest
wins):

1. CLI flag (e.g. `--sources-remote <url>`)
2. Environment variable (`XI_SOURCES_*`)
3. `.xi.toml [sources]` key
4. Nix-module default written to `/etc/xi/sources.toml` or
   `$XDG_CONFIG_HOME/xi/sources.toml`
5. Built-in default (e.g. `branch = "main"`, `poll_interval = 60s`)

The resolved value's provenance MUST be observable via
`xi sources status --explain` (adds a `provenance` map to the response).

#### Scenario: Flag beats everything

- **GIVEN** the same key is set via all five surfaces with distinct values
- **WHEN** the user runs the command with the CLI flag
- **THEN** the flag's value is used
- **AND** `xi sources status --explain` reports provenance = `cli-flag`

### Requirement: Filesystem path table

The capability MUST use the following paths per scope:

| Concept | System (NixOS or nix-darwin) | User scope                         |
| ------- | ---------------------------- | ---------------------------------- |
| sources | `/var/lib/xi/sources`        | `$XDG_DATA_HOME/xi/sources`        |
| state   | `/var/lib/xi/state`          | `$XDG_STATE_HOME/xi`               |
| config  | `/etc/xi/sources.toml`       | `$XDG_CONFIG_HOME/xi/sources.toml` |

XDG fallbacks follow the XDG Base Directory spec:

- `XDG_DATA_HOME` unset → `~/.local/share`
- `XDG_STATE_HOME` unset → `~/.local/state`
- `XDG_CONFIG_HOME` unset → `~/.config`

#### Scenario: XDG unset falls back to spec defaults

- **GIVEN** `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME` are all unset
- **WHEN** `xi sources status` runs at user scope
- **THEN** paths resolve to `~/.local/share/xi/sources`, `~/.local/state/xi`,
  `~/.config/xi/sources.toml`

#### Scenario: System paths require root at activation

- **GIVEN** the Nix module `xi.sources.enable = true` is set at `nixos` scope
- **WHEN** the system activates
- **THEN** `/var/lib/xi/sources` and `/var/lib/xi/state` exist with mode `0700`,
  owned by `root:root`
- **AND** no `xi` command has been invoked yet

### Requirement: `.xi.toml [sources]` schema

The `[sources]` section accepts the following keys, all optional:

- `remote` - remote git URL (ssh or https)
- `branch` - default `main`
- `working-dir` - path on local filesystem; records the user's editable clone
  location so `xi switch` from any CWD can find it
- `poll-interval` - duration string (e.g. `60s`, `5m`); default `60s`
- `require-signed` - boolean; when `true`, the gitops daemon refuses to advance
  past unsigned commits (details in [[xi-sources-gitops]])
- `allowed-signers` - path to an SSH `allowed_signers` file; required when
  `require-signed = true`
- `scope`, `system` or `user`; required when ambiguous (running as root with
  both scopes configured)
- `sync-mode`, one of `prompt | auto-copy | never`; default `prompt`. Owned by
  [[xi-sources-conflict]]; read here so it travels with the rest of the sources
  config through the precedence ladder.
- `installer`, one of `detsys | lix | none`; default picks `lix` when the
  resolved flake's declared substituters list contains `cache.lix.systems`,
  otherwise `detsys`. Owned by [[xi-sources-bootstrap]]; read here so phase 2 of
  the bootstrap flow resolves it from the same precedence ladder.

Any key outside this set MUST cause a load-time error naming the file+line.

#### Scenario: Unknown key errors with location

- **GIVEN** `.xi.toml` contains `[sources] frobnicate = true`
- **WHEN** any xi command loads the config
- **THEN** the command exits non-zero with a message
  `<file>:<line>:
  unknown key 'frobnicate' in [sources]`

### Requirement: Env variable surface

The capability MUST support env variables prefixed `XI_SOURCES_` for every
`[sources]` key, with hyphens replaced by underscores (e.g. `poll-interval` →
`XI_SOURCES_POLL_INTERVAL`).

#### Scenario: Env variable beats toml

- **GIVEN** `.xi.toml` sets `branch = "main"` and `XI_SOURCES_BRANCH=dev`
- **WHEN** `xi sources status` runs
- **THEN** the resolved branch is `dev`

### Requirement: `xi sources init`

The subcommand `xi sources init <remote-url>` MUST:

1. Determine scope (`--scope` flag, else `system` if effective uid is 0, else
   `user`).
2. Refuse if `.xi.toml` already has a `[sources]` block, unless `--force`.
3. Write a minimal `[sources]` block carrying only the user-supplied keys.
4. Create the sources dir, state dir, and config dir per the path table.
5. Clone the remote into the sources dir.
6. Return a response envelope matching the house convention; status is `Ready`
   on success, `Degraded` if `require-signed = true` and the HEAD is unsigned,
   `Pending` if the clone needs credentials.

#### Scenario: Init on fresh user profile

- **GIVEN** a user-scope config dir without `.xi.toml`
- **WHEN** `xi sources init https://git.example/dotfiles` runs
- **THEN** `.xi.toml` is created with
  `[sources] remote =
  "https://git.example/dotfiles"`
- **AND** `$XDG_DATA_HOME/xi/sources` contains a clone of the repo
- **AND** `xi sources status` returns `Ready`

#### Scenario: Init refuses to overwrite

- **GIVEN** `.xi.toml` already contains a `[sources]` block
- **WHEN** `xi sources init <different-url>` runs without `--force`
- **THEN** the command exits non-zero without modifying any file
