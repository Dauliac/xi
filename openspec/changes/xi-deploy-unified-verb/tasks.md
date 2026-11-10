# Tasks

Live state in beads under `change:xi-deploy-unified-verb`. Depends on
`xi-deploy-unify-activation`, `xi-deploy-mechanism-detection`, and
`xi-deploy-build-config` all landing first.

## 1. Positional disambiguation

- [ ] 1.1 Implement the bare-word-checks-targets-first rule on top of
      `classify_positional`, as a new function called before it rather than a
      modification to it; unit test every existing `classify_positional` case is
      unaffected when no target matches.
- [ ] 1.2 Unit test: a bare word matching a discovered/inventory target resolves
      as a target even when a same-named local directory exists.
- [ ] 1.3 Add `--target` (forces the target reading) and verify it overrides the
      heuristic in the ambiguous-collision case.

## 2. The decision tree

- [ ] 2.1 Implement the cwd-independent tree (target/inventory-membership/
      adoption/dirty/fleet-existence/digest, in that order) as a pure function
      over already-resolved facts, so it is unit-testable without a filesystem
      or network.
- [ ] 2.2 Implement the cwd-to-scope disambiguation as a separate, earlier step
      feeding `--scope` resolution, not a branch inside the tree itself; unit
      test it resolves the right scope on a dual-adoption fixture and leaves the
      tree's own local-vs-fleet outcome unaffected.
- [ ] 2.3 Add `--local`/`--here` and `--fleet` as explicit overrides of the
      tree's own outcome; unit test both against a dirty-tree-with-inventory
      fixture (the one case where they disagree with the default).
- [ ] 2.4 BDD: the full tree against fixtures for every leaf in
      `xi-deploy-unified-verb/spec.md`'s first requirement, including the
      standalone-host-from-anywhere regression this design corrected.

## 3. Flag surface

- [ ] 3.1 Define `DeployArgs` as the union of `SwitchSourcesArgs`,
      `CommonRebuildArgs`, and `PushArgs`, plus `--scope`, `--mechanism`,
      `--local`/`--here`, `--target`, `--fleet`, and `--dry`/`-n`; verify every
      existing `switch`/`push` flag parses identically under the new struct via
      a parser-only test (no dispatch).
- [ ] 3.2 Group the flag surface under `help_heading`s ("Where", "How it
      builds", "Safety") and verify `--help`'s rendered output reads as distinct
      sections.
- [ ] 3.3 Implement `--dry`: resolve the full plan (scope, mechanism, targets,
      backend, build strategy) and print it without dispatching; unit test
      against a fixture that would otherwise dispatch, asserting no
      build/copy/activate call happens.

## 4. `fleet-digest` and `verified-at`

- [ ] 4.1 Implement `fleet-digest`'s cache file next to `.xi.toml` (not a scope
      state dir) and its hash input (resolved target set, backend,
      build-location decisions); unit test cache-hit and cache-miss paths.
- [ ] 4.2 Wire a successful fleet rollout to write `fleet-digest` and a
      successful clean-tree local activation to write `verified-at`; BDD test
      both writes against fixture transports.
- [ ] 4.3 Wire the decision tree's fleet-existence/digest step to read
      `fleet-digest` and skip re-evaluation on a hit; BDD test a changed
      inventory invalidates the cached digest and forces re-evaluation.

## 5. Dispatch and `XiCommand` surface

- [ ] 5.1 Add `Deploy(DeployArgs)` to `XiCommand`, dispatching internally to the
      same `switch::prepare`/`record_activation_at` and
      `push::orchestrate::execute` code paths the existing verbs call, chosen by
      the tree from step 2.
- [ ] 5.2 Mark `Os`, `Home`, `Darwin`, `System`'s `Switch` subcommand, `Push`,
      and the legacy `Deploy` variants deprecated: keep them fully functional,
      hide them from `--help` (the same `#[command(hide = true)]` precedent the
      bare `Switch` rejection already uses), and print a one-line stderr notice
      naming `xi deploy` on each invocation.
- [ ] 5.3 BDD: each deprecated verb still activates correctly and emits the
      notice; `--help`'s command list does not show any of them.
- [ ] 5.4 After the deprecation window (tracked separately, not blocking this
      change's own completion): remove the deprecated `XiCommand` variants
      entirely; verify `xi system switch` exits as an unrecognized subcommand.

## Workflow follow-up

- Do not start task group 5.4 until the deprecation window this change's
  proposal names has actually elapsed in a shipped release.
- Archive this change once group 5.4 lands; at that point every verb this effort
  set out to replace is gone.
