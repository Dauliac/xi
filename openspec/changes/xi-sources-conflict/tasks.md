# Tasks: xi-sources-conflict

Live state in beads under `change:xi-sources-conflict`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-conflict/spec.md`.
- [ ] 1.2 Cross-link with `xi-sources-model` (ref types) and `xi-sources-gitops`
      (where the daemon enforces this).

## 2. Resolver (P1)

- [ ] 2.1 Implement `resolve_evaluation_source(ctx) -> EvaluationSource` with
      branches: `WorkingDir`, `SourcesDir`, `Error { reason }`.
- [ ] 2.2 Enumerate the 6 ref-state cases (table in spec).
- [ ] 2.3 Document the `--force-dirty` escape with its own test.

## 3. Dirty-tree detection (P1)

- [ ] 3.1 Call `git status --porcelain` on the working dir.
- [ ] 3.2 Also detect in-progress rebase/merge via `.git/MERGE_HEAD` /
      `.git/REBASE_HEAD` existence.
- [ ] 3.3 "Dirty" = any of the above; "clean" = none.

## 4. Daemon invariant (P1)

- [ ] 4.1 Gitops daemon's write path MUST assert the write target is
      `sources_dir`; writing to `working_dir` is a panic in debug and a logged
      error in release that aborts the tick.

## 5. `xi sources diff` (P2)

- [ ] 5.1 Three-way diff (remote vs sources vs working); shells out to
      `git diff` for the body, wraps with the house envelope.
- [ ] 5.2 No working dir configured, emit a two-way diff (remote vs sources).

## 5a. Hermetic sync `xi sources sync` (P1)

- [ ] 5a.1 Implement the copy-with-erase from sources dir to working dir using
      `git ls-tree -r --name-only <sources_head>` as the authoritative
      tracked-file set, plus a deletion pass for files tracked at `working_head`
      but absent at `sources_head`.
- [ ] 5a.2 After the file pass, run
      `git -C <working_dir> reset --soft
      <sources_head>` so HEAD matches
      but index and staged edits survive for review.
- [ ] 5a.3 Guard the sync against any write outside the working dir. Panic in
      debug on an attempted write to the sources dir, state dir, or anywhere
      else; abort the sync in release.
- [ ] 5a.4 Refuse a dirty working dir without `--force-dirty`.
- [ ] 5a.5 Forbid any `git pull`, `git merge`, `git rebase`, or `git fetch` into
      the working dir in the daemon code path. Lint rule plus a runtime
      assertion.

## 5b. `sync_mode` config key (P1)

- [ ] 5b.1 Add `sync_mode` enum with `prompt | auto-copy | never` to
      `.xi.toml [sources]` and the matching env var `XI_SOURCES_SYNC_MODE` under
      [[xi-sources-config]].
- [ ] 5b.2 Wire the daemon's post-advance hook to the enum: `prompt` sets
      `sync_available: true` in state; `auto-copy` queues the sync for the next
      interactive command; `never` logs only.
- [ ] 5b.3 Expose `sync_available: bool` and `pending_sources_sha` on the
      `xi sources status` response envelope.

## 6. Tests (P1)

- [ ] 6.1 Unit test for each of the 6 ref-state matrix cells.
- [ ] 6.2 BDD scenario `bdd_sources_conflict.rs` for the SC-X-* criteria.

## 7. Follow-ups (P4)

- [ ] 7.1 Operator-chosen three-way merge driver. Deferred.
- [ ] 7.2 Auto-rebase working dir on top of remote. Deferred - explicit against
      user's "local non-pushed is OK" rule.
