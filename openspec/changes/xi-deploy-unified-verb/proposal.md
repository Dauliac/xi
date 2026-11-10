# Proposal

## Why

xi has five verbs today that all mutate a running generation - `os switch`,
`home switch`, `darwin switch`, `system switch`, and `push` - plus a sixth,
legacy `deploy`, that overlaps with `push` for two of its three backends. No
command decides between them; an operator has to already know which scope, which
mechanism, and whether this is a local change or a fleet rollout before typing
anything. `xi-deploy-unify-activation` makes all five routes converge on the
same activation entrypoint and state; `xi-deploy-mechanism-detection` makes the
mechanism and scope answerable from host state and sources;
`xi-deploy-build-config` makes backend and build-location answerable from the
flake. Once all three are true, the five verbs are five ways of saying things
`xi deploy` can read for itself - with a forcing flag for every one of them, for
the operator or the script that knows better than the heuristic.

## What Changes

- `os switch`, `home switch`, `darwin switch`, `system switch`, and `push` are
  removed as CLI surfaces. `xi deploy` is the one command that replaces all
  five, dispatching internally to the exact same code they called. `build`,
  `sources`, `status`, and `uninstall` are untouched - they inspect or manage,
  they don't activate.
- The decision tree `xi deploy` resolves, in order: an explicit target or
  `--target` always wins; a host that is itself a registered fleet member
  (machine-id in the inventory) always activates locally; a scope with no
  adopted source at all refuses, naming `--adopt`; a dirty tree activates
  locally first; a clean, adopted, non-fleet-member host with no inventory or
  flake-deploy outputs at all activates locally (it has nothing to fan out to);
  otherwise it rolls out to the fleet, skipping re-evaluation when
  `fleet-digest` is unchanged since the last rollout. None of this branches on
  `cwd` - a standalone, already-adopted host with no fleet configured must
  resolve to a local activation no matter where the command runs from.
- `cwd` DOES matter for one narrower question Fig. 3's tree does not answer:
  which scope, on a host where more than one is adopted (a system config at
  `/etc/xi/sources` and a personal one at `~/dotfiles` are both real,
  independent adoptions). `cwd` matching a scope's configured `working_dir`
  disambiguates which scope `--scope` left unresolved; it does not change
  whether that scope's own tree resolves to a local activation or a fleet
  rollout.
- A positional argument is disambiguated between "a fleet target name" and "a
  path or remote to build/adopt" by a narrow, additive rule on top of
  `classify_positional`'s existing behavior: anything already shaped like a
  path, URL, or scp spec keeps meaning exactly what it means today; a bare word
  with none of those shapes is checked against the discovered/inventory target
  set first, falling through to `classify_positional` unchanged on no match; two
  or more positionals are always a target list.
- The full flag surface is the union of what `switch` and `push` already have,
  plus `--scope`, `--mechanism`, `--local`/`--here`, `--target`, `--fleet`, and
  `--dry`/`-n` (closing the gap that `push` has no preview of its own today).
  `--fleet` forces a whole-fleet rollout even when nothing else would route
  there (a dirty tree with no named target, for instance) - a deliberate
  tightening over today's bare `xi push --force-dirty`, which ships to every
  discovered host with no explicit acknowledgment of which.
- Two new operator-side caches support the tree's own decisions: `fleet-digest`
  (a hash of the last resolved target set, backend, and build-location
  decisions, stored next to `.xi.toml`, not in any scope's state dir) lets a
  repeat rollout skip re-evaluation when nothing in the inventory or flake has
  changed; `verified-at` (written by a local activation that built from a clean
  working dir) lets the tree treat a just-verified commit differently from an
  untested one at the dirty/clean boundary.
- `os`, `home`, `darwin`, and `system switch`, `push`, and the legacy `deploy`
  are deprecated with a visible "use `xi deploy` instead" notice for a release
  or two, then removed from `XiCommand` outright. This is the one step in this
  whole effort with no way back, and it does not ship until every one of
  `xi-deploy-unify-activation`, `xi-deploy-mechanism-detection`, and
  `xi-deploy-build-config` has landed and proven itself.

## Capabilities

### New Capabilities

- `xi-deploy-unified-verb`: the single `xi deploy` command - its decision tree,
  its positional-disambiguation rule, its forcing-flag surface, and the two new
  caches that back its decisions - that replaces `os switch`, `home switch`,
  `darwin switch`, `system switch`, `push`, and the legacy `deploy` as the only
  CLI surface for activation.

### Modified Capabilities

(none - greenfield; none of the five verbs this supersedes are yet living specs
under `openspec/specs/`.)

## Impact

- **Affected code**: `crates/xi/src/interface.rs` (`XiCommand`'s `Os`, `Home`,
  `Darwin`, `System`'s `Switch` subcommand, `Push`, and `Deploy` variants
  replaced by one `Deploy(DeployArgs)` variant dispatching internally),
  `crates/xi-core/src/sources/args.rs` (`classify_positional`'s fleet-name-first
  bare-word check), a new operator-side cache file next to `.xi.toml` for
  `fleet-digest`, a new state-dir marker `verified-at` alongside
  `xi-deploy-unify-activation`'s `applied-origin`/`inventory-role`.
- **Coordination**: depends on `xi-deploy-unify-activation` (the entrypoint
  every leaf of the decision tree dispatches to),
  `xi-deploy-mechanism-detection` (the `--mechanism`/`--scope` auto-detection
  this tree's first step reads), and `xi-deploy-build-config` (the
  backend/build-strategy resolution a fleet rollout leaf reads). Supersedes, in
  full, the CLI-verb shape proposed by the still-open `xi-sources-push` change.
- **Breaking?**: Yes, in the final step only (removing the five old verbs and
  the legacy `deploy`). Everything else - the new `xi deploy` verb existing
  alongside the old ones, the caches, the disambiguation rule - is additive and
  ships first, with the old verbs deprecated but functional for a release or two
  before removal.
- **Migration**: A script or runbook calling `xi push`, `xi <scope> switch`, or
  `xi deploy` with the legacy flake-ref shape rewrites to the equivalent
  `xi deploy` invocation (§5.10/§5.12 of the design memo this change implements
  enumerate the mapping) before the deprecated verbs are removed.

## Success Criteria

- **SC-UV-001**: `xi deploy` with no arguments, run on an already-adopted,
  clean, non-fleet-member host with an inventory configured, rolls out to the
  fleet; the same command on an otherwise-identical host with no inventory and
  no flake-deploy outputs at all activates locally.
- **SC-UV-002**: `xi deploy` run from an unrelated directory (not the scope's
  `working_dir`, not any checkout at all) on an already-adopted standalone host
  still activates locally - `cwd` does not gate this decision.
- **SC-UV-003**: On a host with both a system-scope and a user-scope adoption,
  `cwd` matching the user scope's `working_dir` resolves `--scope
  user` with
  no explicit flag; `cwd` matching neither leaves scope resolution to
  `SystemDeploymentKind::detect()` and recorded-source presence.
- **SC-UV-004**: `xi deploy web1` always resolves `web1` as a fleet target when
  `web1` is a discovered or inventory name, regardless of whether a same-named
  directory exists in `cwd`; `xi deploy ./web1` always resolves as a local path.
- **SC-UV-005**: `xi deploy --force-dirty` with no named target and no `--fleet`
  activates locally even when an inventory exists;
  `xi deploy
  --force-dirty --fleet` rolls the dirty tree out to the whole
  discovered fleet.
- **SC-UV-006**: `xi deploy --dry` prints the resolved scope, mechanism, target
  set, backend, and build strategy, and dispatches nothing.
- **SC-UV-007**: A second `xi deploy` fleet rollout with nothing changed in the
  inventory or flake since the first skips re-evaluation (`fleet-digest`
  unchanged); a third rollout after the flake's target set changes re-evaluates.
- **SC-UV-008**: During the deprecation window, `xi system switch` still
  activates exactly as before, but prints a one-line stderr notice naming
  `xi deploy` as its replacement, and is removed from `--help`'s command list.
  After the window, `xi system switch` is removed from `XiCommand` entirely and
  exits non-zero as an unrecognized command.
