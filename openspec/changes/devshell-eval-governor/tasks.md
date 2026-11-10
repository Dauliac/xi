# Tasks: devshell-eval-governor

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads.

## 1. Specification (P1)

- [ ] 1.1 Land the spec delta under `specs/xi-devshell-eval-governor/spec.md`.
- [ ] 1.2 Cross-link with `xi-devshell-activation` (metadata field is additive,
      no wire change) and `xi-devshell-trust` (governor is strictly post-trust).
- [ ] 1.3 Product decision: confirm the default `max_concurrent_evals = 2` (vs.
      `min(parallelism, 8)` as used by `xi_core::cache`). Eval is memory-bound,
      so the lower default is proposed; record the decision in the spec once
      made.

## 2. Slot pool (P1)

- [ ] 2.1 Add `eval_slots_dir()` to `crates/xi-develop/src/dirs.rs`, creating
      `/tmp/xi-{uid}/eval-slots/` with the same ownership/permission guarantees
      as the socket directories.
- [ ] 2.2 New `governor` module in `xi-develop`: `SlotPool::acquire(n)` that
      probes slot files `0..n` with `flock(LOCK_EX | LOCK_NB)` and falls back to
      a blocking wait on one slot; RAII guard releases on drop; crib the
      semaphore shape from `xi-deploy/src/push/limits.rs`.
- [ ] 2.3 Make the blocking wait interruptible: poll-based wait that checks the
      daemon shutdown flag and consumer refcount each cycle (reuse the 200 ms
      `WATCHER_POLL_MS` cadence).
- [ ] 2.4 Gate `spawn_eval`'s background path on `SlotPool::acquire`;
      foreground/force paths bypass per Decision 4 in `design.md`.
- [ ] 2.5 Ensure slot waits do not advance the eval backoff (`should_eval`/state
      machine interaction).
- [ ] 2.6 BDD `bdd_governor_slot_serializes`: two daemons, one slot, assert at
      most one evaluator process at any instant (SC-G-001).
- [ ] 2.7 BDD `bdd_governor_crash_releases_slot`: SIGKILL the holder, assert a
      queued daemon acquires within one poll interval (SC-G-002).
- [ ] 2.8 BDD `bdd_governor_deregister_while_queued`: last consumer deregisters
      during a slot wait; daemon exits without spawning the eval (SC-G-006).

## 3. Resource scoping (P1)

- [ ] 3.1 Spawn wrapper: detect a usable systemd user manager once per daemon;
      wrap background eval children in
      `systemd-run --user --scope
      --slice=xi-develop.slice` with
      `-p MemoryMax=`, `-p CPUWeight=`, `-p Nice=` from config. Keep the wrapper
      a single function so foreground paths can adopt it later.
- [ ] 3.2 Fallback path: no systemd, set niceness on the child directly
      (pre-exec `setpriority` or `nice`-prefix), log degradation once per daemon
      lifetime (SC-G-004).
- [ ] 3.3 Map cgroup OOM kills of the eval child to a `BuildFailed` diagnostic
      that names the configured memory cap (SC-G-003).
- [ ] 3.4 BDD `bdd_governor_nice_applied`: assert the eval child's niceness
      differs from the daemon's.
- [ ] 3.5 BDD `bdd_governor_oom_diagnostic` (Linux-only, systemd-only):
      `memory_max` small, flake whose eval exceeds it, assert `BuildFailed` with
      cap-naming diagnostic and a surviving daemon.

## 4. Configuration (P1)

- [ ] 4.1 Add `[develop.governor]` parsing to the user-level config layer:
      `max_concurrent_evals`, `memory_max`, `cpu_weight`, `nice`; env overrides
      `XI_DEVELOP_GOVERNOR_*` win over file config.
- [ ] 4.2 Explicitly ignore the table when it appears in repo-level `.xi.toml`,
      with a log line (SC-G-007).
- [ ] 4.3 BDD `bdd_governor_repo_config_ignored` and
      `bdd_governor_env_override`.

## 5. Observability (P2)

- [ ] 5.1 Additive state-query metadata: waiting flag + wait duration while
      queued (`state_meta.rs` / `state_responder.rs`); no new `DaemonState`
      variant.
- [ ] 5.2 Daemon startup log line reporting effective governor config (slot
      count, caps, systemd availability).
- [ ] 5.3 BDD `bdd_governor_queued_state_query`: daemon mid-wait answers a state
      query within budget and reports the wait (SC-G-005).

## 6. Foreground bypass verification (P2)

- [ ] 6.1 BDD `bdd_governor_enter_bypasses_pool`: hold all slots with fake
      lockers, run `xi develop enter` on a trusted flake, assert entry is not
      delayed (SC-G-008).
- [ ] 6.2 BDD `bdd_governor_untrusted_untouched`: untrusted root under full slot
      contention produces no daemon, no slot wait, no governor logs (trust
      requirement).
