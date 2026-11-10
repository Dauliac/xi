# Change: devshell-eval-governor

## Why

Each devshell flake root gets its own forked daemon, and each daemon spawns
`nix print-dev-env` to evaluate and realize the shell environment
(`crates/xi-develop/src/daemon/server.rs::run_nix_eval`). Derivation-level build
concurrency is already governed globally by the system nix-daemon (`max-jobs`,
`cores`, store-path dedup that `enter.rs` explicitly relies on). Evaluation is
not: it happens client-side, inside the per-daemon `print-dev-env` process,
where nix-daemon's limits cannot see it.

Consequences on a machine with several active projects:

- N independent evaluator processes can run at once, each with an RSS that
  reaches multiple GB on large flakes. Nothing bounds the aggregate.
- No memory, CPU, or niceness control exists anywhere in the workspace for
  devshell work; a background re-eval competes at full priority with the user's
  foreground compile.
- There is no queue limiting concurrent devshell evaluations across projects.

The obvious alternative, folding devshell work into the central gitops daemon
(`xi-sources-daemon`), was considered and rejected; see `design.md`. The chosen
approach keeps the per-flake daemon topology untouched and adds a cross-process
governor at the nix spawn seam: a flock-based eval slot pool plus a shared
systemd slice for resource caps.

## Verified Against Current Code (2026-11-10)

- **One eval at a time per daemon already**: `spawn_eval` is gated by the
  `eval_in_progress` mutex (`daemon/server.rs:818-830`) and `current_target` is
  a single `RwLock<String>`. `job_registry.rs` exists but is not wired to
  execution (only reachable from the v3 `attach_or_start` handler, which mints
  job IDs and returns `Accepted`; see TODOs at `state_responder.rs:384` and
  `env_pointer.rs:39-43`). So the gap is cross-daemon, not intra-daemon.
- **The eval spawn carries no resource flags**: `run_nix_eval`
  (`daemon/server.rs:1339-1377`) passes only `connect-timeout` and
  `download-attempts`. No `nice`, `setrlimit`, cgroup, `systemd-run`,
  `--max-jobs`, or `--cores` usage exists anywhere in the devshell path.
  `NIX_BUILD_CORES` appears only as a variable _stripped_ from generated env
  files (`env_file.rs:41`).
- **`systemd-run` precedent exists** but only for remote push units
  (`crates/xi-deploy/src/push/mode_sources.rs:185`), with no resource properties
  set.
- **Semaphore precedent exists in-tree**: the hand-rolled counting
  `Semaphore`/`PhaseLimits` in `crates/xi-deploy/src/push/limits.rs` and
  `xi_core::cache::run_bounded` with `resolve_max_parallel_jobs` defaulting to
  `min(available_parallelism(), 8)`
  (`crates/xi-core/src/cache.rs:26-45,
  213-231`). Both are in-process only;
  neither spans processes.
- **Daemon identity and socket layout**: one daemon per canonical flake root,
  keyed by `flake_id = sha256(path)[..8]`, sockets under
  `/tmp/xi-{uid}/{flake_id}/daemon.sock` (`dirs.rs:18-24, 97-110`). The per-uid
  `/tmp/xi-{uid}/` directory is the natural home for a cross-daemon slot pool.
- **Foreground spawn sites**: `enter.rs:136-152` runs a `nix build` concurrently
  with eval in a `thread::scope`; `enter.rs:322-347` runs a synchronous
  `print-dev-env` with a 2-minute timeout. These are user-attended, interactive
  paths.

## What Changes

Introduce capability `xi-devshell-eval-governor` that owns:

- **Cross-process eval slot pool**: a directory of flock-backed slot files under
  `/tmp/xi-{uid}/eval-slots/`. Before spawning a background `print-dev-env`, a
  daemon MUST acquire one slot (first lockable file wins). Slot count defaults
  to 2 and is configurable. flock semantics give crash safety for free: a slot
  held by a killed daemon is released by the kernel.

- **Scope: background evals only**. Daemon-initiated (watcher- or
  interval-triggered) evaluations are subject to the pool. Foreground,
  user-attended paths (`xi develop enter`, explicit force-eval commands) are NOT
  queued: a human is waiting, and one human types in one shell at a time. This
  keeps the governor invisible in the interactive hot path.

- **Resource scoping via a shared systemd slice**: when a systemd user manager
  is available, daemons MUST spawn background evals under
  `systemd-run --user --scope --slice=xi-develop.slice` with configurable
  `MemoryMax`, `CPUWeight`, and `Nice`. Because the slice is shared, the caps
  bound the _aggregate_ of all devshell evals, which is strictly stronger than
  per-process limits. When systemd is unavailable (Darwin, non-systemd Linux),
  fall back to setting niceness on the child process only and log the
  degradation once per daemon lifetime.

- **Config surface, user scope only**: `[develop.governor]` with
  `max_concurrent_evals`, `memory_max`, `cpu_weight`, `nice`. Read from the
  user-level config layer and `XI_DEVELOP_GOVERNOR_*` environment overrides
  only. Repo-level `.xi.toml` MUST NOT be able to change governor settings:
  limits are a property of the machine and its owner, not of a checked-out
  repository, and a hostile repo must not be able to lift them (defense in depth
  on top of the trust gate).

- **Queue visibility without wire changes**: a daemon waiting for a slot stays
  in its existing state-machine state; the wait is reported as metadata on the
  state query response (waiting flag plus wait duration). No new `DaemonState`
  variant, so the `devshell-activation-contract` wire surface is untouched.

- **Interruptible waits**: a slot wait MUST be interruptible by daemon shutdown
  and by last-consumer deregistration, and MUST NOT feed the eval-failure
  backoff (waiting is not failing).

- **Non-goals**: priority scheduling between projects; a central eval broker
  process; centralizing the file watcher; queueing foreground interactive entry;
  touching nix-daemon `max-jobs`/`cores`; wiring `job_registry.rs` to execution
  (tracked by the async-daemon work, not this change).

## Impact

- **Affected specs**: `xi-devshell-eval-governor` (new capability; first delta
  introduces its requirements).
- **Affected code**: `crates/xi-develop/src/daemon/server.rs` (`spawn_eval` /
  `run_nix_eval` call site), a new `governor` module in `xi-develop` (slot
  pool + spawn wrapper; the semaphore shape can crib from
  `xi-deploy/src/push/limits.rs`), `crates/xi-develop/src/dirs.rs` (slot
  directory under the existing `/tmp/xi-{uid}/` runtime dir), the layered config
  reader (user-scope `[develop.governor]` table), and
  `state_meta.rs`/`state_responder.rs` for the queued-wait metadata.
- **Coordination**: `xi-devshell-activation` (no wire change; metadata field is
  additive), `xi-devshell-trust` (governor applies strictly after the trust
  gate; untrusted still means no daemon and no eval at all).
- **Breaking?**: No. Purely additive. With the default slot count, machines
  running one or two projects see no behaviour change; machines running many
  projects see background evals serialize instead of stampeding.
- **Migration**: None. Absent configuration, defaults apply. Setting
  `max_concurrent_evals` to a large value effectively restores today's unbounded
  behaviour.

## Success Criteria

- **SC-G-001**: With `max_concurrent_evals = 1` and two daemons on two flakes
  triggered in the same second, exactly one `nix print-dev-env` process runs at
  any instant; the second daemon's eval starts only after the first completes.
- **SC-G-002**: `kill -9` of a daemon mid-eval releases its slot without any
  cleanup step: a queued daemon acquires the slot within one watcher poll
  interval.
- **SC-G-003**: With `memory_max = "1G"` configured on a systemd host, an eval
  that exceeds the cap is killed by the kernel; the owning daemon transitions to
  `BuildFailed` with a diagnostic naming the memory cap, and the daemon process
  itself survives.
- **SC-G-004**: On a host without a systemd user manager, background evals still
  run, carry the configured niceness, and the daemon logs exactly one "resource
  scoping degraded" line per lifetime.
- **SC-G-005**: A daemon waiting for a slot answers state queries within the
  existing state-query budget and reports the wait in the response metadata; the
  shell prompt hook is never blocked by a queued eval.
- **SC-G-006**: Deregistering the last shell while the daemon is queued for a
  slot causes the daemon to exit promptly without ever spawning the eval or
  consuming a slot.
- **SC-G-007**: A repo `.xi.toml` containing
  `[develop.governor]
  max_concurrent_evals = 64` has no effect; the user-scope
  value (or default) wins, and the daemon logs that the repo-level table was
  ignored.
- **SC-G-008**: `xi develop enter` on a trusted flake is never delayed by the
  slot pool, even when all slots are held by background evals.

## Assumptions

- flock on files under `/tmp/xi-{uid}/` is reliable on the supported platforms
  (Linux tmpfs, Darwin); the pool directory inherits the `0700` per-uid
  guarantees that `dirs.rs` already enforces for sockets.
- A default of 2 concurrent background evals is a reasonable floor: eval is
  memory-bound, not core-bound, so `min(parallelism, 8)` (the cache default)
  would be too permissive. The exact default is a product decision and is
  flagged in `tasks.md`.
- Exempting foreground paths is acceptable: interactive use is self-limiting,
  and queueing a human behind background work is worse than briefly exceeding
  the slot count by one.
- `systemd-run --user --scope` is available on the primary Linux targets; Darwin
  accepts nice-only degradation until a launchd/posix_spawn resource story is
  specified (out of scope here).
- Changing the slot count takes effect for newly started waits only; already
  queued daemons keep the slot file they are blocked on. Acceptable because
  daemons are short-lived (1 h idle timeout).
