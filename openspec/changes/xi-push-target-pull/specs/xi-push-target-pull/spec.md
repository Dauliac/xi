# Capability: xi-push-target-pull

## ADDED Requirements

### Requirement: A target can be ordered to pull its own sources

When a target's `sources-transport` resolves to `target-pull`, `xi push` MUST
reach that target's gitops daemon control socket (via a local relay run over
SSH) and send an `AdvanceNow` request naming the sha to advance to, instead of
copying a closure or syncing a directory from the operator. The daemon MUST
perform the fetch and fast-forward using the same resolve/fast-forward step its
regular tick already uses.

#### Scenario: A target-pull push fetches instead of receiving bytes

- **GIVEN** a target configured `sources-transport = "target-pull"` with a
  reachable gitops daemon
- **WHEN** `xi push` runs against it
- **THEN** the target's daemon fetches from its own configured remote and
  fast-forwards, and the operator's process never calls `copy_closure` or
  `sync_dir` for that target

#### Scenario: A non-fast-forward advance is refused

- **GIVEN** an `AdvanceNow { to_sha }` naming a sha that is not a descendant of
  the target's current `sources_head`
- **WHEN** the daemon receives the request
- **THEN** it replies `AdvanceReply::NotFastForward` and does not advance, using
  the identical check its own tick loop performs on every poll

#### Scenario: A concurrent tick reports busy rather than blocking

- **GIVEN** the daemon's tick is already in flight
- **WHEN** an `AdvanceNow` request arrives
- **THEN** it replies `AdvanceReply::Busy` immediately, without deferring or
  blocking the caller

### Requirement: An unreachable daemon refuses rather than silently falls back

`xi push` against a target configured `target-pull` where the local relay cannot
connect to the target's control socket (no daemon running, or the socket does
not exist) MUST refuse the push with an error naming `operator-copy` as the
available fallback, and MUST NOT silently retry using `operator-copy` on the
same invocation. This is a push-side classification of the connection failure,
not a reply from the daemon - there is nothing listening to reply.

#### Scenario: No daemon on the target

- **GIVEN** a target configured `sources-transport = "target-pull"` with no
  daemon listening on its control socket
- **WHEN** `xi push` runs against it
- **THEN** it exits non-zero, naming `operator-copy` as the fallback the
  operator can choose explicitly, rather than switching to it on its own

### Requirement: Transport selection follows the same ladder shape as other push settings

`sources-transport` MUST resolve through: an explicit CLI flag, then a per-host
`.xi.toml [[push.hosts]]` override, then the table-level
`.xi.toml [push] sources-transport` default, then the compiled floor
`operator-copy`.

#### Scenario: The floor is operator-copy

- **GIVEN** no CLI flag and no `.xi.toml` entry for a target
- **WHEN** `xi push` resolves its transport
- **THEN** it resolves `operator-copy`, identical to today's behavior

#### Scenario: A per-host override wins over the table default

- **GIVEN** a table-level `sources-transport = "operator-copy"` and a per-host
  override of `target-pull` for one target
- **WHEN** `xi push` resolves that target's transport
- **THEN** it resolves `target-pull`
