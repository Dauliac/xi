# Design: devshell-eval-governor

## Context

The question that triggered this change: should per-devshell daemon work move
into the central gitops daemon (`xi-sources-daemon`) to save one process per
devshell and gain resource caps plus a build queue?

Investigation showed the premise is slightly off. The per-flake daemon is cheap
(one re-exec'd `xi` process, a 200 ms poll loop, a tiny inotify set, exit on
last-deregister or 1 h idle). Derivation builds are already queued and deduped
system-wide by nix-daemon. The genuinely unbounded resource is the client-side
nix _evaluator_: N concurrent `print-dev-env` processes, each potentially
multi-GB, invisible to `max-jobs`.

## Goals / Non-Goals

Goals:

- Bound the number of concurrent background devshell evaluations machine-wide.
- Cap aggregate memory/CPU and lower scheduling priority of background evals.
- Zero change to daemon topology, wire contract, or trust model.
- Crash-safe: no cleanup daemons, no stale-lock janitor.

Non-goals:

- Central eval broker, inter-project priority scheduling, watcher
  centralization, foreground queueing, `job_registry.rs` wiring.

## Decisions

### Decision 1: keep the per-flake daemon; do not centralize

The seam for resource governance is around the **nix spawn sites**, not around
the daemon processes. Rejected alternative: merge devshell eval into
`xi-sources-daemon`. Three couplings argued against it:

1. **Lifetime**: devshell daemons are refcounted by shells and die with them;
   the sources daemon is a long-lived per-scope systemd unit. Merging trades
   "shell exits, daemon dies" for refcount/GC logic inside a daemon that must
   never die.
2. **Trust**: today trust is enforced client-side before any daemon exists
   (`prompt.rs`; pinned by `xi-devshell-trust`: untrusted means no daemon spawn,
   no eval). A multi-project daemon must re-check trust server-side per request,
   and a poisoned eval for project A runs in the process holding project B's
   profiles and GC roots. Same uid, so no privilege escalation, but a wider
   blast radius.
3. **Availability**: an OOM'd eval of one huge flake must not take down gitops
   activation for NixOS/home-manager configs.

The specs pin flake-root identity, socket paths, and the `PromptResponse`
contract, not process topology, so centralizing stays possible later. If the
governor proves insufficient (priority scheduling across projects, real inotify
exhaustion), the escape hatch is a user-scope _eval broker_ that per-flake
daemons submit jobs to: that is where `job_registry.rs` would finally earn its
keep. Not now.

### Decision 2: flock slot pool over a broker socket or an advisory counter

A fixed directory of slot files, `flock(LOCK_EX | LOCK_NB)` each in turn, block
on one if all are busy. Chosen because:

- Kernel-owned release on process death: no stale state after `kill -9`, no
  janitor, no heartbeats. (SC-G-002 falls out for free.)
- No new long-lived process, no protocol, no version skew.
- Prior art in-tree for the semaphore shape (`xi-deploy` `push/limits.rs`,
  `xi_core::cache::run_bounded`), extended across processes by the lock files.

Rejected: a counter file with compare-and-swap (stale on crash); a broker socket
(reintroduces the centralization costs for marginal benefit); POSIX named
semaphores (orphaned on crash, unlink races).

Shrinking `max_concurrent_evals` from K to J takes effect lazily: acquirers only
probe slots `0..J`, so slots `J..K` drain as holders finish. Growing takes
effect immediately. No coordination needed.

### Decision 3: systemd slice for caps, nice-only fallback

`systemd-run --user --scope --slice=xi-develop.slice` around the eval child. A
shared slice makes `MemoryMax`/`CPUWeight` bound the aggregate of all devshell
evals, strictly stronger than per-process rlimits and exactly the cap the user
asked for ("the devshell feature may use at most X"). Rejected: raw cgroup fs
writes (fights systemd's delegation model); `setrlimit` (per-process only, RSS
limits are unenforceable on Linux); running the whole daemon in the scope (would
also throttle the socket responder; only the eval child should be capped).

Detection: try `systemd-run --user`; on failure (Darwin, no user manager, PID 1
not systemd), degrade to setting niceness on the spawned child and log once. The
spawn wrapper is one function so `enter.rs`-style callers can adopt it later if
desired.

### Decision 4: background-only scope

Only daemon-initiated evals acquire slots. Foreground `xi develop enter` and
explicit force-eval bypass the pool. Rationale: interactive use is self-limiting
(one human, one shell), the worst case is a transient +1 over the cap, and
blocking a prompt behind a background re-eval of another project is the exact UX
this feature must not create. The slice caps still apply to whatever the wrapper
spawns, so the memory bound stays honest if foreground paths adopt the wrapper.

### Decision 5: user-scope config only

`[develop.governor]` is read from the user config layer and
`XI_DEVELOP_GOVERNOR_*` env vars; repo `.xi.toml` is ignored for this table (and
the daemon logs the ignore). Limits describe the machine, not the project, and a
cloned repo must never be able to raise them. This is defense in depth behind
the trust gate, not a replacement for it.

### Decision 6: no new wire state

A daemon waiting for a slot remains in its current state-machine state and
exposes the wait as additive metadata (flag + duration) on state query
responses. Adding a `QueuedForEval` variant would touch the
`xi-devshell-activation` contract and every client match arm for a condition
that is operational, not semantic. The wait must be interruptible (shutdown,
last-deregister) and must not feed the eval backoff.

## Risks / Trade-offs

- **Starvation under a low slot count**: with 1 slot and a slow mega-flake,
  other projects' envs go stale until the slot frees. Mitigation: staleness is
  already a modeled condition (daemons serve the previous env and re-eval
  later); default of 2 slots; configurable.
- **flock on exotic /tmp**: network or FUSE /tmp could break flock. Judged
  acceptable: `/tmp/xi-{uid}` already hosts unix sockets, which are pickier than
  flock.
- **systemd-run latency**: ~tens of ms per spawn, negligible against eval cost.
- **Transient over-cap from foreground bypass**: accepted by Decision 4.
