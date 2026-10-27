# Change: devshell-watcher-scope

## Why

The daemon's file watcher (`crates/xi-develop/src/daemon/watcher.rs`) decides
when the flake is re-evaluated. Its scope — which files trigger a re-eval — is
the single biggest lever on daemon CPU and eval cache hit rate. Watch too much
and every save re-evaluates for no reason; watch too little and the user's
`flake.nix` edit doesn't take effect until they force it.

Today the watcher defaults to `flake.nix` and `flake.lock`, plus every
git-tracked `*.nix` file, with a `[develop.watch] extra` config knob for user
additions. This is roughly correct behaviour but:

- The specific default set has never been specified — a future refactor could
  quietly change it.
- The debounce / coalescing behaviour is buried in the notify plumbing and
  interacts with the state machine's `EvalStarted` gate in ways that are not
  spelled out.
- The "watcher degraded" state is defined by `async-daemon-cqrs` at the
  state-machine level, but the conditions that produce it (inotify watch-limit
  exhaustion, macOS FSEvents failure, permission errors) are not.
- On the largest monorepos, the watch set is 10k+ paths; recursive-mode choices
  and inotify limits become operationally relevant.

## What Changes

- Introduce capability `xi-devshell-watcher` that owns:

  - **Default watch set**: `flake.nix`, `flake.lock`, every git-tracked file
    matching `*.nix`, plus `.git/index` (to detect staging changes that could
    add or remove tracked `*.nix` files).

  - **User-extension surface**: `[develop.watch] extra = [<pattern>, ...]` in
    `.xi.toml`. Patterns are gitignore-style globs, matched against paths
    relative to the flake root. Patterns MUST be evaluated in addition to (not
    in place of) the default set.

  - **NonRecursive watch of parent directories**, not recursive watch of the
    flake root. The watcher SHOULD compute the minimal set of directories that
    contain at least one watched file and register each at
    `RecursiveMode::NonRecursive`. Recursive watching of a monorepo root can
    blow the inotify limit.

  - **Coalescing window**: filesystem events are coalesced within a 200 ms
    window before being reported to the state machine. Rapid save-bursts (a
    formatter rewriting 50 files) produce at most one re-eval trigger per
    window.

  - **Degradation conditions**: the watcher MUST transition the daemon to
    `WatcherDegraded` (a state defined by `xi-devshell-daemon`) when any of
    these conditions occur:
    - inotify watch limit exhausted (Linux; returns EMFILE or ENOSPC)
    - macOS FSEvents stream drops
    - a watched directory becomes unreadable
    - the `notify` crate returns a fatal error

  - **Recovery**: on entering `WatcherDegraded`, the daemon MUST continue to
    serve queries (state responder is unaffected — see `xi-devshell-daemon`) and
    MUST accept an explicit `InvalidateCache` command to force a re-eval. It
    MUST NOT retry watch registration in a busy loop; a single retry on the next
    command is sufficient.

  - **Git-index awareness**: when `.git/index` changes (staging area mutated),
    the watcher MUST re-derive the set of git-tracked `*.nix` files. A new
    staged `*.nix` file becomes watched without daemon restart.

  - **Non-goals**: watching arbitrary imported nix paths that live outside the
    flake root (via `builtins.readFile`, absolute-path imports) is out of scope.
    Users who need that add explicit patterns to `extra`.

## Impact

- **Affected specs**: `xi-devshell-watcher` (new capability — first delta
  introduces its requirements).
- **Affected code**: `crates/xi-develop/src/daemon/watcher.rs` mainly.
  Coalescing may require a small buffer between the notify callback and the
  state-machine event channel.
- **Coordination with `xi-devshell-daemon`**: the `WatcherDegraded` variant of
  `DaemonState` and the `InvalidateCache` command are defined by
  `async-daemon-cqrs`. This spec owns the _conditions_ that trigger the
  transition; the daemon owns the transition itself.
- **Breaking?**: No. Default watch set matches current behaviour; the coalescing
  window and degradation conditions codify current intent.
- **Migration**: None.

## Success Criteria

- **SC-W-001**: A `nix fmt` run that rewrites 50 tracked `.nix` files triggers
  exactly one re-eval (not fifty), consistent with the 200 ms coalescing window.
- **SC-W-002**: Adding a new file via `git add path/foo.nix` (which mutates
  `.git/index`) causes subsequent modifications to `foo.nix` to trigger
  re-evals, without requiring a daemon restart.
- **SC-W-003**: On a Linux system with `fs.inotify.max_user_watches` set to 8,
  opening a devshell on a flake with more than 8 watched directories transitions
  the daemon to `WatcherDegraded` (not a crash), and queries still return within
  the state-query budget.
- **SC-W-004**: In `WatcherDegraded` state, running any xi command that issues
  `InvalidateCache` triggers a single re-eval; the watcher attempts one
  re-registration on that command.
- **SC-W-005**: Adding `extra = ["Cargo.lock"]` to `[develop.watch]` causes
  `Cargo.lock` modifications to trigger re-eval; removing it reverts to default
  behaviour on the next daemon start.
- **SC-W-006**: The watcher registers at most one directory per unique
  parent-dir of a watched file (not one per file); a flake root with 100 tracked
  `.nix` files spread across 4 subdirectories consumes at most 5 inotify watches
  (4 subdirs + `.git/index`).

## Assumptions

- `notify` remains the underlying watch library; its abstraction over inotify
  (Linux), FSEvents (macOS), and Kqueue (BSD) is sufficient.
- Watched files live inside the flake root; imported paths outside are
  intentionally not tracked (a change to `/etc/nix/config.nix` won't
  re-trigger). Users who need that add explicit patterns.
- `.git/index` is a reliable proxy for the set of tracked files; worktrees and
  submodules that use a different index location are out of scope for v1.
- 200 ms is a reasonable coalescing floor for "user finished a save burst"; this
  is a heuristic and configurable via `[develop.watch] coalesce_ms` in a
  follow-up.
- Users may configure very large `extra` patterns; the watcher does not cap them
  but reports the total watched-directory count at daemon startup so that
  operators can spot pathologies.
