# Capability: xi-deploy-unified-verb

## ADDED Requirements

### Requirement: `xi deploy`'s decision tree resolves local vs. fleet independently of cwd

With no explicit target and no `--local`/`--fleet` override, `xi deploy` MUST
resolve, in order: an explicit target or `--target` activates exactly those
targets; a host whose own machine-id matches an inventory entry activates
locally; a scope with no adopted source and no targets or inventory refuses,
naming `--adopt`; a dirty relevant tree activates locally; a clean, adopted host
with no inventory and no flake-deploy outputs activates locally; otherwise it
rolls out to the discovered fleet. None of these steps MUST read `cwd`.

#### Scenario: A standalone adopted host activates locally from anywhere

- **GIVEN** a host with an adopted source, a clean tree, and no inventory or
  flake-deploy outputs at all
- **WHEN** `xi deploy` runs from `/tmp`, or from the scope's `working_dir`, or
  from any other directory
- **THEN** it activates locally in every case, with identical behavior
  regardless of `cwd`

#### Scenario: A dirty tree activates locally before any fleet rollout

- **GIVEN** the scope's relevant tree (working dir if configured, sources dir
  otherwise) is dirty
- **WHEN** `xi deploy` runs with no explicit target
- **THEN** it activates locally; it does not roll out to a fleet even when one
  is configured

### Requirement: `cwd` disambiguates scope, not destination

When a host has more than one scope adopted, `xi deploy` with no explicit
`--scope` MUST prefer the scope whose configured `working_dir` matches the
canonicalized `cwd`, when such a match exists. This resolution MUST NOT change
whether the matched scope's own tree then activates locally or rolls out to a
fleet - that remains governed entirely by the decision tree above.

#### Scenario: cwd picks the scope on a dual-adoption host

- **GIVEN** a host with `Scope::System` adopted at `/etc/xi/sources` and
  `Scope::User` adopted with `working_dir` at `/home/me/dotfiles`
- **WHEN** `xi deploy` runs from `/home/me/dotfiles` with no `--scope` flag
- **THEN** it resolves `--scope user`

#### Scenario: cwd match does not force a local destination

- **GIVEN** the same host and scope match as above, but the user scope's tree is
  clean and a fleet inventory for `Scope::User` exists
- **WHEN** `xi deploy` runs from `/home/me/dotfiles`
- **THEN** scope resolves to `user`, and the local-vs-fleet decision for that
  scope still follows the ordinary decision tree (rolling out to the fleet in
  this case)

### Requirement: A positional argument is disambiguated without breaking existing meanings

`xi deploy`'s positional arguments MUST be read as follows: anything already
shaped like a path, a URL, an scp spec, or a flake reference with `#` keeps
exactly the meaning `classify_positional` already gives it today; a single bare
word with none of those shapes is checked against the discovered or inventory
target set first, falling through to `classify_positional` unchanged when no
target matches; two or more positionals are always read as a target list.

#### Scenario: A bare word matching a target resolves as a fleet name

- **GIVEN** `web1` is a discovered or inventory target name
- **WHEN** `xi deploy web1` runs
- **THEN** it is read as a fleet target, not a path, even if a directory named
  `web1` exists in `cwd`

#### Scenario: A path-shaped positional is never read as a target

- **GIVEN** the same fleet as above
- **WHEN** `xi deploy ./web1` or `xi deploy github:you/cfg` runs
- **THEN** it is read as an installable/remote exactly as `classify_positional`
  would read it today, never as a fleet target

#### Scenario: `--target` and `--adopt`/`--no-adopt` resolve the rare genuine collision

- **GIVEN** a bare word that is both a valid target name and a valid local
  directory name
- **WHEN** `xi deploy --target <name>` or `xi deploy --no-adopt <name>` runs
- **THEN** the explicit flag's reading wins over the default heuristic

### Requirement: Every auto-detected decision has a forcing flag

`xi deploy` MUST expose a flag for every decision its tree makes automatically:
`--scope {system,user,all}`, `--mechanism
{nixos,darwin,system-manager}`,
`--local`/`--here`, `--target`, `--on`, `--fleet`,
`--adopt`/`--no-adopt`/`--branch`/`--force` (unchanged from `switch`),
`--mode`/`--remote-build`/`--build-strategy`/`--backend`/
`--copy-via`/`--phase`/`--parallel`/`--fail-fast`/`--magic-rollback`/
`--interactive-sudo` (unchanged from `push`), `--force-dirty`/
`--force-working-dir`/`--ask`/`--diff` (unchanged from `switch` and this
effort's earlier changes), and `--dry`/`-n` (new).

#### Scenario: `--fleet` overrides the dirty-routes-local default

- **GIVEN** a dirty tree with no explicit target and an inventory configured
- **WHEN** `xi deploy --force-dirty --fleet` runs
- **THEN** the dirty tree is rolled out to the whole discovered fleet, rather
  than activating locally

#### Scenario: `--dry` previews without dispatching

- **GIVEN** any resolvable combination of scope, mechanism, targets, backend,
  and build strategy
- **WHEN** `xi deploy --dry` runs
- **THEN** it prints the fully resolved plan and exits without building,
  copying, or activating anything

### Requirement: `fleet-digest` and `verified-at` back the tree's decisions

`fleet-digest`, a hash of the last resolved target set, backend, and
build-location decisions, MUST be cached next to `.xi.toml` (not in any scope's
state dir) after a successful fleet rollout, and MUST cause the next rollout to
skip re-evaluation when the inventory and flake outputs are unchanged.
`verified-at`, a state-dir marker written by a local activation that built from
a clean working dir, MUST be available to the decision tree's dirty/clean
boundary.

#### Scenario: An unchanged fleet skips re-evaluation

- **GIVEN** a successful `xi deploy` fleet rollout has cached `fleet-digest`
- **WHEN** a second `xi deploy` runs with the inventory and flake outputs
  unchanged
- **THEN** discovery and selection are skipped; the cached target/backend/
  build-location decisions are reused directly

#### Scenario: A changed fleet re-evaluates

- **GIVEN** the same cached `fleet-digest`
- **WHEN** a target is added to the inventory before the next `xi deploy` runs
- **THEN** discovery and selection re-run, and `fleet-digest` is recomputed and
  re-cached after the rollout

### Requirement: The five superseded verbs deprecate before removal

`os switch`, `home switch`, `darwin switch`, `system switch`, `push`, and the
legacy `deploy` MUST continue to activate exactly as before for a deprecation
window, printing a one-line stderr notice naming `xi deploy` as the replacement
and disappearing from `--help`'s command list during that window. After the
window, each MUST be removed from `XiCommand` entirely.

#### Scenario: A deprecated verb still works, with a notice

- **GIVEN** the deprecation window is active
- **WHEN** `xi system switch` runs
- **THEN** it activates exactly as it did before this change, and prints a
  stderr notice naming `xi deploy`

#### Scenario: A removed verb is an unrecognized command

- **GIVEN** the deprecation window has ended
- **WHEN** `xi system switch` runs
- **THEN** it exits non-zero as an unrecognized subcommand
