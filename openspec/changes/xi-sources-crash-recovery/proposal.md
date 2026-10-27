# Change: xi-sources-crash-recovery

## Why

Every mutating actor in xi-sources (daemon, switch, push, bootstrap, sync,
cache-prestage) can be killed between the first and last line of its critical
section. Causes vary: OOM-killer, operator Ctrl+C, SIGTERM from a package
upgrade, reboot during build, laptop lid closed, remote SSH drop mid-push.

Current specs assume success-path semantics. In failure, the sources dir may
hold a partial checkout, the state dir may name a sha that was never actually
realised, the `xi.conf` include may point at a file that no longer exists, the
receipt may claim an action that was rolled back. Any of these produces subtle
breakage on next invocation.

This change names the recovery contract. Every actor declares its transaction
boundary, uses atomic rename for state dir writes, keeps a `tick-in-progress`
marker so a restart detects abandoned work, and offers a `xi sources repair`
verb that fixes recoverable damage without hitting the remote.

## What Changes

- Introduce capability `xi-sources-crash-recovery` that owns:
  - A **transaction boundary** per actor. Every write path has three phases:
    begin (write a `.transaction` sentinel into state dir), body (do the work,
    writing to shadow paths), commit (atomic rename of shadow into place, remove
    sentinel).
  - A **shadow-write rule** for the sources dir: fetches land into
    `<sources_dir>.incoming/.git` first; a successful fetch atomically renames
    `<sources_dir>.incoming` into `<sources_dir>`. A crash mid-fetch leaves only
    the incoming dir, which the next actor deletes on startup.
  - A **state-dir metadata atomicity rule**: every file in the state dir is
    written via `write -> rename` on the same filesystem. Readers never see
    half-written content.
  - A **`xi sources repair`** verb that:
    1. Deletes dangling `.incoming/` shadow dirs.
    2. Rolls back any `.transaction` sentinel whose owning actor is dead (per
       [[xi-sources-race]] liveness check).
    3. Verifies the sources dir sha matches `state_dir/sources-head`; on
       mismatch, prefers the sha of the actual on-disk HEAD and updates state.
    4. Verifies the `xi.conf` include line in `nix.conf` points at an existing
       file; on dangling, removes the include per the
       [[xi-sources-cache-prestage]] receipt.
    5. Reports every repair action; non-destructive when `--dry-run` is passed.
  - An **auto-repair on startup** rule: the gitops daemon runs
    `xi sources repair` as its first action on startup, before acquiring the
    lock. Any destructive repair logs a warning. The repair itself is
    idempotent.
  - A **receipt consistency rule** for [[xi-sources-bootstrap]] and
    [[xi-sources-cache-prestage]]: each receipt entry is written AFTER its
    action succeeds, inside the actor's transaction boundary. A crash between
    action success and receipt write is recoverable: `xi sources repair`
    compares the on-disk state with the receipt and reconstructs the missing
    entry.

## Impact

- **Affected specs**: `xi-sources-crash-recovery` (new capability).
- **Affected code (planned)**: a transaction wrapper under
  `crates/xi-sources/src/transaction.rs`; integration in every actor's write
  path; a new `xi sources repair` subcommand.
- **Coordination**: every actor's write-path section is amended to wrap its
  critical section with a transaction. [[xi-sources-race]]'s lock protocol runs
  OUTSIDE the transaction (lock first, then transaction, then unlock).
- **Breaking?**: No. Pre-upgrade installs without a transaction sentinel are
  treated as "no in-progress transaction" by the repair logic.
- **Migration**: None. Repair is idempotent and safe to run.

## Success Criteria

- **SC-Y-001**: Daemon killed mid-fetch leaves `<sources_dir>.incoming` on disk
  and the sources dir unchanged; next startup's auto-repair deletes the shadow
  and logs the recovery.
- **SC-Y-002**: Daemon killed between fetch success and `sources-head` state
  write leaves no mismatch: the sources dir HEAD is the new sha, the state file
  still names the old sha; the next invocation detects the mismatch and corrects
  state.
- **SC-Y-003**: Bootstrap killed mid-phase leaves a receipt file listing the
  completed phases; `xi system uninstall` reverses exactly those phases.
- **SC-Y-004**: Cache-prestage killed between writing `xi.conf` and amending
  `nix.conf` leaves a dangling `xi.conf` without a reference;
  `xi sources repair` deletes the dangling file.
- **SC-Y-005**: Sync killed mid-copy leaves the working dir in an intermediate
  state; `xi sources repair` detects the sync-in-progress sentinel, prompts the
  operator, and either resumes or rolls back via `git reset --hard HEAD`.
- **SC-Y-006**: `xi sources repair --dry-run` prints the planned repair actions
  without executing any of them.
- **SC-Y-007**: Repair is idempotent: running it twice in a row with no new
  changes produces no output on the second run.
- **SC-Y-008**: Daemon's first action on startup is auto-repair; a repair log
  line precedes any tick.
