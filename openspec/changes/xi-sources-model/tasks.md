# Tasks: xi-sources-model

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads under label `change:xi-sources-model`.

## 1. Specification (P1)

- [x] 1.1 Land the spec delta under `specs/xi-sources-model/spec.md`.
- [x] 1.2 Cross-link with `xi-sources-config` (where the model gets its inputs),
      `xi-sources-conflict` (ref-lineage transitions), and `xi-sources-gitops`
      (who writes `remote_head`).

## 2. Data types (P1)

- [x] 2.1 Add `Scope { System, User }` and
      `SourcesId { scope, canonical_url
      }` to `xi-core` (model is shared
      across every verb crate).
- [x] 2.2 Add
      `SourcesContext { sources_dir, state_dir, working_dir?,
      sources_id, refs: RefLineage }`
      and a loader that reads the state dir.
- [x] 2.3 Define
      `RefLineage { remote_head: Option<Sha>, working_head:
      Option<Sha>, sources_head: Option<Sha>, active_generation:
      Option<GenerationId> }`.
      All four are optional - any can be missing on a fresh install.

## 3. Scope resolver (P1)

- [x] 3.1 Each `switch`-style command declares its scope at parse time via
      `fn scope() -> Scope`.
- [x] 3.2 `xi switch` (verb-less) is a parse error; print the four concrete
      verbs.

## 4. State dir loader (P1)

- [x] 4.1 On load, the state dir layout is a flat set of named files
      (`remote-head`, `working-head`, `sources-head`, `working-dir`,
      `active-generation`, `lock`). Each file is readable only by the owning
      user. Missing files mean "unknown", not an error.
- [x] 4.2 Document the actual paths in the spec; defer concrete OS layout to
      `xi-sources-config` (which owns the path tables).

## 5. Machine surface (P1)

- [x] 5.1 Add `xi sources status` as a JSON command returning the house response
      envelope. Status mapping: no sources dir → `Missing`; state dir missing
      files but sources dir exists → `Pending`; all refs known and
      `active_generation` present → `Ready`; refs known but last gitops tick
      failed → `Degraded`.
- [x] 5.2 Return the same envelope on stderr-free `stdout`; keep the existing xi
      stdout/stderr split.

## 6. Tests (P1)

- [x] 6.1 Unit test the scope-resolver mapping (one case per verb).
- [x] 6.2 Unit test `SourcesContext::load` across the four envelope statuses
      using a tempdir with simulated state dirs.
- [x] 6.3 BDD test `bdd_sources_model.rs`:
      `given a configured sources dir,
      when I run xi sources status, then I get Ready with the expected
      refs`.

## 7. Follow-ups (P4 - tracked but out of scope)

- [ ] 7.1 Multi-remote sources (one state dir pointing at N upstreams for
      failover). Deferred.
- [ ] 7.2 Multi-user system-scope sources on the same host. Deferred - first cut
      is one system-scope sources per host.

## Implementation notes

### Scope table vs the shipped CLI (resolved drift)

The scope table in the spec names `xi nixos switch`, but the binary's subcommand
has always been `xi os`. Rather than weaken the spec, the `Os` variant gained
`#[command(alias = "nixos")]`, so both spellings resolve and the table is
literally true. `os` remains the primary name in `--help`.

Pinned by `crates/xi/tests/bdd_sources_scope.rs`, which asserts each command's
`scope()` against `Scope::for_switch_verb` rather than against a second copy of
the table.

### Provisional path resolution (task 4.2)

`xi-sources-config` owns the real per-scope path tables. Until it lands,
`crates/xi/src/sources/mod.rs::resolve_dirs` derives both directories from
`xi_core::dirs::xdg_state_dir()/sources/<sources-id-slug>/{sources,state}`. The
shape of the answer is already correct; only the prefix moves when config lands.
`SourcesId::slug()` is the stable leaf component config should build its tables
from.

### Gitops health is an input, not a reading

The status mapping depends on whether the last gitops tick failed, but the state
dir's file set is fixed by spec and has no slot for it. `GitopsHealth` is
therefore passed into `SourcesContext::status`, and `xi-sources-gitops` owns
producing it. Today the CLI always passes `Disabled`.

### Degraded outranks Pending

The spec lists both without stating precedence. `status()` checks the three
`Degraded` causes first: a stale pointer, a failed tick, or junk in the state
dir is actionable now, whereas "never switched" resolves itself on the next
switch.
