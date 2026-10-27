# Change: xi-sources-conflict

## Why

The user stated the conflict policy in one line: _"You have to manage the
conflicts the current sources are always right. And if you are in a local non
pushed version that is OK, it's a normal state. And once the remote is pushed."_

Translating that into a specification requires naming the states, the
transitions, and what "current sources are always right" means when a gitops
daemon fetches a remote that disagrees with the local working dir. Comin dodges
the question by maintaining its own clone and never touching the user's working
dir (prior art notes: `openspec/research/xi-sources/pull-gitops.md`, step 2). xi
inherits that split: sources dir and working dir are two different clones, and
the conflict policy governs how the gitops daemon's writes to the sources dir
interact with the user's independent commits in the working dir.

## What Changes

- Introduce capability `xi-sources-conflict` that owns:
  - A **three-ref model**: `remote_head`, `working_head`, `sources_head`
    (defined by [[xi-sources-model]]).
  - A **local-wins policy** for interactive subcommands: when the user runs
    `xi {nixos,home,darwin,system} switch`, xi evaluates from the WORKING DIR if
    it is configured and non-dirty; otherwise from the sources dir. Local
    commits on the working dir SHALL NOT be overwritten by the gitops daemon.
  - A **daemon never touches the working dir** rule, mirroring comin's pattern
    (research doc, "Who holds the clone?" section).
  - A classification of six (sources, working, remote) ref states and the xi
    behaviour for each - the full matrix is in the spec.
  - A **dirty-tree detection** rule for the working dir: a working dir with
    unstaged changes, untracked files matching tracked paths, or an in-progress
    rebase/merge is "dirty" and `xi switch` refuses to proceed without
    `--force-dirty`.
  - A **hermetic one-way sync from sources dir to working dir** modelled on
    chezmoi's `apply` verb. The user asked for this explicitly: naive `git pull`
    into the working dir would introduce merge conflicts or pull unwanted files.
    Instead, when the gitops daemon advances the sources dir past the working
    dir, xi offers a copy-with-erase from sources into the working dir, driven
    by the `sync_mode` config key:
    - `prompt` (default): xi notifies via the house status surface and waits for
      `xi sources sync` to confirm.
    - `auto-copy`: xi performs the copy-with-erase automatically on the next
      interactive command, after confirming the working dir is not dirty.
    - `never`: xi logs the available sync but never acts. The copy is **sources
      dir to working dir, one direction only**. The reverse flow (working dir to
      sources dir) exists only through `git push origin && daemon fetch`, which
      is the user's normal "I pushed, now the sources dir and working dir agree
      again" cycle.
  - A **positioning note**: xi becomes a hybrid of three patterns. Pull-mode
    gitops (comin's write path into sources dir), push-mode deploy
    ([[xi-sources-push]] into remote hosts), and chezmoi-style
    source-to-destination apply (the hermetic sync defined here).

## Impact

- **Affected specs**: `xi-sources-conflict` (new capability).
- **Affected code (planned)**: `crates/xi-sources/src/conflict.rs`,
  state-machine helpers; integration points in each verb crate that calls into
  the resolver before invoking the backend.
- **Coordination**: [[xi-sources-model]] (ref types), [[xi-sources-gitops]] (the
  daemon enforces this policy on every tick), [[xi-sources-bootstrap]] (initial
  clone establishes the "fresh" state).
- **Breaking?**: No. On a machine with no working dir configured, behaviour
  reduces to "sources is the only authority" - identical to today's implicit
  model.

## Success Criteria

- **SC-X-001**: With a configured working dir ahead of remote by N commits and
  clean tree, `xi switch` evaluates the working dir and does NOT invoke the
  gitops daemon in-line.
- **SC-X-002**: With a configured working dir having unstaged changes,
  `xi switch` refuses with a message naming the dirty files and the
  `--force-dirty` escape hatch.
- **SC-X-003**: Gitops daemon's fetch of a remote that has advanced never
  mutates the working dir, even when the working dir lags.
- **SC-X-004**: Gitops daemon's fetch of a remote that has diverged from the
  sources dir (force push) is REJECTED by default; status becomes `Degraded`
  with a human-readable reason, and the sources dir is left on the previous sha.
- **SC-X-005**: With no working dir configured, `xi switch` reads from the
  sources dir; conflict is undefined (sources is the single authority).
- **SC-X-006**: `xi sources diff` prints the three-way diff (remote vs sources
  vs working) using the house envelope.
- **SC-X-007**: After the daemon advances the sources dir past the working dir
  with `sync_mode = "prompt"`, `xi sources status` reports
  `sync_available: true` with the new sources sha and the stale working sha. No
  file in the working dir has been modified.
- **SC-X-008**: `xi sources sync` with a clean working dir performs a
  copy-with-erase from sources dir to working dir: every tracked file in the
  sources dir is written to the working dir replacing any current content; files
  tracked by the sources dir but absent in the sources tree are deleted from the
  working dir. The working dir's `.git` directory is NOT touched; HEAD is
  advanced to match `sources_head`.
- **SC-X-009**: `xi sources sync` with a dirty working dir refuses without
  `--force-dirty`; the error names the dirty files.
- **SC-X-010**: `sync_mode = "auto-copy"` performs SC-X-008 on the next
  interactive xi command when the working dir is clean, and refuses silently
  (logs only) when the working dir is dirty.
- **SC-X-011**: `sync_mode = "never"` reports `sync_available: true` via status
  but never writes to the working dir, regardless of cleanliness.
- **SC-X-012**: `xi sources sync` never touches files outside the working dir.
  In particular, the operator's `$HOME` outside the working dir, the sources
  dir, and the state dir are byte-identical before and after the sync.
- **SC-X-013**: The reverse direction (working dir to sources dir) is NOT
  implemented by `xi sources sync`. The only sanctioned reverse path is
  `git push` from the working dir, followed by the daemon's next fetch into the
  sources dir.
