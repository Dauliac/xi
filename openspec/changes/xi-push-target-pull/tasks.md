# Tasks

Live state in beads under `change:xi-push-target-pull`.

## 1. Control-socket RPC

- [ ] 1.1 Define `AdvanceNow { to_sha, requester_pid }` and `AdvanceReply`
      (`Advanced { new_head }` | `NotFastForward` | `Busy`) as a sibling
      request/reply pair to `PauseRequest`/`PauseReply` in
      `crates/xi-sources-daemon/src/control.rs`, dispatched from the same
      `handle_connection` read-one-write-one loop.
- [ ] 1.2 Factor `tick::run`'s fetch-and-resolve steps so `AdvanceNow`'s handler
      can invoke them directly, outside the poll loop, without a second
      fast-forward check; resolve Open Question 1 (whole pipeline vs.
      fetch-and-resolve prefix only) before finishing this task.
- [ ] 1.3 `AdvanceNow` arriving while `tick_in_flight` is true replies `Busy`
      immediately rather than deferring like a `pause` call does.
- [ ] 1.4 Unit test the fast-forward refusal against a fixture history,
      asserting it is the _same_ check the poll-driven tick uses (a shared
      function, not a parallel copy).

## 2. Reaching the socket from the operator

- [ ] 2.1 A `ctl` subcommand on `xi-sources-daemon`'s binary: connect to the
      local `control.sock`, relay one request frame from stdin and one reply
      frame to stdout, exit.
- [ ] 2.2 Unit test: the `ctl` subcommand against a running `ControlServer`
      round-trips an `AdvanceNow` request correctly.

## 3. Push-side transport

- [ ] 3.1 `PushMode::TargetPull`: run `xi-sources-daemon ctl` over
      `Transport::run` (SSH), write the `AdvanceNow` frame to its stdin, read
      the reply frame from its stdout.
- [ ] 3.2 A failed SSH spawn, a non-zero `ctl` exit, or a decode failure all
      surface as one named error pointing at `operator-copy` - not a silent
      fallback, and not three different error shapes.
- [ ] 3.3 `--sources-transport` CLI flag and
      `.xi.toml [push]
      sources-transport` ladder (per-host, then table,
      then floor `operator-copy`), mirroring `resolve_remote_build`.

## 4. Tests

- [ ] 4.1 BDD: a target configured `target-pull` against a fixture daemon socket
      completes a push via `AdvanceNow` with no `copy_closure`/ `sync_dir` call
      ever made.
- [ ] 4.2 BDD: the same target with no daemon listening (socket connect fails)
      refuses with the named `operator-copy`-pointing error.
- [ ] 4.3 BDD: `AdvanceNow` arriving mid-tick gets `Busy`, not a hang.
- [ ] 4.4 BDD: ladder precedence (CLI > per-host > table > floor) exercised the
      same way `resolve_remote_build`'s own tests already are.

## Workflow follow-up

- Confirm Open Question 2 (whether a target-pull push should also participate in
  `xi-sources-race`'s `ActorName::Push` lock priority) against `lock.rs`'s
  actual enum before closing task 1.2.
