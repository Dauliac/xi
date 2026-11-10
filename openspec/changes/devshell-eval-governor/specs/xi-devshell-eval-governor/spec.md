# xi-devshell-eval-governor (delta)

## ADDED Requirements

### Requirement: Cross-process eval slot pool

Before spawning a background evaluation (`nix print-dev-env` initiated by the
daemon's watcher or interval logic), a devshell daemon MUST acquire one slot
from a machine-wide, per-uid pool of `max_concurrent_evals` slots. The pool MUST
be implemented as flock-backed slot files under the per-uid runtime directory
(`/tmp/xi-{uid}/eval-slots/`), created `0700`-equivalent like the existing
socket directories. A slot MUST be held for the duration of the evaluation
process and released when it exits.

#### Scenario: Two daemons, one slot

- **WHEN** `max_concurrent_evals = 1` and daemons for two distinct flake roots
  trigger background evals within the same second
- **THEN** at most one `nix print-dev-env` process spawned by any devshell
  daemon exists at any instant, and the second daemon's eval starts only after
  the first daemon's eval process exits

#### Scenario: Crash releases the slot

- **WHEN** a daemon holding a slot is killed with SIGKILL mid-eval
- **THEN** a daemon blocked waiting on that slot acquires it without any cleanup
  command being run, within one watcher poll interval of the kernel releasing
  the lock

#### Scenario: Known gap, current behaviour

- **GIVEN** `daemon/server.rs` as of 2026-11-10 gates concurrency only
  per-daemon (the `eval_in_progress` mutex) and no cross-process mechanism
  exists
- **WHEN** daemons for N flake roots trigger evals simultaneously
- **THEN** today's build spawns N concurrent evaluator processes with unbounded
  aggregate memory; this scenario documents the gap tracked in `tasks.md`

### Requirement: Foreground paths bypass the pool

User-attended evaluation paths (`xi develop enter`, an explicit force-eval
command issued by the user) MUST NOT block on slot acquisition. They MAY
register their presence (for observability) but MUST proceed immediately even
when all slots are held.

#### Scenario: Interactive entry under slot exhaustion

- **WHEN** all slots are held by background evals and the user runs
  `xi develop enter` on a trusted flake
- **THEN** the foreground evaluation starts without waiting for a slot

### Requirement: Resource scoping via shared systemd slice

When a systemd user manager is available, the daemon MUST spawn background
evaluation processes inside a transient scope attached to a shared
`xi-develop.slice`, applying the configured `MemoryMax`, `CPUWeight`, and `Nice`
values. The slice MUST be shared by all devshell daemons of the uid so the caps
bound the aggregate, not the individual process. The daemon process itself MUST
NOT run inside the capped scope (only the eval child), so query responsiveness
is never throttled by eval caps.

#### Scenario: Aggregate memory cap enforced

- **WHEN** `memory_max = "1G"` is configured and two concurrent background evals
  together exceed 1 GiB
- **THEN** the kernel kills eval process(es) per cgroup accounting, each
  affected daemon transitions to `BuildFailed` with a diagnostic that names the
  configured memory cap, and every daemon process survives and continues
  answering state queries

#### Scenario: Niceness applied

- **WHEN** `nice = 10` is configured and a background eval spawns on a systemd
  host
- **THEN** the eval process runs at niceness 10 while the daemon process keeps
  its original priority

### Requirement: Graceful degradation without systemd

When no systemd user manager is available (Darwin, non-systemd Linux, or
`systemd-run --user` fails), the daemon MUST still run evaluations, MUST apply
the configured niceness directly to the spawned child, MUST skip memory and
CPU-weight capping, and MUST log the degradation exactly once per daemon
lifetime.

#### Scenario: Non-systemd host

- **WHEN** a daemon on a host without a systemd user manager runs three
  background evals over its lifetime
- **THEN** all three evals run at the configured niceness, and exactly one log
  line reports that memory/CPU capping is unavailable

### Requirement: Governor configuration is user-scoped

Governor settings (`max_concurrent_evals`, `memory_max`, `cpu_weight`, `nice`)
MUST be read only from the user-level configuration layer and
`XI_DEVELOP_GOVERNOR_*` environment variables, with environment taking
precedence. A `[develop.governor]` table in a repository-level `.xi.toml` MUST
be ignored for these keys, and the daemon MUST log that it was ignored. The
default for `max_concurrent_evals` MUST be 2 pending the product decision
tracked in `tasks.md`.

#### Scenario: Repo cannot raise limits

- **WHEN** a repository `.xi.toml` contains
  `[develop.governor] max_concurrent_evals = 64` and the user scope sets no
  value
- **THEN** the effective slot count is the default (2), and the daemon log
  records that the repo-level governor table was ignored

#### Scenario: Environment override wins

- **WHEN** the user config sets `max_concurrent_evals = 4` and the daemon starts
  with `XI_DEVELOP_GOVERNOR_MAX_CONCURRENT_EVALS=1`
- **THEN** the effective slot count is 1

### Requirement: Queue wait is observable without wire changes

A daemon blocked on slot acquisition MUST remain in its current state-machine
state, MUST continue answering state queries within the existing state-query
budget, and MUST report the wait as additive metadata (a waiting flag and the
wait duration) in state query responses. No new `DaemonState` variant MAY be
introduced for queueing, and the shell prompt hook MUST NOT block on a queued
eval.

#### Scenario: Queued daemon stays responsive

- **WHEN** a daemon has been waiting 30 s for a slot and a client issues a state
  query
- **THEN** the response arrives within the state-query budget, carries the
  daemon's pre-wait state, and its metadata indicates a slot wait of
  approximately 30 s

### Requirement: Slot waits are interruptible and backoff-neutral

A pending slot acquisition MUST be abandoned promptly when the daemon receives a
shutdown command or when the last registered consumer deregisters. Time spent
waiting for a slot MUST NOT count as an evaluation failure and MUST NOT advance
the eval retry backoff.

#### Scenario: Last shell exits while queued

- **WHEN** the only registered shell deregisters while its daemon is queued for
  a slot
- **THEN** the daemon exits by its normal last-deregister path without spawning
  the eval and without ever holding a slot

#### Scenario: Waiting does not escalate backoff

- **WHEN** a daemon waits 120 s for a slot and the eval then succeeds
- **THEN** the daemon's backoff state is identical to an immediate success (no
  backoff escalation from the wait)

### Requirement: Governor applies strictly after trust

The governor MUST only ever act on evaluations that the trust model
(`xi-devshell-trust`) already permits. Slot acquisition, slice setup, and
configuration reads MUST NOT cause any daemon spawn, socket creation, or
evaluation for an untrusted flake root.

#### Scenario: Untrusted flake unaffected

- **WHEN** the prompt hook runs in an untrusted flake root while the slot pool
  is fully contended
- **THEN** no daemon is started, no slot is consumed or waited on, and no
  governor log output is produced for that root
