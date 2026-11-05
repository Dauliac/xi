# Capability: xi-deploy-build-config

## ADDED Requirements

### Requirement: Backend selection follows a declarative-first ladder

Backend selection MUST be resolved by a ladder, highest precedence first: an
explicit CLI flag, a per-host `[[push.hosts]]` override, the table-level
`.xi.toml [push] backend` key, a Nix-module-written `xi.sources.push.backend`
default, and finally `detect_backend`'s auto-detect floor. The ladder MUST
share its resolution shape with the existing `remote-build` ladder (one
generalized helper), not duplicate it.

#### Scenario: Flake-parts default picks the backend with no flag

- **GIVEN** `xi.sources.push.backend = "colmena"` is written into the scope's
  config file by the flake-parts module
- **AND** no `--backend` flag, no `.xi.toml [push] backend` entry
- **WHEN** `xi push` runs against a fleet whose outputs would otherwise
  auto-detect `builtin`
- **THEN** every target is pushed via `colmena`

#### Scenario: CLI flag outranks every other rung

- **GIVEN** the same flake-parts default as above
- **WHEN** `xi push --backend builtin` runs
- **THEN** every target is pushed via `builtin`, not `colmena`

### Requirement: A batched build strategy decouples building from shipping

With `--build-strategy batched`, every selected `PushMode::Closure` target
MUST be built in an upfront phase, before any target is copied to or
activated, producing a manifest mapping each target's name to its built store
path. A subsequent invocation given that manifest MUST ship every target it
names without rebuilding. With no `--build-strategy` flag, behavior MUST be
unchanged from today: build, copy, and activate each target in sequence (or
per `--parallel`), with no manifest produced.

#### Scenario: Batched build produces a manifest before shipping anything

- **GIVEN** five `PushMode::Closure` targets selected for a push
- **WHEN** `xi push --build-strategy batched --build-only` runs
- **THEN** all five targets are built before any is copied or activated, and
  a manifest naming all five store paths is written

#### Scenario: A manifest ships without rebuilding

- **GIVEN** the manifest from the previous scenario
- **WHEN** a later invocation runs against that manifest
- **THEN** all five targets are copied and activated from the manifest's
  recorded store paths, and `nix build` is not invoked again for any of them

#### Scenario: Default strategy is unchanged

- **GIVEN** no `--build-strategy` flag
- **WHEN** a closure-mode push runs
- **THEN** each target is built, copied, and activated in the same order and
  with the same per-target behavior as before this capability existed

### Requirement: The build-location floor favors remote builds for genuinely remote targets

Behind a one-release opt-in, the build-location ladder's compiled floor (used
when no CLI flag, `.xi.toml` entry, or flake-parts default resolves it) MUST
default to remote-build (`true`) for a target that is not the machine the
command is running on, and MUST NOT consult the ladder at all when the
activation target *is* the local machine. Every existing override rung keeps
applying unchanged on top of the new floor.

#### Scenario: An unconfigured remote target defaults to building itself, after the opt-in

- **GIVEN** the opt-in is active
- **AND** a target with no `--remote-build` flag, no `.xi.toml` entry, and no
  Nix-module default configured
- **WHEN** a push resolves the build-location ladder for that target
- **THEN** it resolves to remote-build (`true`)

#### Scenario: A configured override still wins

- **GIVEN** the same opt-in and the same unconfigured target, but
  `.xi.toml [push] remote-build = false` at the table level
- **WHEN** the ladder resolves
- **THEN** it resolves to closure (`false`), because the table-level override
  outranks the floor

#### Scenario: The local machine never consults the ladder

- **GIVEN** an `xi deploy` invocation that resolves to activating the local
  machine itself
- **WHEN** build-location resolution would otherwise run
- **THEN** the ladder is not consulted at all; the build happens locally
  because the source and the target are the same machine
