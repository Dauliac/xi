# Proposal

## Why

`push::mode_sources::wrap_in_systemd_run`
(`crates/xi-deploy/src/push/mode_sources.rs:124`) already survives an SSH drop
in the one sense that matters most: the unit
`systemd-run --wait
--pipe --collect` starts is parented to the target's
systemd, not to the dying SSH session, so a remote build-and-switch keeps
running after the operator's connection breaks. But everything downstream of
that fact is still missing. `--wait`'s exit-code propagation and `--pipe`'s
output streaming both belong to the _local_ `systemd-run` client process invoked
over the now-dead SSH session, so the operator's own `xi push` process loses
both the moment the connection drops - it has no record of which unit it
started, on which target, so there is nothing to reconnect to even though the
work is still happening. And `--collect` unloads the unit from systemd's live
bookkeeping the instant it finishes, success or failure, so a reattach that
arrives late enough has nothing live left to query unless it already knows the
unit's name to search journald by.

## What Changes

- `switch` (`push::mode_sources::switch`) names its transient unit explicitly
  (`systemd-run --unit=xi-push-<push-id> ...`) instead of letting `systemd-run`
  generate an anonymous name, where `<push-id>` is a locally-generated id unique
  to this push invocation (not a timestamp or pid alone - both wrap on a long
  enough timeline, and a target may receive two pushes in the same second from
  two operators).
- Before SSHing in, the operator side writes a local receipt -
  `<state dir>/push-receipts/<push-id>.json`:
  `{target, unit_name, started_at, scope}` - and removes it only after the
  push's own process observes a terminal outcome (success or failure) for that
  unit. A receipt left behind by a process that never got to remove it is
  exactly the signal a reattach needs.
- A new `xi push --reattach [push-id]` verb: with no id, lists every local
  receipt whose target is still reachable, newest first; with an id, SSHes to
  that receipt's target, asks
  `systemctl show <unit_name>
  --property=ActiveState,SubState,ExecMainStatus,ExecMainCode`,
  and reports Running / Succeeded / Failed accordingly - `--collect` having
  already unloaded the unit is read as "ask journald by unit name instead," not
  as an error, since journald retains unit-indexed history independent of
  whether the unit is still live. A still-running unit is followed by streaming
  `journalctl -u <unit_name> -f --since <started_at>`.
- On successful reattach to a terminal state, the local receipt is removed; on
  reattach to a still-running unit, the receipt is left in place for a later
  reattach.
- An optional `--notify <command>` passed to the push invocation that started
  the build: run once a reattach (in the same process, if the connection
  survives, or a later `--reattach`) observes a terminal state, so "tell me when
  it's done" does not require the operator to keep polling by hand.

## Capabilities

### New Capabilities

- `xi-push-resilient-remote-build`: a named, locally-receipted transient unit
  for every remote-build push, and a `--reattach` verb that recovers that unit's
  outcome after an operator-side disconnect.

### Modified Capabilities

(none - greenfield; builds on `xi-sources-push`'s `--mode sources` systemd
wrapping, not yet archived.)

## Impact

- **Affected code**: `crates/xi-deploy/src/push/mode_sources.rs`
  (`wrap_in_systemd_run` gains an explicit unit name),
  `crates/xi-deploy/src/push/orchestrate.rs` (receipt write/remove around the
  `switch` call), a new `crates/xi-deploy/src/push/reattach.rs`,
  `crates/xi-deploy/src/push/args.rs` (`--reattach`, `--notify`).
- **Coordination**: depends on nothing else in this effort landing first;
  `xi-sources-retry`'s classifier is the natural thing `--reattach`'s own SSH
  probe should use once it exists, so a flaky reconnect attempt does not itself
  get misread as "unit gone."
- **Breaking?**: No. The unit name change is internal (operators never typed the
  old anonymous name); everything else is additive.
- **Migration**: None.

## Success Criteria

- **SC-RB-001**: A remote-build push whose SSH session is killed mid-build
  leaves a local receipt naming the target and the unit; the unit is still
  observably running on the target via `systemctl show`.
- **SC-RB-002**: `xi push --reattach <id>` against a target where the unit
  finished successfully while disconnected reports success, removes the receipt,
  and exits zero.
- **SC-RB-003**: The same command against a unit that failed reports the failure
  (including `ExecMainStatus`) and does not remove the receipt silently - the
  operator still has to acknowledge it.
- **SC-RB-004**: `xi push --reattach` with no id lists every outstanding
  receipt; reattaching to one still running streams its journal live.
- **SC-RB-005**: `--notify <command>` runs exactly once per terminal
  observation, never on an intermediate "still running" poll.
