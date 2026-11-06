# Tasks

Live state in beads under `change:xi-deploy-build-config`.

## 1. Generalize the ladder resolver

- [ ] 1.1 Extract `resolve_remote_build`'s CLI-over-per-host-over-table-over-
      module-over-floor precedence into a generic `resolve_ladder<T>` helper
      in `crates/xi-deploy/src/push/config.rs`; re-express
      `resolve_remote_build` as a call to it, and verify every existing
      `resolve_remote_build` unit test in `config.rs` still passes unchanged.

## 2. Declarative backend

- [ ] 2.1 Add `backend` to `PUSH_KEYS` and a per-host `backend` field to
      `PushHost`, parsed the same way `remote-build` is today; unit test
      parsing of both the table-level and per-host forms.
- [ ] 2.2 Add `resolve_backend` on top of `resolve_ladder<T>`, consuming a
      new `nix_module_backend: Option<PushBackend>` parameter the same way
      `resolve_remote_build` consumes `nix_module_remote_build`; unit test
      the five-rung precedence end to end.
- [ ] 2.3 Add the flake-parts `xi.sources.push.backend` option, written into
      the same per-scope config file `remoteBuild` already writes into;
      verify the written file's shape against the existing `remoteBuild`
      entry.
- [ ] 2.4 Wire `orchestrate.rs` to call `resolve_backend` instead of reading
      `args.backend` directly; verify `bdd_push_builtin.rs`'s existing
      backend-selection scenarios still pass with no `.xi.toml`/flake-parts
      entries present (CLI-flag and auto-detect paths unchanged).

## 3. Batched build strategy

- [ ] 3.1 Add `--build-strategy {interleaved, batched}` to `PushArgs`,
      default `interleaved`; unit test default parsing.
- [ ] 3.2 Implement the batched phase: for every selected
      `PushMode::Closure` target, call `nix.build` in an upfront pass bounded
      by the existing `limits.eval` semaphore, collecting a
      `BTreeMap<target name, store path>` manifest.
- [ ] 3.3 Implement manifest serialization (`--build-only` writes it;
      `--from-manifest <path>` reads it) and the ship phase that consumes a
      manifest without calling `nix.build` again; BDD test covering build-
      then-ship as two separate invocations against a fixture `Nix`.
- [ ] 3.4 Verify `--build-strategy interleaved` (or the flag omitted)
      produces byte-identical dispatch order to today's `execute()` for the
      existing `bdd_push_orchestrator.rs` scenarios.

## 4. Build-location floor flip

- [ ] 4.1 Add the one-release opt-in gate (env var or config flag, matching
      whatever convention the project already uses for staged default
      changes) around the floor's new value.
- [ ] 4.2 Change the floor itself to resolve `true` for a target that is not
      the local machine, `false`→"not consulted" for a target that is;
      unit test both branches and the opt-in gate's on/off states.
- [ ] 4.3 Update `xi-sources-push`'s own existing `resolve_remote_build` unit
      tests (`with_nothing_configured_the_nix_module_default_answers`,
      `with_every_rung_silent_the_floor_is_false`) to parameterize on the
      opt-in gate rather than asserting the old floor unconditionally.
- [ ] 4.4 Document the migration (`remote-build = false` at the table level)
      in the opt-in window's own warning output, printed once per push that
      would be affected by the flip.

## Workflow follow-up

- Archive this change once `xi-deploy-unified-verb` exposes `--backend` and
  `--build-strategy` through its own flag surface and the opt-in window for
  the floor flip has run its course.
