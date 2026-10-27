# Capability: xi-sources-race

## ADDED Requirements

### Requirement: Single per-sources-id advisory lock

Every actor that intends to WRITE to the sources dir or to a non-metadata part
of the state dir MUST acquire an advisory lock on `<state_dir>/lock` before any
write. Readers (`xi sources status`, `xi sources diff`) do NOT acquire the lock.

Lock scope is one lock per `SourcesId` ([[xi-sources-model]]). System scope and
user scope on the same host carry independent locks in their own state dirs.

#### Scenario: Reader does not block writer

- **GIVEN** the daemon is mid-tick holding the lock
- **WHEN** the operator runs `xi sources status`
- **THEN** the status returns immediately without blocking on the lock

#### Scenario: Second writer blocks on first

- **GIVEN** actor A holds the lock
- **WHEN** actor B attempts to acquire the lock
- **THEN** B waits (bounded by the lease timeout) until A releases

### Requirement: Lock record format

On successful acquisition the actor MUST write a single JSON line to
`<state_dir>/lock` containing exactly these fields:

```json
{
  "pid": 12345,
  "actor_name": "daemon|switch|push|bootstrap|sync|cache-prestage",
  "started_at": "2026-10-26T19:14:03Z",
  "intent": "<human-readable one-liner>"
}
```

The actor MUST also touch `<state_dir>/lock.alive` every 60 seconds while
holding the lock.

On release the actor MUST truncate `<state_dir>/lock` to empty and remove
`<state_dir>/lock.alive`.

#### Scenario: Status surface reports holder

- **GIVEN** actor A has acquired the lock
- **WHEN** another actor fails to acquire
- **THEN** the failure message includes A's `actor_name`, `pid`, `intent`, and
  `started_at`

### Requirement: Steal protocol

A new acquirer that finds `<state_dir>/lock` non-empty MUST verify holder
liveness in this order:

1. Check `/proc/<pid>` on Linux, or `kill(pid, 0)` on other platforms. If the
   pid is absent, enter the grace path.
2. Grace path: wait 10 seconds, re-check `<state_dir>/lock` (the previous holder
   may have been renaming), then steal.
3. If the pid exists but `<state_dir>/lock.alive` mtime is older than 90
   seconds, log a loud warning and steal.

Stealing MUST log a line naming both the previous holder record and the new
acquirer.

#### Scenario: Dead pid stolen after grace

- **GIVEN** `<state_dir>/lock` names a pid that no longer exists
- **WHEN** a new acquirer appears
- **THEN** the acquirer waits 10 seconds, then steals and logs both actor
  records

### Requirement: Daemon control-socket pause/resume

The gitops daemon's control socket (defined by [[xi-sources-gitops]]) MUST
expose two RPC methods:

- `pause`: finish the current tick, release the lock, suspend new ticks. Returns
  when the lock is released.
- `resume`: restart the tick loop.

An inbound `xi push` to a host whose local daemon owns the same `SourcesId` MUST
send `pause` before acquiring the lock. On push completion (success or failure)
`xi push` MUST send `resume`.

A `pause` without a matching `resume` within a configurable timeout (default 30
minutes) MUST auto-resume. The daemon MUST log the auto-resume with the reason.

#### Scenario: Push pauses and resumes daemon

- **GIVEN** a daemon running on host H for a sources id S
- **WHEN** `xi push H` from a workstation runs
- **THEN** the daemon receives `pause`, finishes its current tick, releases the
  lock
- **AND** the push acquires the lock and runs
- **AND** on push exit the daemon receives `resume` and restarts ticking

#### Scenario: Orphan pause auto-resumes

- **GIVEN** a daemon is paused by a push that then crashes
- **WHEN** 30 minutes elapse without a `resume`
- **THEN** the daemon logs the orphan and resumes

### Requirement: Priority queue on waiters

When multiple actors wait for the lock, each MUST register at
`<state_dir>/lock.waiters/<pid>` with a file whose body is its `actor_name`. On
release the holder MUST scan the waiters directory and signal the
highest-priority waiter.

Priority order (highest first):

1. `switch`
2. `sync`
3. `push`
4. `bootstrap`
5. `daemon`

Within the same priority level, FIFO by `started_at` of the waiter registration.

#### Scenario: Switch beats push beats daemon

- **GIVEN** the daemon holds the lock mid-tick; a `switch` and a `push` are both
  waiting
- **WHEN** the daemon releases
- **THEN** `switch` acquires next, then `push`, then the daemon reacquires for
  its next tick

### Requirement: Lease timeout

The lock has a lease timeout, default 15 minutes, configurable per actor. A
holder exceeding the timeout WITHOUT refreshing `<state_dir>/lock.alive` is open
to be stolen by any waiter after the 90-second staleness check.

A holder that legitimately needs more time MUST refresh the alive file.
Bootstrap and large builds routinely exceed 15 minutes; those actors MUST
refresh.

#### Scenario: Alive refresh prevents steal

- **GIVEN** actor A holds the lock for 20 minutes with regular alive-file
  refreshes
- **WHEN** a waiter checks liveness
- **THEN** the lock is NOT stolen; the waiter continues waiting
