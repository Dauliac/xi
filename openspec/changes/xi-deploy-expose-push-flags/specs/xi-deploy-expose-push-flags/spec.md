# Capability: xi-deploy-expose-push-flags

## ADDED Requirements

### Requirement: `xi deploy` exposes every `PushArgs` flag `DeployArgs` is missing

`DeployArgs` MUST carry `sources_transport`, `retry`, `build_strategy`,
`build_only`, `from_manifest`, `reattach`, and `notify`, matching
`push::args::PushArgs`'s own field name, type, and default for each one.
`crates/xi/src/deploy.rs::push_args()` MUST pass each straight through from
`DeployArgs` to the `PushArgs` a fleet rollout dispatches to, the same way it
already does for `mode` and `remote_build`.

#### Scenario: `--sources-transport` reaches the fleet rollout

- **GIVEN** a fleet rollout with no per-host `sources-transport` override
- **WHEN** `xi deploy --sources-transport target-pull` runs
- **THEN** every target resolves `PushMode::TargetPull`, identical to
  `xi
  push --sources-transport target-pull`

#### Scenario: `--retry` reaches the fleet rollout

- **GIVEN** a fleet rollout with no `.xi.toml [push] retry-attempts` entry
- **WHEN** `xi deploy --retry 0` runs
- **THEN** every target's transient-failure retrying is disabled entirely,
  identical to `xi push --retry 0`

#### Scenario: `--build-strategy batched --build-only` reaches the fleet rollout

- **GIVEN** five targets selected for a fleet rollout
- **WHEN** `xi deploy --build-strategy batched --build-only` runs
- **THEN** all five are built upfront and a manifest is written, with nothing
  copied or activated, identical to the `xi push` equivalent

#### Scenario: `--from-manifest` ships without rebuilding

- **GIVEN** the manifest from the previous scenario
- **WHEN** `xi deploy --from-manifest <path>` runs
- **THEN** every named target is copied and activated from its recorded store
  path, with no `nix build` call, identical to the `xi push` equivalent

#### Scenario: `--notify` fires on a terminal outcome

- **GIVEN** a fleet rollout with `xi deploy --notify <command>`
- **WHEN** a terminal outcome (success or confirmed failure) for the push is
  observed and its receipt is removed
- **THEN** `<command>` runs exactly once, identical to
  `xi push --notify
  <command>`

#### Scenario: Validation errors match `xi push`'s own

- **GIVEN** `--build-only` and `--from-manifest` both given, or
  `--build-only`/`--from-manifest` given with `--build-strategy` omitted or
  `interleaved`
- **WHEN** `xi deploy` runs with either combination
- **THEN** it refuses with the same error `xi push` already gives for that
  combination - `PushArgs::validate()` is reached unchanged, not reimplemented

### Requirement: `xi deploy --reattach` bypasses the decision tree

With `--reattach` given, `xi deploy` MUST dispatch directly to
`push::orchestrate::run`'s existing reattach handling before any fact gathering
or decision-tree resolution runs for the scope - mirroring
`xi
push --reattach`'s own precedent, where the check happens before
`args.validate()`, context resolution, or dirty-tree inspection. The decision
tree's local-vs-fleet resolution MUST NOT run at all for a `--reattach`
invocation.

#### Scenario: `--reattach` lists receipts without resolving the decision tree

- **GIVEN** a host with an adopted source and no fleet configured at all - a
  case the decision tree would otherwise resolve to `ActivateLocal`
- **WHEN** `xi deploy --reattach` runs with no id
- **THEN** it lists every local push receipt and probes each target's
  reachability, without ever calling `resolve_plan` or dispatching to
  `dispatch_local`

#### Scenario: `--reattach` works identically through either verb

- **GIVEN** the same local push-receipt state
- **WHEN** `xi push --reattach` and `xi deploy --reattach` each run in turn
- **THEN** both produce the same listing, probing the same targets the same way
