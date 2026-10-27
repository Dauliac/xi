# Capability: xi-sources-conflict

## ADDED Requirements

### Requirement: Three-ref state matrix

The capability MUST classify every machine into one of six ref states by
comparing `remote_head`, `working_head`, `sources_head` (ignoring the case where
any ref is `None`, which is the uninitialised state owned by the model spec).

| # | Working vs sources    | Sources vs remote | xi behaviour on `switch`                                  | Gitops daemon behaviour                                                                                                               |
| - | --------------------- | ----------------- | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| 1 | identical             | identical         | evaluate working dir                                      | no-op fetch tick                                                                                                                      |
| 2 | working ahead         | identical         | evaluate working dir (local wins)                         | no-op fetch tick                                                                                                                      |
| 3 | identical             | sources behind    | evaluate working dir                                      | fast-forward sources dir to remote; mark `sync_available: true` so the operator can run `xi sources sync` (hermetic sync requirement) |
| 4 | working ahead         | sources behind    | evaluate working dir (local wins)                         | fast-forward sources dir to remote; status becomes `Pending` until user resolves via `git push` or `xi sources sync --force-dirty`    |
| 5 | diverged (both moved) | any               | refuse; status `Degraded` with reason `working-diverged`  | abort tick; status `Degraded`                                                                                                         |
| 6 | identical             | sources diverged  | refuse; status `Degraded` with reason `remote-force-push` | abort tick; sources dir left on previous sha                                                                                          |

#### Scenario: Working ahead, clean tree, switch succeeds

- **GIVEN** working dir is N commits ahead of sources dir with clean tree
  (state 2)
- **WHEN** `xi {home,nixos,darwin,system} switch` runs
- **THEN** xi evaluates `nixosConfigurations.<host>` (or equivalent) from the
  working dir
- **AND** the gitops daemon's state is unchanged

#### Scenario: Diverged refuses

- **GIVEN** working dir and sources dir have both moved past their common
  ancestor (state 5)
- **WHEN** `xi switch` runs
- **THEN** xi exits non-zero with `status: Degraded, reason:
  working-diverged`
- **AND** no evaluation starts

#### Scenario: Remote force push is rejected

- **GIVEN** the gitops daemon fetches a remote whose current head is not a
  descendant of the recorded `sources_head` (state 6)
- **WHEN** the tick runs
- **THEN** the sources dir is left on the previous sha
- **AND** `xi sources status` reports `Degraded, reason: remote-force-push`
- **AND** the daemon logs the rejected sha for operator review

### Requirement: Dirty-tree detection

A working dir is **dirty** if any of the following hold:

- `git status --porcelain=v2` prints any line
- `.git/MERGE_HEAD`, `.git/REBASE_HEAD`, or `.git/CHERRY_PICK_HEAD` exist
- a non-empty `git stash list`

`xi switch` MUST refuse to evaluate a dirty working dir without the
`--force-dirty` flag. The refusal message MUST list the dirty files (truncated
to 20 with a count of the rest) and the escape-hatch command.

#### Scenario: Dirty tree refused

- **GIVEN** the working dir has an unstaged edit to `flake.nix`
- **WHEN** `xi home switch` runs without `--force-dirty`
- **THEN** xi exits non-zero with a message naming `flake.nix` and the
  `--force-dirty` flag
- **AND** no evaluation starts

#### Scenario: `--force-dirty` evaluates working dir as-is

- **GIVEN** the working dir has unstaged edits
- **WHEN** `xi home switch --force-dirty` runs
- **THEN** xi evaluates the working dir including the unstaged edits
- **AND** the subsequent generation is marked `dirty-source` in the state dir's
  `active-generation` entry

### Requirement: Daemon never touches working dir

The gitops daemon's write operations MUST assert that the write target path is a
prefix of the sources dir. Writing anywhere else - including the working dir,
the state dir's non-metadata areas, or the user's `$HOME`

- MUST be a panic in debug builds and a logged non-fatal error in release builds
  that aborts the current tick.

#### Scenario: Daemon write to working dir aborts tick

- **GIVEN** a bug in future code attempts to write to the working dir from
  within a daemon tick
- **WHEN** the tick runs (release build)
- **THEN** the write is rejected, the tick aborts, and the error is logged with
  the attempted path

### Requirement: `xi sources diff`

The subcommand `xi sources diff` MUST print a three-way diff between
`remote_head`, `sources_head`, and `working_head`. When any ref is `None` the
diff reduces to the subset that is available (e.g. no working dir → two-way
diff). The output body is `git diff`'s standard format; the house response
envelope wraps the exit metadata.

#### Scenario: Three-way diff with all refs present

- **GIVEN** all three refs are known and distinct
- **WHEN** `xi sources diff` runs
- **THEN** the output contains three sections labelled `remote → sources`,
  `sources → working`, `remote → working`
- **AND** the response envelope reports `status: Ready` if diffs succeeded,
  `Degraded` with reason if a ref was unreachable

### Requirement: Hermetic one-way sync from sources dir to working dir

The capability MUST provide a verb `xi sources sync` that performs a one-way
copy-with-erase from the sources dir to the working dir. The flow mirrors
chezmoi's `apply` verb (source holds the authority, the destination gets
rewritten to match). xi uses this verb instead of `git pull` in the working dir
because `git pull` into a user-managed working tree creates merge conflicts the
user never asked for and pulls files the working tree never wanted to track.

The sync direction is fixed. Working dir to sources dir is NOT a sanctioned
flow; the only reverse path is `git push` from the working dir plus the daemon's
next fetch into the sources dir.

The copy semantics are:

1. For every path tracked at `sources_head` in the sources dir, write that file
   to the matching path under the working dir, replacing any current content.
2. For every path tracked at the previous `working_head` but NOT present at
   `sources_head`, delete that file from the working dir.
3. The working dir's `.git` directory and any untracked files outside the
   tracked set MUST NOT be touched.
4. After step 3, set the working dir's HEAD to `sources_head` via
   `git reset --soft`, so the operator can inspect, amend, or immediately commit
   a follow-up without a new fetch.

The verb MUST refuse a dirty working dir without `--force-dirty` (same semantics
as [[Dirty-tree detection]]). With `--force-dirty` the sync proceeds as above;
the operator's unstaged edits are lost without recovery.

#### Scenario: Clean sync advances the working dir

- **GIVEN** sources dir is at sha S, working dir is at sha W where W is an
  ancestor of S (state 3), and the working dir is clean
- **WHEN** `xi sources sync` runs
- **THEN** every file tracked at S exists in the working dir with the S content
- **AND** every file tracked at W but absent at S has been deleted from the
  working dir
- **AND** the working dir's `.git/HEAD` resolves to S
- **AND** the operator's `$HOME` outside the working dir is byte-identical to
  its pre-sync state

#### Scenario: Dirty sync refused

- **GIVEN** the working dir has an unstaged edit
- **WHEN** `xi sources sync` runs without `--force-dirty`
- **THEN** no files are modified; exit non-zero with a message naming the dirty
  files and the `--force-dirty` flag

#### Scenario: Reverse direction is not implemented

- **GIVEN** the working dir has advanced past the sources dir with committed
  local changes
- **WHEN** the operator looks for a sources-dir-to-working-dir reverse sync
- **THEN** no such verb exists; `xi sources status` reports the working-ahead
  state and names `git push origin` as the only path

### Requirement: `sync_mode` config key

The capability MUST honour a `sync_mode` config key under [[xi-sources-config]]
with exactly three values:

- `prompt` (default): on sources advance, the daemon marks
  `sync_available: true` and waits for `xi sources sync`.
- `auto-copy`: on sources advance, the daemon runs the equivalent of
  `xi sources sync` on the next interactive xi command, provided the working dir
  is clean. If dirty, the daemon logs and does nothing.
- `never`: on sources advance, the daemon logs the available sync and never
  acts; `xi sources status` still surfaces the available sha.

#### Scenario: `sync_mode = "prompt"` leaves working dir intact

- **GIVEN** `sync_mode = "prompt"` and the daemon has advanced the sources dir
- **WHEN** the next interactive xi command runs
- **THEN** the working dir is byte-identical to its pre-advance state
- **AND** `xi sources status` reports `sync_available: true` with the new and
  old shas

#### Scenario: `sync_mode = "auto-copy"` applies on next clean command

- **GIVEN** `sync_mode = "auto-copy"`, the daemon has advanced the sources dir,
  and the working dir is clean
- **WHEN** any interactive xi command runs
- **THEN** the sync completes before the command's main action
- **AND** the working dir is now at `sources_head`

#### Scenario: `sync_mode = "auto-copy"` refuses silently on dirty

- **GIVEN** `sync_mode = "auto-copy"` and a dirty working dir
- **WHEN** any interactive xi command runs
- **THEN** no sync happens; the working dir is unchanged
- **AND** the daemon log records a `sync-deferred-dirty` event; the main command
  still runs

### Requirement: Daemon never calls `git pull` or `git merge` in the working dir

The daemon's only sanctioned write path into the working dir is via the hermetic
sync defined above. The daemon MUST NOT invoke `git pull`, `git fetch` writing
refs into the working dir's `.git`, `git merge`, `git rebase`, or any plumbing
equivalent in the working dir. All of those commands apply to the sources dir
only.

#### Scenario: Daemon fetch into working dir forbidden

- **GIVEN** a bug attempts `git -C <working_dir> fetch` from inside the daemon
- **WHEN** the attempt is made (release build)
- **THEN** the write is rejected before `exec`, the tick aborts, and the
  attempted command is logged
