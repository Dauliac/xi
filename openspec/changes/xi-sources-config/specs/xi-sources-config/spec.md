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
`xi <scope> sources status --explain` (adds a `provenance` map to the response).

#### Scenario: Flag beats everything

- **GIVEN** the same key is set via all five surfaces with distinct values
- **WHEN** the user runs the command with the CLI flag
- **THEN** the flag's value is used
- **AND** `xi <scope> sources status --explain` reports provenance = `cli-flag`

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
- **WHEN** `xi home sources status` runs
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
- **WHEN** `xi home sources status` runs
- **THEN** the resolved branch is `dev`

### Requirement: Switch is the sources entrypoint

`xi <scope> switch [<remote-url>]` MUST be the only entrypoint for pointing a
machine at a config repo, where `<scope>` is one of `nixos`, `darwin`, `system`
or `home`. The same invocation MUST work on a machine with no Nix installed and
on a machine already managed by xi. Rationale and the rejected alternatives are
in `design.md`.

When the positional is a config-repo remote, switch MUST:

1. Take scope from the invoked verb, never from a flag.
2. Run the installer pre-check owned by [[xi-sources-bootstrap]].
3. Create the sources dir, state dir and config dir per the path table.
4. Clone the remote into the sources dir, or fast-forward it when the same
   remote is already recorded for this scope.
5. Record the remote as this scope's source.
6. Proceed with the normal build and activation against the sources dir.

Adoption MUST be idempotent: re-running with the remote already recorded for
this scope updates the clone and switches, rather than failing.

Re-pointing a scope at a _different_ remote MUST be refused without `--force`,
because it silently replaces what the machine deploys from.

#### Scenario: Adopt on a fresh machine

- **GIVEN** a machine with no recorded source for the user scope
- **WHEN** `xi home switch https://git.example/dotfiles` runs
- **THEN** `$XDG_DATA_HOME/xi/sources` contains a clone of the repo
- **AND** the remote is recorded as the user scope's source
- **AND** the home-manager configuration is built and activated from it

#### Scenario: The same command works on a managed machine

- **GIVEN** `https://git.example/dotfiles` is already this scope's recorded
  source
- **WHEN** `xi home switch https://git.example/dotfiles` runs again
- **THEN** the command succeeds
- **AND** the existing clone is updated rather than re-cloned
- **AND** the configuration is rebuilt and activated

#### Scenario: Switching remotes needs --force

- **GIVEN** `https://git.example/dotfiles` is this scope's recorded source
- **WHEN** `xi home switch https://git.example/other` runs without `--force`
- **THEN** the command exits non-zero naming both remotes
- **AND** no file and no clone is modified

#### Scenario: Bare switch uses the recorded source

- **GIVEN** a scope with a recorded source
- **WHEN** `xi home switch` runs with no positional
- **THEN** the recorded source is used
- **AND** no network clone is attempted beyond the normal fetch

### Requirement: Remote and installable are distinguished without a flag

Switch MUST decide whether its positional is a config-repo remote or a flake
installable from the argument's own shape, with no flag on the happy path.

Scp-style `user@host:path`, and bare `https`, `http` and `ssh` URLs, MUST be
read as config-repo remotes. Everything xi already parses as an installable,
including `github:owner/repo`, `git+<scheme>://...`, `path:...`, store paths and
filesystem paths, MUST keep its current meaning.

`--adopt` and `--no-adopt` MUST be available to force either reading.

#### Scenario: A flake reference is not adopted

- **WHEN** `xi system switch github:you/cfg` runs
- **THEN** the argument is evaluated as a flake installable, as it was before
  this capability existed
- **AND** no source is recorded and no clone is made

#### Scenario: An https remote is adopted

- **WHEN** `xi system switch https://git.example/cfg.git` runs
- **THEN** the argument is read as a config-repo remote and adopted

### Requirement: `sources` is scoped by its parent verb

The maintenance surface MUST be mounted under each scope verb as
`xi <scope> sources <verb>`, and MUST NOT exist as a top-level `xi sources`
command.

Scope MUST come from the parent verb, so the `--scope` flag and the
`[sources] scope` config key MUST NOT exist. The verb-to-scope mapping is the
resolver owned by [[xi-sources-model]].

This group is for operator maintenance (inspection today; sync, reset and daemon
control as their changes land). It is not how a machine is set up.

#### Scenario: Scope comes from the verb

- **WHEN** `xi home sources status` runs
- **THEN** the reported scope is `user`
- **AND** `xi system sources status`, `xi nixos sources status` and
  `xi darwin sources status` all report `system`

#### Scenario: No top-level sources command

- **WHEN** `xi sources status` runs
- **THEN** the command is rejected as an unrecognised subcommand

#### Scenario: Scope cannot be stated twice

- **WHEN** any `xi <scope> sources` verb is invoked with `--scope`
- **THEN** the flag is rejected as unknown
