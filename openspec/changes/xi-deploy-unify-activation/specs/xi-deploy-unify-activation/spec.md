# Capability: xi-deploy-unify-activation

## ADDED Requirements

### Requirement: Push activates a target through the same entrypoint a local switch uses

A fleet push targeting the `builtin` backend MUST end its per-target work by
invoking the target's own `xi <scope> switch` activation entrypoint over SSH,
instead of a raw remote script. `--mode closure` MUST point that invocation at
the copied store path; `--mode sources` MUST point it at the rsync'd sources
dir. The invocation MUST carry `XI_APPLIED_ORIGIN=push:<operator
hostname>` in
its environment.

#### Scenario: Closure-mode push activates via the local entrypoint

- **GIVEN** `xi push web1 --mode closure` has copied a built store path to
  `web1`
- **WHEN** the activation step runs
- **THEN** the command executed on `web1` is an `xi <scope> switch` invocation
  against that store path, not a bare `nix-env --set` /
  `switch-to-configuration` script

#### Scenario: Sources-mode push activates via the local entrypoint

- **GIVEN** `xi push web1 --mode sources` has rsync'd the sources dir to `web1`
- **WHEN** the activation step runs
- **THEN** the command executed on `web1` is an `xi <scope> switch` invocation
  against the synced sources dir, not a bare `nixos-rebuild switch --flake`
  script

### Requirement: An inbound push takes the target's sources lock

`ActorName` MUST include a `Push` variant. The target-side `xi <scope> switch`
invocation an inbound push execs MUST acquire the target's `SourcesLock` under
`ActorName::Push` before building or activating, exactly as an interactive
switch or a gitops-daemon tick would under their own actor names.

#### Scenario: A push and a daemon tick on the same target serialize

- **GIVEN** `web1` runs the gitops daemon and is mid-tick, holding its
  `SourcesLock`
- **WHEN** an inbound `xi push web1` reaches its activation step
- **THEN** the push's `xi <scope> switch` invocation waits for the lock rather
  than racing the daemon's own activation

### Requirement: Every activation records where it came from

`record_activation_at` MUST write an `applied-origin` state-dir marker on every
activation: `local` when no `XI_APPLIED_ORIGIN` is set and no SSH-session
environment variable is present (today's `applied-via` inference, unchanged);
`daemon` when the gitops daemon's own activation path runs; and
`push:<operator hostname>` when `XI_APPLIED_ORIGIN` is set by an inbound push.
When the triggering push supplied an `[[push.hosts]]` name for this host,
`record_activation_at` MUST also write an `inventory-role` marker holding that
name.

#### Scenario: A pushed target reports its origin

- **GIVEN** `xi push web1` has just activated `web1`
- **WHEN** `xi <scope> status --json` runs on `web1`
- **THEN** the envelope reports
  `applied-origin: "push:<the operator's
  hostname>"` and, if `web1` matched a
  `[[push.hosts]]` entry named `web1`, `inventory-role: "web1"`

#### Scenario: A daemon-ticked target reports its origin

- **GIVEN** the gitops daemon just activated a new generation on its own
- **WHEN** `xi <scope> status --json` runs on that host
- **THEN** the envelope reports `applied-origin: "daemon"`

### Requirement: An explicit local path is checked against the configured working_dir

When `xi <scope> switch`'s positional argument resolves to a local path (not a
config-remote to adopt) and this scope has a configured `working_dir` whose
canonical path differs from the positional's canonical path, the switch MUST
warn and prompt for confirmation before building from the positional, unless
`--force-working-dir` is given. With no TTY attached and no
`--force-working-dir`, the switch MUST refuse rather than prompt.

#### Scenario: A mismatched path prompts interactively

- **GIVEN** scope `system`'s configured `working_dir` is `/home/me/dotfiles`
- **WHEN** `xi system switch ./experiments/fork` runs from a TTY, and
  `./experiments/fork` canonicalizes to a different path
- **THEN** xi prints a warning naming both paths and prompts
  `Build and activate from <path> anyway? [y/N]` before building

#### Scenario: --force-working-dir skips the prompt

- **GIVEN** the same mismatch as above
- **WHEN** `xi system switch ./experiments/fork --force-working-dir` runs
- **THEN** xi builds from `./experiments/fork` without prompting

#### Scenario: A mismatch with no TTY refuses

- **GIVEN** the same mismatch as above
- **WHEN** `xi system switch ./experiments/fork` runs with no TTY attached and
  without `--force-working-dir`
- **THEN** xi exits non-zero, naming the mismatch and `--force-working-dir`,
  without building

### Requirement: One backend-detection function for both target-list and flake-probing callers

Backend detection (`deploy-rs` > `colmena` > `builtin` by flake-output
precedence) MUST be implemented once. A caller that already has a filtered
target list (a push) MUST use that function directly; a caller that only has a
flake reference (the retired legacy `deploy`'s delegation target) MUST discover
targets first and then call the same function, never a second, independently
maintained precedence check.

#### Scenario: Both callers agree on the same fleet

- **GIVEN** a flake declaring `deploy.nodes.web1` and `nixosConfigurations.web2`
- **WHEN** backend detection runs for `web1` and `web2` via the target-list path
  and, separately, via the flake-probing path
- **THEN** both resolve `web1` to `deploy-rs` and `web2` to `builtin`, because
  both call the same underlying function
