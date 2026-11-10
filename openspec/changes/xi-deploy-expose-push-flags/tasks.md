# Tasks

Live state in beads under `change:xi-deploy-expose-push-flags`. Depends on
`xi-deploy-unified-verb`, `xi-deploy-build-config`, `xi-sources-retry`,
`xi-push-target-pull`, and `xi-push-resilient-remote-build` all having landed
already (they have: PRs #2-#8, all merged).

## 1. Flag surface

- [ ] 1.1 Add `sources_transport: Option<SourcesTransport>`,
      `retry:
      Option<u32>`, `build_strategy: BuildStrategy` (default
      `Interleaved`), `build_only: bool`, and
      `from_manifest:
      Option<PathBuf>` to `DeployArgs`
      (`crates/xi-deploy/src/unified_args.rs`), each under
      `help_heading = "How it builds"`, matching their `PushArgs` counterpart's
      clap attrs, default, and doc comment exactly; unit test every existing
      `DeployArgs` parser test still passes, plus one new test per added flag
      parsing both given and omitted.
- [ ] 1.2 Add `reattach: Option<String>` and `notify: Option<String>` to
      `DeployArgs`, under `help_heading = "Safety"`, matching their `PushArgs`
      counterpart's clap attrs and doc comment exactly; unit test both parse the
      same way `PushArgs`'s own do (including `--reattach`'s
      `num_args = 0..=1, default_missing_value = ""` shape).

## 2. Wiring

- [ ] 2.1 Update `push_args()` (`crates/xi/src/deploy.rs`) to pass
      `sources_transport`, `retry`, `build_strategy`, `build_only`,
      `from_manifest`, `reattach`, and `notify` straight through from `args`
      instead of the current hardcoded placeholders; unit test (or BDD, matching
      house convention for this file) that a `RolloutFleet` dispatch's resulting
      `PushArgs` carries each flag's value unchanged from the `DeployArgs` it
      was built from.
- [ ] 2.2 Add an early `--reattach` branch to `run_for_scope` (or `run`,
      whichever owns scope resolution) in `crates/xi/src/deploy.rs` that
      dispatches straight to `push::orchestrate::run`'s existing reattach
      handling before `gather_facts`/`resolve_plan` run for that scope,
      mirroring `push::orchestrate::run`'s own `args.reattach` check; BDD test
      that `xi deploy --reattach` lists receipts without ever calling
      `resolve_plan`, against a fixture host the decision tree would otherwise
      resolve to `ActivateLocal`.

## 3. Verification

- [ ] 3.1 BDD: `xi deploy --sources-transport target-pull` against a fixture
      fleet resolves `PushMode::TargetPull` for every target, matching
      `xi push --sources-transport target-pull`'s own existing BDD coverage for
      the same fixture.
- [ ] 3.2 BDD: `xi deploy --retry 0` against a fixture fleet disables retrying
      entirely, matching `xi push --retry 0`'s own coverage.
- [ ] 3.3 BDD: `xi deploy --build-strategy batched --build-only` then a second
      `xi deploy --from-manifest <path>` invocation round-trips the same way the
      existing `xi push` batched-build-then-ship BDD scenario does, against the
      same fixture shape.
- [ ] 3.4 BDD: `xi deploy --notify <command>` against a fixture fleet fires
      `<command>` once on the push's terminal outcome, matching
      `xi push --notify <command>`'s own coverage.
- [ ] 3.5 Unit test: `xi deploy --build-only --from-manifest <path>` together,
      and each alone with `--build-strategy` omitted, refuse with the exact same
      error text `PushArgs::validate()` already gives for `xi push` - prove it
      by constructing the same `PushArgs` `push_args()` would build from the
      triggering `DeployArgs` and asserting on `.validate()`'s `Err`, not a
      reimplemented check.

## Workflow follow-up

- Archive this change once `xi-deploy-unified-verb` itself archives (its own 5.4
  still waits on a real deprecation window) - there is no reason to hold this
  one open independently once that happens, since by then `DeployArgs` carries
  every flag `PushArgs` does.
