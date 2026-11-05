# Proposal

## Why

Three code paths can activate a generation today - an interactive
`xi <scope> switch`, the gitops daemon's tick, and an inbound `xi push` - and
only two of them go through the same entrypoint. `xi push` activates a target
over raw SSH (`nix-env --profile ... --set` + `switch-to-configuration switch`
for `--mode closure`; a bare `nixos-rebuild switch --flake ...` for
`--mode sources`), so it never takes the target's `SourcesLock`, never runs the
`machineId` guard, and never calls `record_activation_at`. The target's own
`xi <scope> status` goes stale the moment a push lands, even though
`lock.rs`'s own module doc already names "an inbound `xi push` naming this host
as the target" as one of the three actors the lock exists to serialize.

Fixing that is the precondition for every later change in this effort
(`xi-deploy-mechanism-detection`, `xi-deploy-build-config`,
`xi-deploy-unified-verb`): they all assume a target's state dir is
authoritative regardless of what triggered the activation, which is only true
once this change lands.

## What Changes

- Push's two build modes (`--mode closure`, `--mode sources`) stop running a
  raw activation script over SSH. Each ends, instead, with a remote invocation
  of the same local activation entrypoint `xi <scope> switch` already is,
  pointed at the copied closure or the synced sources dir, tagged with an
  `XI_APPLIED_ORIGIN=push:<operator-host>` environment variable.
- `crates/xi-core/src/sources/lock.rs`'s `ActorName` gains a `Push` variant,
  lowest priority, so an inbound push contends for the real `SourcesLock`
  exactly as the module doc already describes, instead of never acquiring it.
- `record_activation_at` gains two new state-dir markers written on every
  activation, local or pushed: `applied-origin` (`local` | `daemon` |
  `push:<host>`, read from `XI_APPLIED_ORIGIN`, falling back to the existing
  `applied-via` inference when unset) and `inventory-role` (this host's own
  `[[push.hosts]]` name, when the triggering push passed one).
- `push::orchestrate::delegate`'s deploy-rs/colmena delegation and
  `xi_deploy::detect_and_deploy` (`crates/xi-deploy/src/lib.rs`, today's
  `xi deploy` verb) stop being two independently maintained call sites for the
  same two backends and become one shared dispatch path, called by both. The
  `xi deploy` CLI verb itself is untouched by this change - still the same
  flake-ref, no-sources-model command it is today. `xi-deploy-unified-verb` is
  the change that reclaims the `deploy` name for the new unified command and
  retires this one; this change only removes the duplication underneath it.
- `xi <scope> switch`'s case 3 (an explicit installable on the command line,
  e.g. `xi system switch .` or `xi system switch ./experiments/fork`) gains a
  working-dir mismatch guard: when the positional resolves to a local path and
  this scope has a configured `working_dir` that canonicalizes to a different
  path, warn and ask (`inquire::Confirm`, the same shape `--ask` already uses)
  before building from it. A new `--force-working-dir` flag answers
  non-interactively; with no TTY, the guard refuses rather than guessing.
- `discover::detect_backend` (target-aware, in `push::discover`) and
  `xi-deploy::detect_backend` (flake-probing, no target list, used by today's
  `xi deploy` verb) collapse into one function: the flake-probing version
  becomes a thin wrapper that discovers targets first and calls the
  target-aware one. One backend precedence table, read in one place, shaped so
  `xi-deploy-build-config`'s declarative backend and build-strategy ladders
  can reuse it without a third hand-rolled copy of the same
  CLI-over-config-over-floor precedence.

## Capabilities

### New Capabilities

- `xi-deploy-unify-activation`: a single activation entrypoint
  (`xi <scope> switch`, called internally regardless of trigger) that every
  one of local-interactive, gitops-daemon, and inbound-push activation paths
  runs through, with the lock, the conflict policy, and the state-dir markers
  it writes behaving identically no matter which of the three triggered it.

### Modified Capabilities

(none - `xi-sources-push`, `xi-sources-model`, and `xi-sources-race` are still
unarchived changes, not yet living specs under `openspec/specs/`; see Impact.)

## Impact

- **Affected code**: `crates/xi-deploy/src/push/{orchestrate,mode_sources}.rs`
  (raw SSH scripts replaced by a remote `xi <scope> switch` invocation),
  `crates/xi-core/src/sources/lock.rs` (`ActorName::Push`),
  `crates/xi-core/src/sources/switch.rs` (`record_activation_at`'s new
  markers, the case-3 working-dir guard), `crates/xi-deploy/src/push/discover.rs`
  and `crates/xi-deploy/src/lib.rs` (backend detection collapsed to one
  function, shared by `xi push` and the still-unchanged `xi deploy` verb).
- **Coordination**: this change's target-side activation step is what
  `xi-deploy-mechanism-detection` and `xi-deploy-build-config` dispatch
  through once their own detection/ladder logic has picked a mechanism and a
  build location; `xi-deploy-unified-verb`'s single `xi deploy` verb is only
  safe to ship once this change's state-dir markers are authoritative for
  every trigger. Supersedes the CLI-verb shape proposed by the still-open
  `xi-sources-push` change for the `builtin` backend's activation step only -
  that change's preflight, secrets, parallelism, and magic-rollback machinery
  is unaffected and reused as-is.
- **Breaking?**: No observable behavior change for an operator calling
  `xi push` or `xi <scope> switch` today; the new markers are additive, and
  the working-dir guard only fires on a case that was previously silent.
- **Migration**: None.

## Success Criteria

- **SC-UA-001**: `xi push` against a target running the `builtin` backend
  results in that target's `xi <scope> status` reporting the pushed
  generation's `active-generation`, `applied-via`, `applied-at`, and
  `sources-head` - all four, not stale.
- **SC-UA-002**: The same push's target-side activation acquires
  `SourcesLock` under `ActorName::Push`; a concurrent gitops-daemon tick on
  that target queues behind it rather than racing it.
- **SC-UA-003**: `xi <scope> status --json` on a pushed target reports
  `applied-origin: "push:<operator-hostname>"`; on a daemon-ticked target it
  reports `"daemon"`; on an interactive switch it reports `"local"`.
- **SC-UA-004**: `xi system switch ./experiments/fork` on a host whose scope
  has a different configured `working_dir` prints the mismatch warning and
  prompts; `xi system switch ./experiments/fork --force-working-dir` proceeds
  without prompting; the same command with no TTY attached refuses.
- **SC-UA-005**: `xi push --backend colmena` and `xi deploy --backend colmena`
  resolve the same backend for the same target set, because both read the one
  collapsed `detect_backend`.
