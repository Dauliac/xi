# Capability: xi-override-input-config

## ADDED Requirements

### Requirement: `--override-input` participates in the full configuration cascade

`--override-input` MUST be settable through an environment variable and a config
key, using the names the documented mapping rule produces: `XI_OVERRIDE_INPUT`
and `build.override_input`. The config key MUST be readable from
`~/.config/xi/config.toml`, from the committed project `.xi.toml`, and from a
gitignored `.xi.local.toml`.

#### Scenario: Config-only override reaches nix

- **WHEN** `override_input = { nix-lib = "~/ghq/github.com/Dauliac/nix-lib" }`
  is set under `[build]` in `~/.config/xi/config.toml` and `xi nix build .#xi`
  runs with no flag and no environment variable
- **THEN** nix receives
  `--override-input nix-lib path:/home/<user>/ghq/github.com/Dauliac/nix-lib`
  and the input resolves to the local checkout

#### Scenario: Nothing configured changes nothing

- **WHEN** no override is set in any config file, environment variable, or flag
- **THEN** the argv xi generates is identical to the argv it generated before
  this capability existed

#### Scenario: The lock file is never written as a side effect

- **WHEN** any command applies one or more overrides
- **THEN** `flake.lock` on disk is byte-identical before and after

### Requirement: Multiple inputs are expressible in both the file and the environment

The config value MUST be a map of input name to flakeref, so duplicate input
names are structurally impossible. `XI_OVERRIDE_INPUT` MUST accept several
`name=flakeref` pairs separated by any run of whitespace, including newlines,
and MUST split each pair on the first `=` only.

#### Scenario: Two inputs overridden from the environment

- **WHEN** `XI_OVERRIDE_INPUT="nix-lib=./a nix-vm-test=./b"` is set
- **THEN** both inputs are overridden, and the same value written across
  multiple lines produces the same result

#### Scenario: A flakeref containing `=` survives parsing

- **WHEN** a pair's value is `github:o/r?ref=main&rev=abc`
- **THEN** the input name is the text before the first `=` and the flakeref
  retains its own `=` characters intact

#### Scenario: A transitive input is addressable

- **WHEN** the config key is the quoted name `"nix-lib/flake-parts"`
- **THEN** nix receives `--override-input nix-lib/flake-parts <ref>`

### Requirement: Path values are normalized to absolute, encoded `path:` flakerefs

A value starting with `path:`, `/`, `./`, `../`, or `~` MUST be treated as a
filesystem path. Any other value, including a scheme-prefixed URL or a bare
indirect flakeref, MUST be passed through verbatim. A path value MUST be
tilde-expanded, made absolute, lexically cleaned without resolving symlinks,
percent-encoded, and emitted with the `path:` scheme.

#### Scenario: A local directory never reaches nix as a bare path

- **WHEN** the configured value is `~/ghq/github.com/Dauliac/nix-lib` and the
  override is applied
- **THEN** nix receives it with the `path:` scheme, so the input locks as
  `{"type":"path"}` rather than `{"type":"git"}`, and a newly created untracked
  file inside that checkout is visible to the build instead of failing with
  `Path '<file>' in the repository "..." is not tracked by Git`

#### Scenario: A path containing a space is percent-encoded

- **WHEN** the resolved path contains a space
- **THEN** the space is percent-encoded before the `path:` prefix is applied,
  and the command succeeds rather than failing with a parse error that prepends
  the current working directory to the literal text `path:`

#### Scenario: A URL value is not touched

- **WHEN** the configured value is `github:NixOS/nixpkgs/nixos-unstable`
- **THEN** it is forwarded verbatim with no path resolution attempted

#### Scenario: A bare word is not mistaken for a directory

- **WHEN** the configured value is `nixpkgs`, with no `./` prefix
- **THEN** it is treated as an indirect flakeref and forwarded verbatim

### Requirement: Relative values resolve against the context they were written in

A relative path value MUST resolve against its own source: the config file's
directory for a value read from a file, the flake root for a value read from
`XI_OVERRIDE_INPUT`, and the process working directory for a value given as a
CLI flag. A relative value in the machine-global config MUST be rejected.

#### Scenario: A project config value does not depend on the current directory

- **WHEN** `.xi.local.toml` at the flake root sets `nix-lib = "../nix-lib"` and
  a command runs from a nested subdirectory such as `crates/xi/`
- **THEN** the value resolves relative to the flake root, not to `crates/xi/`

#### Scenario: A CLI flag keeps nix's own semantics

- **WHEN** `--override-input nix-lib ../nix-lib` is typed at a prompt
- **THEN** it resolves against the process working directory, matching what nix
  itself would do with that argument

#### Scenario: A relative value in the global config is refused

- **WHEN** `~/.config/xi/config.toml` sets a relative path value
- **THEN** the command fails with an error stating that a machine-global config
  requires an absolute or tilde-prefixed path

#### Scenario: An unusable path fails with both spellings

- **WHEN** a value resolves to a directory that does not exist, or to one with
  no `flake.nix`
- **THEN** the error names the value exactly as written and the absolute path it
  resolved to

### Requirement: Overrides are scoped to the flake being operated on

Before invoking nix, xi MUST drop any override whose input name is not an input
of the target flake, determined by reading `flake.lock` without evaluating the
flake.

#### Scenario: A global entry is silent in unrelated repos

- **WHEN** `~/.config/xi/config.toml` overrides `nix-lib` and a build runs in a
  flake that has no `nix-lib` input
- **THEN** no override is passed and nix emits no `does not match any input`
  warning

### Requirement: Overrides are suppressed on lock-writing operations

Because `--override-input` implies `--no-write-lock-file`, xi MUST NOT inject
overrides into flake subcommands whose purpose is to write `flake.lock`, and
MUST report the suppression. Command kinds that do not accept flake arguments
MUST never receive the flag.

#### Scenario: `flake update` still updates the lock

- **WHEN** overrides are configured and `xi nix flake update` runs
- **THEN** no override is injected, the suppression is reported on stderr, and
  `flake.lock` is written as it would be without any configuration

#### Scenario: Store commands never receive flake flags

- **WHEN** a store-level command such as `nix store gc` is proxied
- **THEN** no override flag is added to its argv

### Requirement: Precedence is resolved per input name, and every applied override is reported

The cascade MUST be resolved per input name, in the order global config, then
`.xi.toml`, then `.xi.local.toml`, then `XI_OVERRIDE_INPUT`, then
`--override-input`, emitting one flag per surviving name. Each applied override
MUST be reported once per invocation with its resolved absolute path.

#### Scenario: A flag replaces only the input it names

- **WHEN** config overrides both `nix-lib` and `nix-vm-test`, and
  `--override-input nix-lib <other>` is passed
- **THEN** `nix-lib` takes the flag's value and `nix-vm-test` keeps the
  configured value

#### Scenario: A shadowed entry is not advertised

- **WHEN** the same input is set at two levels of the cascade
- **THEN** exactly one override for that input is emitted and only the winning
  value appears in the report

#### Scenario: The report names the resolved path

- **WHEN** an override is applied from a relative or tilde-prefixed value
- **THEN** the reported line shows the absolute path that was actually used
