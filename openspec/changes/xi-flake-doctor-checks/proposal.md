# Proposal

## Why

`xi doctor` (`crates/xi-flake/src/doctor.rs`) today runs exactly four checks,
all derived from `flake.lock` plus one `nix eval` call:

- `check_nixpkgs_branch` (`doctor.rs:191-223`): warns if the `nixpkgs` input's
  `ref` is outside a built-in or project-configured allowlist.
- `check_input_freshness` (`doctor.rs:226-257`): warns if any input's
  `lastModified` is older than `max_input_age_days` (default 30).
- `check_nixpkgs_owner` (`doctor.rs:260-286`): warns if the `nixpkgs` input's
  owner isn't `NixOS` (gated by `require_official_nixpkgs`).
- `check_formatter` (`doctor.rs:289-316`): warns if `#formatter.<system>`
  doesn't evaluate.

All four only ever look at inputs, never at what the flake actually _outputs_. A
flake can have a pristine `nixpkgs` pin and a declared formatter while still
exposing packages with no `meta.description`, no `meta.license`, a placeholder
`version`, output keys split unevenly across `systems`, or no
`checks`/`formatter` CI surface at all - none of that is visible today.
Separately, `CheckStatus::Fail` (`doctor.rs:41-51`) is defined but never
constructed; every existing check can only ever `Warn`.

This is deliberately scoped to the checks that are cheap to implement on top of
what already exists in this codebase and have real ecosystem precedent
(`statix`/`deadnix`-style linting, DeterminateSystems' `flake-checker`, nixpkgs'
own `meta.*` conventions). It explicitly does **not** chase a "typed flake
outputs" standard: research done alongside this proposal found no ratified,
ecosystem-wide schema for flake outputs. DeterminateSystems' `flake-schemas` (a
`schemas` output + `inventory` function so generic tooling can render custom
outputs) ships only in Determinate Nix; the corresponding upstream PR
([NixOS/nix#8892](https://github.com/NixOS/nix/pull/8892)) has been an open
draft since mid-2024 with unresolved disagreement, and the one known third-party
attempt to consume it for exactly this kind of self-description (`juspay/omnix`
[PR #222](https://github.com/juspay/omnix/pull/222)) was abandoned over
schema/code sync burden. The "non-standard output keys" check in this proposal
therefore stays an informational heuristic against the already-enumerated
allowlist in `crates/xi-core/src/flake_output.rs`
(`FlakeOutput`/`FlakeOutputKind`), not a validation against any external schema.

## What Changes

- **Flake-level structural checks** (static parse of `flake.nix`/
  `flake.lock`/`nix flake metadata --json`, no new eval machinery):
  - Missing top-level flake `description`.
  - `flake.lock` missing, or matched by `.gitignore`.
  - An input pinned with a floating `ref=<branch>` (no `rev=`/tag) even though
    the lock currently holds a rev - flags drift risk on the next
    `nix flake update`, distinct from today's freshness check (which only looks
    at _how old_ the locked rev is, not whether the declared ref is floating).
  - Duplicate logical inputs (e.g. two different `nixpkgs` pulled transitively
    at different revs) not deduped via `inputs.<x>.follows`.
  - Dirty git tree affecting eval, via `nix flake metadata --json`'s
    `dirtyRevision`/`dirtyShortRev`.
  - `nixConfig.extra-substituters` declared without a matching
    `extra-trusted-public-keys` entry - reuses the same substituter/key-pairing
    check already written for cache targets in `xi-binary-cache`
    (`xi cache doctor`), generalized to flake-level `nixConfig`.
- **Derivation metadata checks** (new: walk `packages.<system>.*` via one
  `nix eval --apply <js> --json` call, same shape as
  `discover_leaf_categories`/`discover_single_category` in
  `crates/xi-flake/src/lib.rs:1283-1322`, pulling a `meta` subset per package
  instead of just attribute names):
  - Missing `meta.description`, `meta.license`, `meta.maintainers`,
    `meta.platforms`.
  - Missing/placeholder `version` (absent, or `"unstable"`) compared against
    `pname`/`name`.
  - A package exposed via `apps.<system>.*` whose derivation has more than one
    binary in `outPath/bin` but no `meta.mainProgram`.
  - `meta.insecure` or `meta.knownVulnerabilities` set on a package reachable
    from the flake's default-visible outputs.
- **Output hygiene checks** (new: cross-reference `nix flake show --json` across
  the `systems` axis, reusing `ShowArgs::run`'s existing
  `nix flake show --json` + `serde_json` parsing in
  `crates/xi-flake/src/lib.rs`):
  - Per-system output gaps: a `packages`/`apps`/`devShells`/`checks` key present
    for one system but absent for another declared system.
  - Missing `checks` output entirely.
  - Non-standard top-level output keys, diffed against
    `FlakeOutput`/`FlakeOutputKind` (`crates/xi-core/src/flake_output.rs`).
    Reported as informational (`Warn`, worded "not part of the conventional
    set"), never as a validation failure - see Why above.
- **`CheckStatus::Fail` gets used.** A small number of the new checks that
  indicate an actually-broken state rather than a style nit (`flake.lock`
  missing; dirty tree) use `Fail`; everything else (including all four existing
  checks, unchanged) stays `Warn`. `DoctorArgs::run`'s existing `failures > 0` →
  non-zero exit (`doctor.rs:421-429`) already handles this; no change needed
  there.
- **Check dispatch stays a plain function list for now.** At ~13 new checks
  alongside the existing 4, `run_health_checks` (`doctor.rs:330-381`) grows past
  the point a hand-written push-list is pleasant, but introducing a `Check`
  trait/registry is deferred to a follow-up if the list keeps growing - not
  justified by this change alone (no behavior depends on it; see Open
  Questions).

Explicitly **out of scope** for this change (noted so a later proposal doesn't
have to re-derive why): IFD (import-from-derivation) detection, unpinned
`builtins.fetchTarball`/`fetchurl` hash checks, and
`allowBroken`/`allowUnfree`/`permittedInsecurePackages` baked into flake- level
`nixpkgs.config`. These are real and precedented (`flake-checker`-style) but
need their own design pass on performance (most require an extra eval) and
false-positive rate; bundling them here would blow the scope of a single change.

## Capabilities

### New Capabilities

- `xi-flake-doctor-structure`: static/metadata-only checks (description, lock
  presence, floating refs, duplicate inputs, dirty tree, substituter/key
  pairing).
- `xi-flake-doctor-metadata`: per-package `meta.*`/`version` checks via a new
  eval-based walk.
- `xi-flake-doctor-outputs`: per-system output-gap and non-standard-key checks
  via `nix flake show --json`.

### Related Capabilities (unchanged)

- `xi-binary-cache` (`xi-binary-cache-doctor`): `xi cache doctor` stays the
  separate, cache-target-scoped diagnostic command it already is; this change
  only reuses its substituter/trusted-key pairing logic at the flake-config
  level, it does not merge the two commands.

## Impact

- **Affected code**:
  - `crates/xi-flake/src/doctor.rs`: new check functions following the existing
    `fn(...) -> Option<CheckResult>` / `fn(...) -> CheckResult` pattern, added
    to `run_health_checks`.
  - `crates/xi-flake/src/lib.rs`: new helper alongside
    `discover_leaf_categories`/`discover_single_category` to walk
    `packages.<system>.*` and pull a `meta` subset per package in one
    `nix eval --json` call.
  - `crates/xi-core/src/flake_output.rs`: consumed (not modified) as the
    allowlist for the non-standard-output-key check.
  - `crates/xi-flake/src/project_config.rs`: `ProjectDoctorConfig` gains
    opt-outs for the new check groups (e.g. a project that intentionally ships
    undocumented internal packages can disable the metadata group), following
    the same `[doctor]` section shape `max_input_age_days`/
    `require_official_nixpkgs`/`supported_branches` already use.
- **Coordination**: none outside this repo.
- **Breaking?**: No. All new checks are additive `Warn`/`Fail` entries in the
  printed report; the four existing checks are unchanged. A flake that passed
  `xi doctor` cleanly before may now see new warnings, but exit code only
  changes for flakes hitting one of the two new `Fail` conditions (missing
  `flake.lock`, which already hard-errors today via `bail!` at
  `doctor.rs:337-340` - this change doesn't add a new failure mode there, only
  the dirty-tree check is newly a `Fail`).
- **Migration**: None required; all new checks run by default with an opt-out,
  matching the existing config posture.

## Success Criteria

- **SC-FD-001**: A flake with no top-level `description` produces a `Warn`
  naming that fact; adding one clears it.
- **SC-FD-002**: A `flake.lock` matched by `.gitignore` produces a `Fail` naming
  the file as the problem.
- **SC-FD-003**: An input declared with `ref = "some-branch"` and no `rev`
  produces a `Warn` distinct from the existing freshness check, even when the
  locked rev is recent.
- **SC-FD-004**: Two transitively-pulled inputs resolving to the same logical
  dependency (e.g. `nixpkgs`) at different locked revs, with no `follows`
  connecting them, produce a `Warn` naming both input paths.
- **SC-FD-005**: `nix flake metadata --json` reporting a dirty tree produces a
  `Fail`.
- **SC-FD-006**: `nixConfig.extra-substituters` with no corresponding
  `extra-trusted-public-keys` entry at the same index produces a `Warn`.
- **SC-FD-007**: A package missing `meta.description` produces a `Warn` naming
  that specific package and attribute, not a generic message.
- **SC-FD-008**: A package with `version = "unstable"` (or no `version` at all)
  produces a `Warn`.
- **SC-FD-009**: A package exposed via `apps.<system>.*` with two or more
  binaries in `bin/` and no `meta.mainProgram` produces a `Warn`.
- **SC-FD-010**: `packages.x86_64-linux.foo` present with no
  `packages.aarch64-darwin.foo`, while both systems are declared in `systems`,
  produces a `Warn` naming the specific package and the system it's missing
  from.
- **SC-FD-011**: A flake with no `checks` output at all produces a `Warn`.
- **SC-FD-012**: A top-level output key not present in `FlakeOutput`'s known set
  produces a `Warn` worded as informational ("not part of the conventional
  set"), never phrased as invalid/error.
- **SC-FD-013**: Disabling a check group via `[doctor]` config in `.xi.toml`
  suppresses every check in that group and no others.

## Open Questions

1. The per-package `meta.*` walk needs one `nix eval --json` call across every
   `packages.<system>.*` attribute; for a flake with many packages this is the
   most expensive check group added here. Should it run by default, or
   default-off behind an opt-in flag (`xi doctor --deep` or a
   `[doctor] metadata = true` default-false config key) until real-world timing
   on a representative flake is measured? Leaning toward on-by- default with a
   documented opt-out, since the existing `check_formatter` already pays one
   eval call unconditionally, but worth confirming once task implementation
   lands and can be timed against a flake with O(50+) packages.
2. The duplicate-logical-input check (same input name/repo pulled at different
   revs without `follows`) needs a definition of "same logical input" precise
   enough to avoid false positives - matching on `(owner, repo)` from `locked`,
   as the existing `is_nixpkgs_input` heuristic already does for nixpkgs
   specifically, or something broader? Leaning toward reusing exactly that
   `(owner, repo)` shape rather than inventing a new heuristic, but this needs
   confirming against a flake. lock with genuinely duplicated transitive inputs
   before finalizing.
3. Should the non-standard-output-key check (SC-FD-012) opportunistically read a
   flake's own `schemas`/`exportedSchemas` output, if present, to suppress the
   warning for keys the flake itself documents via DeterminateSystems'
   convention? Per the Why section, this is explicitly deferred rather than
   built now - vendor-specific, unmerged upstream, and the ecosystem's own
   attempt to lean on it was abandoned. Revisit only if `NixOS/nix#8892` merges
   upstream.
