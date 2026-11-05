# Proposal

## Why

`--backend` asks an operator to make, by hand, on every invocation, a
decision the fleet's own flake already fully determines:
`push::discover::detect_backend` derives `deploy-rs` / `colmena` / `builtin`
from exactly which outputs the flake declares. There is no reason that
decision should default to a CLI flag instead of a declaration living next
to the rest of the fleet's definition. Separately, the build-location ladder
(`resolve_remote_build`, `crates/xi-deploy/src/push/config.rs`) only ever
decides *where* a target builds; it has no opinion on *when*, relative to
shipping - every closure-mode push today builds, copies, and activates one
target at a time, with no way to pre-build a fleet ahead of a deploy window
and ship already-built closures later. And its compiled floor (`false`,
build-locally-and-copy) is a one-size default that funnels every build
through the operator's own machine even for a large, genuinely remote fleet,
where distributing the build load to each target already is the ladder's own
top rung - just not the default anyone reaches without configuring it.

## What Changes

- `.xi.toml [push] backend` and a Nix-module `xi.sources.push.backend` option
  join the existing `remote-build` ladder's shape: CLI `--backend` > per-host
  `[[push.hosts]]` override > table-level `[push] backend` > the Nix-module
  default > `detect_backend`'s auto-detect floor. `resolve_remote_build`
  generalizes into one `resolve_ladder<T>` helper, used by this ladder and
  the next one, rather than a third hand-rolled copy of the same precedence.
- A `--build-strategy {interleaved, batched}` flag, default `interleaved`
  (matching every closure-mode push today exactly). `batched` lifts
  `nix.build(&target.installable)` out of `push_one`'s per-target closure
  branch into its own upfront phase over every selected `PushMode::Closure`
  target, producing a `target -> store path` manifest, then ships every
  already-built target from that manifest in a second phase - decoupling the
  CPU-bound build step from the deploy window it used to be interleaved with.
- The build-location ladder's compiled floor flips from `false` (closure,
  operator-built) to a context-sensitive default: `true` (remote-build, the
  target builds itself) for a target that is genuinely remote, trivially
  local (no ladder consulted at all) when the activation target is the
  machine the command is running on. Every existing override (CLI,
  `.xi.toml`, per-host, the Nix-module default) still applies on top of the
  new floor unchanged; only the answer when nothing anywhere is configured
  changes. Ships behind a one-release opt-in (`XI_BUILD_FLOOR_REMOTE=1` or
  equivalent) before becoming the floor outright, because it is the one
  user-visible default change in this effort.

## Capabilities

### New Capabilities

- `xi-deploy-build-config`: makes backend selection and build-location/
  build-timing strategy declarative-first (flake, then CLI override) instead
  of CLI-first, and adds a batched build-then-ship strategy alongside today's
  interleaved one.

### Modified Capabilities

(none - greenfield; the `remote-build` ladder this generalizes is not yet a
living spec under `openspec/specs/`.)

## Impact

- **Affected code**: `crates/xi-deploy/src/push/config.rs`
  (`resolve_remote_build` generalized to `resolve_ladder<T>`, reused for a new
  `resolve_backend` and a new `resolve_build_strategy`), `crates/xi-deploy/src/push/args.rs`
  (`--build-strategy`), `crates/xi-deploy/src/push/orchestrate.rs` (the batched
  build phase and its manifest), the Nix flake-parts module that writes each
  scope's config file (a new `xi.sources.push.backend` option alongside the
  existing `remoteBuild` one).
- **Coordination**: builds on `xi-deploy-unify-activation`'s collapsed
  `detect_backend` as the auto-detect floor for the backend ladder.
  `xi-deploy-unified-verb` exposes `--backend` and `--build-strategy` as part
  of its union flag surface and is the only consumer of the default-floor
  flip's opt-in window.
- **Breaking?**: The build-strategy addition and the declarative backend
  ladder are purely additive. The build-location floor flip IS a breaking
  default change for any fleet relying on the unconfigured
  closure-built-by-operator behavior; it ships behind an explicit opt-in for
  one release before becoming the default, exactly once, and is called out
  as such rather than bundled silently with the additive parts of this
  change.
- **Migration**: An operator who wants to keep today's floor after the flip
  sets `remote-build = false` at the table level in `.xi.toml [push]` (or the
  Nix-module equivalent) once, before the opt-in window ends.

## Success Criteria

- **SC-BC-001**: `xi.sources.push.backend = "colmena"` in the flake resolves
  `colmena` for every push with no `--backend` flag, no `.xi.toml` entry, on
  a fleet whose outputs would otherwise auto-detect `builtin`.
- **SC-BC-002**: Precedence order, highest first: `--backend` on the command
  line, then a per-host `[[push.hosts]]` override, then the table-level
  `.xi.toml [push] backend`, then the flake-parts `xi.sources.push.backend`
  default, then the auto-detect floor - identical in shape to the existing
  `remote-build` ladder's own precedence.
- **SC-BC-003**: `--build-strategy batched` against five `PushMode::Closure`
  targets builds all five before copying or activating any of them, and
  produces a manifest naming all five store paths; a subsequent run
  consuming that manifest activates all five without rebuilding.
- **SC-BC-004**: With no `--build-strategy` flag, behavior is bit-for-bit
  identical to today's interleaved closure-mode push (build, copy, activate,
  per target, in sequence or per `--parallel`).
- **SC-BC-005**: Before the opt-in window ends, an unconfigured push against
  a genuinely remote target still resolves to closure mode; after the
  opt-in flag (or the window itself) is set, the same push resolves to
  remote-build, and a push resolving to the local machine itself never
  consults this ladder at all.
