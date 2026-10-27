# Change: xi-sources-race

## Why

Three actors can mutate the same sources dir and state dir on the same host at
the same time:

- The gitops daemon ticking per [[xi-sources-gitops]]
- An interactive `xi {nixos,home,darwin,system} switch` acting on the working or
  sources dir
- An inbound `xi push` from an operator's workstation to this host as the target
  ([[xi-sources-push]])

Each spec names its own write targets, but no spec names how they coordinate
when they overlap. Without coordination, two actors can race to advance
`sources_head`, two can activate different generations, or one can observe a
half-written state. The user's local-wins rule from [[xi-sources-conflict]]
names a policy at the ref level; this change names the enforcement mechanism at
the process level.

## What Changes

- Introduce capability `xi-sources-race` that owns:
  - A **single per-sources-id advisory lock** at the state dir path
    `state_dir/lock`. Every mutating actor acquires it before any write to the
    sources dir or non-metadata state. Non-mutating reads (`xi sources status`,
    `xi sources diff`) do not acquire the lock.
  - A **lock record format**: the lock file carries one JSON line with
    `{pid, actor_name, started_at, intent}` so a stuck lock can be diagnosed
    without strace. `actor_name` is one of `daemon`, `switch`, `push`,
    `bootstrap`, `sync`, `cache-prestage`.
  - A **lock lease timeout** of 15 minutes. A holder that has not touched its
    own `pid-liveness` file (atomic ping each 60s) is considered dead; a new
    acquirer MAY steal the lock after verifying the pid is not a running
    process.
  - A **daemon-pause protocol** for inbound push: when `xi push` detects a
    running daemon for the target sources id, it sends a `pause` RPC over the
    daemon's control socket. The daemon finishes any in-flight tick, suspends
    new ticks, and releases the lock. On push completion xi sends `resume`. On
    push timeout (configurable, default 30 minutes) the daemon resumes
    automatically.
  - A **priority order** when multiple actors contend for a free lock, applied
    by waiting-queue ordering, not preemption:
    1. Interactive user commands (`switch`, `sync`)
    2. Inbound push
    3. Bootstrap (fresh machine; only runs once)
    4. Daemon tick Lower-priority holders release at the next natural checkpoint
       when higher-priority waiters appear.
  - A **cross-scope sanity rule**: system-scope and user-scope sources ids on
    the same host each have their own lock in their own state dir and do not
    cross-interact. The mutual-exclusion rule for the SAME remote URL at both
    scopes is owned by [[xi-sources-nix-modules]].

## Impact

- **Affected specs**: `xi-sources-race` (new capability).
- **Affected code (planned)**: `crates/xi-sources/src/lock.rs` wrapping
  `fcntl`-based advisory locks; integration in every actor entry point; new RPC
  method on the gitops daemon control socket.
- **Coordination**: every mutating actor documented in model, config, conflict,
  gitops, push, bootstrap, cache-prestage, and sync honours this capability.
- **Breaking?**: No. The lock file is new; before this change the behaviour was
  effectively "fire and hope".
- **Migration**: None. Stale lock files from pre-upgrade are stolen on first
  acquisition after a pid-liveness check.

## Success Criteria

- **SC-E-001**: Daemon tick holding the lock blocks a parallel
  `xi system switch` for the duration of the tick; the switch reports
  `status: Pending, reason: lock-held-by-daemon` with the holder's record and
  the estimated wait.
- **SC-E-002**: `xi push <this-host>` from a workstation while the local daemon
  is running sends `pause`, the daemon acknowledges and releases the lock within
  60 seconds of its current tick finishing, the push proceeds, and `resume`
  restarts the daemon.
- **SC-E-003**: Dead-holder recovery: a lock file naming a PID that no longer
  exists is stolen by the next acquirer after a 10-second grace period, with a
  log line naming both the dead holder and the new acquirer.
- **SC-E-004**: Priority queue honoured: with the daemon in a mid-tick, a
  `switch` and a `push` arrive. On tick completion the switch acquires first,
  then the push.
- **SC-E-005**: System-scope and user-scope operations on the same host at the
  same time do not interact; their locks live in different state dirs.
- **SC-E-006**: Lock lease timeout defaults to 15 minutes; a holder that would
  exceed MUST refresh its liveness file; failure to refresh constitutes an
  invitation to steal.
- **SC-E-007**: Lock acquisition failure due to a well-formed held lock prints
  the holder's actor name, pid, intent, started_at, and estimated completion if
  the actor reports it.
