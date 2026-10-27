# Capability: xi-sources-crash-recovery

## ADDED Requirements

### Requirement: Transaction boundary per actor

Every actor that mutates the sources dir or state dir MUST wrap its critical
section in a transaction scope that:

1. Writes `<state_dir>/transaction.json` with
   `{actor, intent, started_at: iso8601, pid}` at begin.
2. Performs the work on shadow paths (see shadow-write rule).
3. On success, atomically renames shadows into place AND removes the transaction
   sentinel.
4. On panic or operator abort, calls `rollback()` which removes the shadow paths
   and the sentinel.

The sentinel is observable by `xi sources repair` to detect abandoned
transactions.

#### Scenario: Clean commit removes sentinel

- **GIVEN** actor A completes its transaction successfully
- **WHEN** the transaction scope exits
- **THEN** `<state_dir>/transaction.json` is absent
- **AND** all shadow paths have been renamed into place

#### Scenario: Panic triggers rollback

- **GIVEN** actor A's body panics
- **WHEN** the Drop impl runs
- **THEN** shadow paths are removed and the sentinel is deleted
- **AND** the sources dir is byte-identical to pre-transaction

### Requirement: Shadow-write for the sources dir

Fetches MUST land into `<sources_dir>.incoming/.git` first via a git subprocess
with the alternate `--git-dir` argument. The working dir of `.incoming` is
populated via the same mechanism as the current sources dir.

On fetch success the actor MUST:

1. Rename `<sources_dir>` to `<sources_dir>.previous` (atomic).
2. Rename `<sources_dir>.incoming` to `<sources_dir>` (atomic).
3. Remove `<sources_dir>.previous` recursively (non-atomic cleanup; safe because
   readers see the new sources dir already).

On fetch failure the actor MUST remove `<sources_dir>.incoming`.

#### Scenario: Mid-fetch crash leaves only shadow

- **GIVEN** a SIGKILL at any point before step 1 above
- **WHEN** `xi sources repair` runs
- **THEN** `<sources_dir>.incoming` is detected and deleted
- **AND** `<sources_dir>` is unchanged

#### Scenario: Mid-rename crash recovered

- **GIVEN** a crash after step 1 but before step 2
- **WHEN** `xi sources repair` runs
- **THEN** `<sources_dir>.previous` is renamed back to `<sources_dir>`
  (recovery), `.incoming` is deleted
- **AND** the state file `sources-head` matches the restored content

### Requirement: Atomic state-dir metadata writes

Every write to a file under `<state_dir>` MUST go through a helper that:

1. Writes content to `<path>.tmp.<pid>` on the same filesystem.
2. `fsync`s the temp file.
3. `rename`s the temp file over `<path>`.
4. `fsync`s the parent directory.

Readers of the file MUST handle "file missing" as a legitimate state (returning
`None`), never as an error.

#### Scenario: Concurrent reader sees whole file

- **GIVEN** a writer actor refreshing `sources-head`
- **WHEN** a reader opens `sources-head` concurrently
- **THEN** the reader sees either the old whole content or the new whole
  content, never a mix

### Requirement: `xi sources repair` verb

The capability MUST provide a `xi sources repair` subcommand with these phases:

1. **Discover**: enumerate recoverable damage without side-effects:
   - Dangling `<sources_dir>.incoming` or `.previous`
   - Dangling `<state_dir>/transaction.json` whose pid is dead
     ([[xi-sources-race]] liveness)
   - `state_dir/sources-head` disagreeing with
     `git -C <sources_dir> rev-parse HEAD`
   - `nix.conf` `!include xi.conf` pointing at a missing file
   - `<state_dir>/sync.in-progress` sentinel left by a crashed sync
2. **Plan**: emit a JSON plan of repair actions.
3. **Execute**: perform each action; emit the final report.

`--dry-run` prints the plan without executing. Repair itself runs inside its own
transaction so a crash of repair is itself recoverable.

#### Scenario: Dry-run reports without acting

- **GIVEN** a dangling `.incoming` dir
- **WHEN** `xi sources repair --dry-run` runs
- **THEN** the output includes `action: remove-incoming` with the path
- **AND** the dir is still present on disk

#### Scenario: Repair is idempotent

- **GIVEN** `xi sources repair` has just run successfully
- **WHEN** `xi sources repair` runs again with no new damage
- **THEN** the discover phase finds nothing
- **AND** the output reports `no-op`

### Requirement: Daemon auto-repair on startup

The gitops daemon's first action on startup, BEFORE acquiring the lock, MUST be
an invocation of the repair logic. The daemon MUST log the repair report before
entering the tick loop.

Any destructive action by auto-repair MUST produce a WARN-level log line naming
the action.

#### Scenario: Startup repair precedes first tick

- **GIVEN** the daemon starts fresh after a crash
- **WHEN** the daemon main runs
- **THEN** the first log lines are the repair plan and report
- **AND** no fetch has started

### Requirement: Receipt consistency rule

Receipts owned by [[xi-sources-bootstrap]] and [[xi-sources-cache-prestage]]
MUST be written INSIDE the actor's transaction, as the last operation before
commit. A crash between the action's physical effect and the receipt append MUST
be recoverable by `xi sources repair` reconstructing the entry from the on-disk
evidence (e.g. the presence of `/nix/store` entries, the `!include xi.conf`
line).

Reconstructed entries MUST carry a `reconstructed: true` flag and a
`reconstructed_at` timestamp so an operator can audit them.

#### Scenario: Reconstructed receipt reversible

- **GIVEN** a crash left bootstrap's receipt missing the `InstallNix` entry but
  Nix is installed
- **WHEN** `xi sources repair` runs
- **THEN** the receipt gains a reconstructed `InstallNix` entry
- **AND** `xi system uninstall` honours it
