# Capability: xi-flake-doctor-checks

## ADDED Requirements

### Requirement: Doctor flags a missing flake description

`xi doctor` MUST check whether the flake declares a top-level
`description` attribute and emit a `Warn` naming its absence when it does
not.

#### Scenario: Flake without a description warns

- **GIVEN** a flake whose `flake.nix` has no top-level `description`
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming the missing `description`

#### Scenario: Flake with a description passes

- **GIVEN** a flake whose `flake.nix` declares `description = "..."`
- **WHEN** `xi doctor` runs
- **THEN** no warning is reported for this check

### Requirement: Doctor flags a missing or ignored flake.lock

`xi doctor` MUST check that `flake.lock` exists and is not matched by the
repository's `.gitignore`, emitting a `Fail` naming the specific problem
when either condition holds.

#### Scenario: Gitignored flake.lock fails

- **GIVEN** a `flake.lock` present on disk but matched by `.gitignore`
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Fail` naming `flake.lock` as ignored

### Requirement: Doctor flags floating input refs

`xi doctor` MUST check each direct input's declared (`original`) ref in
`flake.lock` and emit a `Warn` when that ref names a branch (no `rev=` or
tag) rather than an immutable reference, independent of how recently the
currently-locked revision was updated.

#### Scenario: Branch-pinned input warns even when freshly locked

- **GIVEN** an input declared with `ref = "main"` and no `rev`, whose
  currently-locked revision is less than a day old
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming the input as using a floating ref,
  separate from the existing input-freshness check

#### Scenario: Tag- or rev-pinned input passes

- **GIVEN** an input declared with an explicit `rev` or tag
- **WHEN** `xi doctor` runs
- **THEN** no floating-ref warning is reported for that input

### Requirement: Doctor flags un-followed duplicate inputs

`xi doctor` MUST detect when two or more nodes in `flake.lock` resolve to
the same logical input (same `(owner, repo)` pair in their `locked`
metadata) at different locked revisions, with no `follows` relationship
connecting them, and emit a `Warn` naming all affected input paths.

#### Scenario: Divergent transitive nixpkgs warns

- **GIVEN** a `flake.lock` where the root's `nixpkgs` and a transitive
  dependency's `nixpkgs` lock to different revisions of
  `github:NixOS/nixpkgs`, with no `follows` between them
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming both input paths and the divergent
  revisions

#### Scenario: Followed input does not warn

- **GIVEN** the same setup, but the transitive dependency's `nixpkgs`
  input declares `follows = "nixpkgs"`
- **WHEN** `xi doctor` runs
- **THEN** no duplicate-input warning is reported

### Requirement: Doctor flags a dirty evaluation tree

`xi doctor` MUST check `nix flake metadata --json`'s dirty-tree indicator
and emit a `Fail` when the flake is being evaluated from an unclean git
tree.

#### Scenario: Dirty tree fails

- **GIVEN** `nix flake metadata --json` reports a dirty revision
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Fail` naming the dirty working tree as the
  problem

### Requirement: Doctor flags unpaired flake-level substituters

`xi doctor` MUST check that every entry in a flake's
`nixConfig.extra-substituters` has a corresponding entry (by index) in
`nixConfig.extra-trusted-public-keys`, emitting a `Warn` naming the
unpaired substituter when it does not.

#### Scenario: Substituter without a trusted key warns

- **GIVEN** `nixConfig.extra-substituters = [ "https://example.cachix.org" ]`
  and no corresponding `extra-trusted-public-keys` entry
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming that substituter as untrusted

### Requirement: Doctor flags packages missing core meta attributes

`xi doctor` MUST evaluate `meta.description`, `meta.license`,
`meta.maintainers`, and `meta.platforms` for every package under
`packages.<system>.*` and emit a `Warn` per package per missing
attribute, naming both the package and the specific missing attribute.

#### Scenario: Package missing description warns

- **GIVEN** `packages.<system>.foo` whose derivation has no
  `meta.description`
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming `packages.<system>.foo` and
  `meta.description` specifically

#### Scenario: Fully-described package passes

- **GIVEN** a package with `meta.description`, `meta.license`,
  `meta.maintainers`, and `meta.platforms` all set
- **WHEN** `xi doctor` runs
- **THEN** no missing-metadata warning is reported for that package

### Requirement: Doctor flags missing or placeholder versions

`xi doctor` MUST check each package's `version` (or `pname`/`version`
pair) and emit a `Warn` when it is absent or equal to the literal
placeholder `"unstable"`.

#### Scenario: Unversioned package warns

- **GIVEN** `packages.<system>.foo` with no `version` attribute
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming the missing version

### Requirement: Doctor flags multi-binary apps missing mainProgram

`xi doctor` MUST check, for each package exposed via `apps.<system>.*`,
whether its derivation's `bin/` output contains more than one executable
and `meta.mainProgram` is unset, emitting a `Warn` when both hold.

#### Scenario: Ambiguous multi-binary app warns

- **GIVEN** a package exposed via `apps.<system>.foo` whose `bin/`
  contains two executables and no `meta.mainProgram`
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming the app and its ambiguous binaries

### Requirement: Doctor flags insecure packages reachable from default outputs

`xi doctor` MUST check whether any package reachable from the flake's
default-visible outputs sets `meta.insecure` or
`meta.knownVulnerabilities`, emitting a `Warn` naming the package when it
does.

#### Scenario: Insecure package warns

- **GIVEN** `packages.<system>.foo` with `meta.insecure = true`
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming `packages.<system>.foo` as insecure

### Requirement: Doctor flags per-system output gaps

`xi doctor` MUST compare the output keys present under each of the
flake's declared `systems` for `packages`, `apps`, `devShells`, and
`checks`, emitting a `Warn` naming the specific key and the system it is
missing from when a key present for one declared system is absent for
another.

#### Scenario: Package missing on one system warns

- **GIVEN** `packages.x86_64-linux.foo` is defined and
  `packages.aarch64-darwin.foo` is not, with both systems declared in
  `systems`
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming `foo` and `aarch64-darwin`
  specifically

### Requirement: Doctor flags a missing checks output

`xi doctor` MUST check whether the flake declares a `checks` output at
all, emitting a `Warn` when it is entirely absent.

#### Scenario: No checks output warns

- **GIVEN** a flake with no `checks` output for any system
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` naming the missing `checks` output

### Requirement: Doctor flags non-standard top-level output keys as informational

`xi doctor` MUST diff a flake's top-level output keys against the known
set in `FlakeOutput`/`FlakeOutputKind`
(`crates/xi-core/src/flake_output.rs`) and emit a `Warn`, worded as
informational ("not part of the conventional set"), for any key outside
that set. This check MUST NOT be worded or treated as a validation
failure against any external or ratified schema, and MUST NOT consult
any vendor-specific schema mechanism (e.g. a flake's own `schemas` output)
as of this change.

#### Scenario: Custom output key is reported informationally

- **GIVEN** a flake with a top-level `ociImages` output not present in
  `FlakeOutput`'s known set
- **WHEN** `xi doctor` runs
- **THEN** it reports a `Warn` describing `ociImages` as outside the
  conventional set, not as invalid or an error

### Requirement: Doctor check groups are individually configurable

`.xi.toml`'s `[doctor]` section MUST allow disabling each new check group
(structural, metadata, output-hygiene) independently, following the same
configuration shape already used by `max_input_age_days`,
`require_official_nixpkgs`, and `supported_branches`.

#### Scenario: Disabling the metadata group suppresses only its checks

- **GIVEN** `.xi.toml` disables the metadata check group, and the flake
  has both a package missing `meta.description` and a missing top-level
  `description`
- **WHEN** `xi doctor` runs
- **THEN** the missing-`meta.description` warning is absent, and the
  missing-top-level-`description` warning still appears
