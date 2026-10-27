# Tasks: xi-sources-race

Live state in beads under `change:xi-sources-race`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-race/spec.md`.
- [ ] 1.2 Cross-link with every actor spec (gitops, push, conflict, bootstrap,
      cache-prestage, model).

## 2. Lock primitive (P1)

- [ ] 2.1 `crates/xi-sources/src/lock.rs` wrapping `fcntl` advisory lock on
      `state_dir/lock`. Acquires with timeout parameter.
- [ ] 2.2 Lock record is a single JSON line
      `{pid, actor_name, started_at: iso8601, intent: str}` written after
      acquisition.
- [ ] 2.3 `pid-liveness` sidecar file at `state_dir/lock.alive` touched every
      60s while held.

## 3. Steal protocol (P1)

- [ ] 3.1 On acquisition failure, read the lock record; verify the pid exists
      (`/proc/<pid>` on Linux, `kill -0` fallback).
- [ ] 3.2 If pid absent, wait 10 seconds grace, re-check, then steal with a log
      line naming both actors.
- [ ] 3.3 If pid exists but `lock.alive` mtime is older than 90s, treat as hung;
      steal with a loud warning.

## 4. Daemon control socket pause/resume (P1)

- [ ] 4.1 Daemon exposes `pause` and `resume` RPC methods on its control socket
      (named per [[xi-sources-gitops]]).
- [ ] 4.2 `pause` returns after the current tick finishes AND the daemon has
      released the lock. New ticks do not start.
- [ ] 4.3 `resume` restarts the tick loop. A dead client (push process gone)
      auto-resumes after 30 minutes.

## 5. Priority queue (P2)

- [ ] 5.1 Waiters register themselves at `state_dir/lock.waiters/<pid>` with
      their actor name.
- [ ] 5.2 Lock release scans the waiter dir and picks the highest-priority
      waiter. Priority order: switch > sync > push > bootstrap > daemon.

## 6. Status surface (P1)

- [ ] 6.1 `xi sources status` reports the lock state (`held_by`, `held_since`,
      `estimated_wait`) when non-empty.
- [ ] 6.2 Any lock-wait exit from a mutating command prints the holder record
      and the wait estimate.

## 7. Tests (P1)

- [ ] 7.1 BDD: concurrent `switch` and daemon tick serialise.
- [ ] 7.2 BDD: inbound push pauses daemon, completes, resumes.
- [ ] 7.3 BDD: dead-holder recovery with a killed pid.
- [ ] 7.4 BDD: priority order with three queued actors.

## 8. Follow-ups (P4)

- [ ] 8.1 Fair queuing beyond strict priority (anti-starvation of daemon ticks).
      Deferred.
- [ ] 8.2 Cross-host lock service for multi-host push-with-pull coordination.
      Deferred.
