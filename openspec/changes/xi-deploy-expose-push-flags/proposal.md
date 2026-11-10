# Proposal

## Why

`xi-deploy-unified-verb` built `DeployArgs` as the union of every flag `switch`
and `push` had _at the time it branched_. Four sibling changes landed on
`push::args::PushArgs` after that point - `xi-deploy-build-config`
(`--build-strategy`/`--build-only`/`--from-manifest`), `xi-sources-retry`
(`--retry`), `xi-push-target-pull` (`--sources-transport`), and
`xi-push-resilient-remote-build` (`--reattach`/`--notify`) - and
`crates/xi/src/deploy.rs::push_args()`, the function that builds the `PushArgs`
a fleet rollout dispatches to, was never updated to carry them through. Today it
hardcodes `retry: None`, `build_strategy:
BuildStrategy::Interleaved`,
`build_only: false`, `from_manifest: None`, `sources_transport: None`,
`reattach: None`, `notify: None` - silently discarding whatever an operator
might want from these seven capabilities every time they reach them through
`xi deploy` instead of the deprecated `xi push` directly. The `.xi.toml` ladder
still resolves correctly with no flag given, so this is a forcing-flag gap, not
a routing bug: exactly the kind of gap `xi-deploy-unified-verb/spec.md`'s own
"Every auto-detected decision has a forcing flag" requirement exists to close,
just for flags that did not exist yet when that requirement was written.

## What Changes

- `DeployArgs` (`crates/xi-deploy/src/unified_args.rs`) gains seven fields, one
  per missing `PushArgs` flag, with the same name, type, and default as their
  `PushArgs` counterpart: `sources_transport: Option<SourcesTransport>`,
  `retry: Option<u32>`, `build_strategy: BuildStrategy` (default `Interleaved`),
  `build_only: bool`, `from_manifest: Option<PathBuf>`,
  `reattach: Option<String>`, and `notify: Option<String>`. `sources_transport`,
  `retry`, `build_strategy`, `build_only`, and `from_manifest` take the
  `help_heading = "How it builds"` the module's existing build/ship knobs
  already use; `reattach` and `notify` take `"Safety"`, alongside the existing
  confirmation/preview/rollback/failure-handling flags - a push already in
  flight, and recovering from one, is a safety concern, not a build-location
  one.
- `push_args()` passes all seven straight through from `args` instead of a
  hardcoded placeholder, the same way it already does for `mode` and
  `remote_build`.
- `xi deploy`'s dispatch gains an early, decision-tree-bypassing branch for
  `--reattach`, mirroring `push::orchestrate::run`'s own precedent exactly:
  `xi push --reattach` already checks `args.reattach` before resolving context,
  calling `reattach::run` directly against the scope's state dir with no
  fact-gathering or decision-tree involvement, because a reattach is about a
  push already in flight, not a new activation decision. `run_for_scope` in
  `crates/xi/src/deploy.rs` needs the same early check, or `--reattach` would
  get funneled through `gather_facts`/`resolve_plan` and only reach
  `push::orchestrate::run`'s own check if the tree happens to resolve to
  `RolloutFleet` - which it has no reason to for a reattach.
- No change to `PushArgs`, `push::orchestrate`, or the ladders themselves: every
  one of these seven flags' resolution semantics already exists and is already
  tested; this change only re-exposes them on the newer verb.

## Capabilities

### New Capabilities

- `xi-deploy-expose-push-flags`: `DeployArgs`'s flag surface and `push_args()`'s
  conversion gain the seven flags enumerated above; `xi
  deploy --reattach`
  dispatches the same way `xi push --reattach` does, bypassing the decision
  tree. (Modifies `xi-deploy-unified-verb`'s own `DeployArgs`/`push_args` in
  practice, but that change has not archived into `openspec/specs/` yet, so
  there is no living spec delta to modify against - this lands as its own
  capability instead.)

## Impact

- **Affected code**: `crates/xi-deploy/src/unified_args.rs` (seven new
  `DeployArgs` fields), `crates/xi/src/deploy.rs` (`push_args()`'s conversion,
  and `run_for_scope`'s/`run`'s new `--reattach` early branch).
- **Coordination**: none - this only re-exposes flags and dispatch behavior that
  already exist and are already tested on `push::args::PushArgs` and
  `push::orchestrate::run`; it does not change their resolution semantics.
- **Breaking?**: No. Every new field defaults to the same "let the ladder
  decide" behavior `xi deploy` already has today; nothing about the existing,
  already-shipped flag surface changes.
- **Migration**: None needed. An operator who was already dropping down to
  `xi push --build-strategy batched --build-only`, `xi push --retry 0`,
  `xi
  push --sources-transport target-pull`, or
  `xi push --reattach`/`--notify` because `xi deploy` lacked the flag can use
  `xi deploy` for all of them once this lands.

## Success Criteria

- **SC-EPF-001**: `xi deploy --sources-transport target-pull` on a fleet rollout
  resolves `PushMode::TargetPull` for every target with no per-host override,
  the same way `xi push --sources-transport target-pull` already does.
- **SC-EPF-002**: `xi deploy --retry 0` on a fleet rollout disables retrying
  entirely for every target, the same way `xi push --retry 0` already does.
- **SC-EPF-003**: `xi deploy --build-strategy batched --build-only` builds every
  selected target upfront and writes a manifest, without copying or activating
  anything; a later `xi deploy --from-manifest <path>` ships from it without
  rebuilding - identical to the `xi push` equivalents.
- **SC-EPF-004**: `xi deploy --reattach` with no id lists every local push
  receipt and probes each target's reachability, without first resolving the
  decision tree for any scope - identical to `xi push --reattach`.
- **SC-EPF-005**: `xi deploy --notify <command>` on a fleet rollout fires
  `<command>` once a terminal outcome for the push is observed, identical to
  `xi push --notify <command>`.
- **SC-EPF-006**: `xi deploy --build-only --from-manifest <path>` (both given)
  and `xi deploy --build-only` with `--build-strategy` omitted or `interleaved`
  both refuse with the same validation errors `xi push` already gives for the
  same combinations - `PushArgs::validate()` is reached unchanged, not
  reimplemented.
