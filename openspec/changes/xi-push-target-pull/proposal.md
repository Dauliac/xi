# Proposal

## Why

`openspec/changes/xi-sources-push/tasks.md` section 9 names "Reverse mode
(target initiates pull from operator over SSH tunnel)" as task 9.3 and leaves it
unchecked and deferred - a one-line placeholder, never designed. Every push mode
today (`--mode closure`, `--mode sources`) moves bytes from the operator to the
target: `copy_closure` or `sync_dir`, both initiated and paid for (bandwidth,
CPU to stream) by the operator's own machine, even when the target could reach
the exact same git remote directly and pull it itself far more cheaply.

The target side of this is not hypothetical: `crates/xi-sources-daemon` already
runs there, with a working `AF_UNIX` control socket
(`crates/xi-sources-daemon/src/control.rs`, length-prefixed JSON framing,
`state_dir/control.sock`) that an external caller already connects to (today
only `xi push`'s `pause`/`resume` calls, per `xi-sources-race`), and a tick
(`crates/xi-sources-daemon/src/tick.rs::run`) that already fetches from the
configured remote and fast-forwards the sources dir as step 4 of its own
pipeline. A push naming a target whose daemon can already reach the remote does
not need a second transport invented from scratch; it needs one more request
type on the socket that is already listening, and a thin wrapper around the
tick's existing fast-forward step so it can run on demand instead of only on the
poll timer.

## What Changes

- `control.rs`'s socket gains a sibling request/reply pair alongside
  `PauseRequest`/`PauseReply` -
  `AdvanceNow { to_sha: String,
  requester_pid: u32 }` and `AdvanceReply`
  (`Advanced { new_head }` | `NotFastForward` | `Busy`) - framed and dispatched
  the same way (`handle_connection` already reads one length-prefixed JSON
  request and writes one reply; it grows a second message type, not a second
  listener). On receipt, the daemon runs `tick::run`'s fetch-and-resolve steps
  immediately instead of waiting for its poll interval, reusing `tick::run`
  itself rather than a second, parallel fast-forward check. `NotFastForward` is
  the exact outcome `tick::run` already produces for a non-descendant advance
  today; this change does not add a new conflict policy, it adds a way to ask
  for the existing one out of band.
- `AdvanceNow` arriving while a tick is already in flight (`tick_in_flight` in
  `control.rs`'s `Inner`) replies `Busy` rather than queuing or blocking: a push
  that wants a guaranteed fresh pull should retry, not silently wait behind a
  tick it has no visibility into.
- `xi push` gains a `PushMode::TargetPull` (alongside today's `Closure` and
  `Sources`): instead of `copy_closure`/`sync_dir`, it sends `AdvanceNow` and
  receives the reply once the daemon's fetch-and-advance (and, if the target
  also runs the daemon's own build/switch hooks, the activation) completes. The
  control socket is `AF_UNIX`, local to the target's own filesystem, with
  nothing today bridging it to an operator's remote process - `control::call`
  (`crates/xi-sources-daemon/src/control.rs:130`) is only ever invoked locally
  in its own tests today. Rather than SSH port-forwarding the socket (a second
  transport mechanism alongside `Transport::run`'s existing SSH plumbing),
  `xi-sources-daemon` gains a small `ctl` subcommand that connects to the local
  socket and relays one request/reply frame over its own stdin/stdout, invoked
  through the existing `Transport::run` (`ssh ... -- xi-sources-daemon ctl`)
  exactly like every other remote command this crate already runs.
- A config ladder - `--sources-transport {operator-copy, target-pull}` /
  `.xi.toml [push] sources-transport` (per-host override first, then table
  default) / compiled floor `operator-copy` - because `target-pull` is only
  correct when the target can actually reach the configured remote: a private
  repo behind an operator-only credential, or a target with no outbound network
  path to the git host, must keep defaulting to bytes-from-operator. The floor
  never guesses reachability; an operator opts a target into `target-pull`
  explicitly once they have verified it.
- `xi push` treats a failed `UnixStream::connect` to the target's `control.sock`
  (no daemon running, or the socket path doesn't exist) as a local, push-side
  error - refusing `target-pull` with a named error pointing at `operator-copy`,
  rather than silently falling back. This is not a reply the daemon sends (there
  is nothing listening to send one); it is `xi push`'s own classification of the
  connection failure.

## Capabilities

### New Capabilities

- `xi-push-target-pull`: a third push transport that orders a target to pull its
  own sources from the configured remote via its existing gitops daemon control
  socket, instead of the operator copying bytes, gated by an explicit per-target
  opt-in.

### Modified Capabilities

- `xi-sources-race`: `control.rs`'s socket protocol grows a second request/reply
  pair (`AdvanceNow`/`AdvanceReply`) alongside `PauseRequest`/`PauseReply`. Not
  yet archived into `openspec/specs/`, so recorded here as a forward reference
  rather than a formal spec delta.

## Impact

- **Affected code**: `crates/xi-sources-daemon/src/control.rs`
  (`AdvanceNow`/`AdvanceReply` framing and dispatch, reusing
  `handle_connection`'s existing read-one-write-one shape),
  `crates/xi-sources-daemon/src/tick.rs` (expose `run`'s fetch-and-resolve steps
  as a callable unit the control handler can invoke directly, not only from the
  poll loop), a new `ctl` subcommand on `crates/xi-sources-daemon`'s own binary
  (`src/main.rs`) that relays one frame over stdio against the local socket, new
  `crates/xi-deploy/src/push/mode_target_pull.rs`,
  `crates/xi-deploy/src/push/args.rs` (`--sources-transport`),
  `crates/xi-deploy/src/push/config.rs` (the ladder).
- **Coordination**: `crates/xi-sources-daemon` and its control socket already
  exist and are in active development under the `xi-sources-gitops` epic
  (`xi-llb`) - this change is not blocked on them landing, only on reading the
  current state of `tick.rs::run` accurately enough to factor out the piece
  `AdvanceNow` needs without duplicating its fast-forward check. Also promotes
  and supersedes `xi-sources-push` task 9.3, which named only an
  SSH-tunnel-based reverse mode; this change's control-socket RPC replaces that
  approach rather than complementing it.
- **Breaking?**: No. `operator-copy` stays the floor; `target-pull` is opt-in
  per target.
- **Migration**: None.

## Success Criteria

- **SC-TP-001**: A target configured `sources-transport = "target-pull"`
  responds to `xi push` by fetching from its own configured remote and
  fast-forwarding, not by receiving an `rsync`'d tree from the operator.
- **SC-TP-002**: `AdvanceNow` to a sha that is not a descendant of the target's
  current `sources_head` gets `AdvanceReply::NotFastForward` - the exact outcome
  `tick::run` already produces for a non-descendant advance on its own poll
  cycle, not a second, divergent check.
- **SC-TP-003**: `AdvanceNow` arriving while a tick is already in flight gets
  `AdvanceReply::Busy` rather than blocking or queuing.
- **SC-TP-004**: A target with `target-pull` configured but no daemon listening
  on the control socket refuses the push, locally, with an error naming
  `operator-copy` as the fallback, rather than silently switching transports.
- **SC-TP-005**: The compiled floor (`operator-copy`) and per-host ladder
  precedence match `resolve_remote_build`'s existing shape exactly: CLI >
  per-host > table > floor.

## Open Questions

1. `tick::run` (`crates/xi-sources-daemon/src/tick.rs:89`) takes `&dyn
   Exec`
   and `&TickInputs<'_>` and runs the full pipeline (fetch, verify signing,
   resolve/fast-forward, build, switch, hooks) as one call. Does `AdvanceNow`
   want that whole pipeline, or only its fetch-and-resolve prefix (leaving
   build/switch to the target's own next scheduled tick)? Leaning toward the
   whole pipeline, matching what the proposal's own text already promises ("if
   the target also runs the daemon's own build/switch hooks, the activation"),
   but this needs confirming against `TickOutcome`'s actual shape before task
   1.2 is implemented.
2. Does a target-pull push still want `xi-sources-race`'s lock contention
   semantics (`ActorName::Push`) even though the daemon, not the push client,
   does the actual write? Likely yes - the daemon's own tick already takes the
   lock - but worth confirming against `lock.rs`'s actual `ActorName` enum.
