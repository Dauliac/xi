# Tasks: devshell-watcher-scope

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads.

## 1. Specification (P1)

- [ ] 1.1 Land the spec delta under `specs/xi-devshell-watcher/spec.md`.
- [ ] 1.2 Cross-link with `xi-devshell-daemon` — the `WatcherDegraded` state and
      the `InvalidateCache` command are defined there.

## 2. Coalescing + minimal-directory registration (P1)

- [ ] 2.1 Add a 200 ms coalescing buffer between the `notify` callback and the
      state-machine event channel. Multiple filesystem events within one window
      collapse to a single `FileChangeEvent`.
- [ ] 2.2 Refactor the watch-registration logic to compute the minimal set of
      unique parent directories containing watched files. Register each at
      `RecursiveMode::NonRecursive`.
- [ ] 2.3 BDD `bdd_watcher_coalesce`: fire 50 modification events across 50
      files inside a 100 ms window; assert exactly one re-eval trigger.
- [ ] 2.4 BDD `bdd_watcher_min_dirs`: place 100 tracked `.nix` files in 4
      subdirectories; assert the watcher registers exactly 5 paths (4 subdirs +
      `.git/index`).

## 3. Degradation + recovery (P1)

- [ ] 3.1 On `notify` errors matching the documented degradation conditions,
      emit a `WatcherDegraded` transition event to the state machine.
- [ ] 3.2 On any `InvalidateCache` command, attempt a single re-registration of
      the watcher before returning; do NOT loop on failure.
- [ ] 3.3 BDD `bdd_watcher_degrade_inotify_limit` (Linux-only): with a
      restricted `max_user_watches`, assert the daemon transitions to
      `WatcherDegraded` and state queries continue to answer within the
      state-query budget.
- [ ] 3.4 BDD `bdd_watcher_invalidate_recovers`: from `WatcherDegraded`, run
      `InvalidateCache`, assert one re-eval fires and one re-registration is
      attempted.

## 4. Git-index awareness (P2)

- [ ] 4.1 Register `.git/index` in the watched-directory set.
- [ ] 4.2 On `.git/index` change, re-derive the git-tracked `*.nix` set and
      update the watch registration diff (add/remove per file set delta).
- [ ] 4.3 BDD `bdd_watcher_new_tracked_file`: `git add foo.nix`, then modify
      `foo.nix`; assert the modification triggers a re-eval without daemon
      restart.

## 5. User-extension surface (P2)

- [ ] 5.1 Wire `[develop.watch] extra` from `.xi.toml` through the shared
      `TimeoutsConfig` (or a new `WatcherConfig` sibling) so both client and
      daemon can read it consistently.
- [ ] 5.2 BDD `bdd_watcher_extra_patterns`: with `extra =
      ["Cargo.lock"]`,
      assert modifications to `Cargo.lock` trigger re-eval and modifications to
      `README.md` do not.

## 6. Observability (P3)

- [ ] 6.1 Emit a single `info!` on daemon startup summarising watched dir count,
      coalescing window, and inotify usage. Operators can grep this line to spot
      pathological watch sets.

## 7. Follow-ups (P4 — deferred to future changes)

- [ ] 7.1 User-configurable coalescing window (`coalesce_ms`).
- [ ] 7.2 Watching imported nix files outside the flake root (needs a design
      proposal — potentially expensive; not driven by current user demand).
- [ ] 7.3 Submodule / worktree index awareness.
