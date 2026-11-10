# Change: devshell-watcher-scope

## Why

The daemon's file watcher (`crates/xi-develop/src/daemon/watcher.rs`) decides
when the flake is re-evaluated. Its scope — which files trigger a re-eval — is
the single biggest lever on daemon CPU and eval cache hit rate. Watch too much
and every save re-evaluates for no reason; watch too little and the user's
`flake.nix` edit doesn't take effect until they force it.

Today the watcher's documented intent is to default to `flake.nix` and
`flake.lock`, plus every git-tracked `*.nix` file, with a
`[develop.watch]
extra` config knob for user additions. As verified below, the
implemented default is narrower than that intent. This is roughly correct
behaviour but:

- The specific default set has never been specified; a future refactor could
  quietly change it.
- The debounce / coalescing behaviour is buried in the notify plumbing and
  interacts with the state machine's `EvalStarted` gate in ways that are not
  spelled out.
- The "watcher degraded" state is defined by `async-daemon-cqrs` at the
  state-machine level, but the conditions that produce it (inotify watch-limit
  exhaustion, macOS FSEvents failure, permission errors) are not.
- On the largest monorepos, the watch set is 10k+ paths; recursive-mode choices
  and inotify limits become operationally relevant.

## Verified Against Current Code (2026-11-07)

Re-read `crates/xi-develop/src/daemon/watcher.rs` in full, including its own
unit tests:

- **Default watch set is narrower than this proposal previously claimed**:
  `DEFAULT_PATTERNS` is `["flake.nix", "flake.lock"]` only. The file's own
  `default_patterns()` test asserts `shell.nix` and `modules/foo.nix` do NOT
  match by default. "Every git-tracked `*.nix` file" is not current behaviour;
  it is one option for what the default should be, not a fact about what it is.
- **`.git/index` is watched, but changes to it do nothing**: `GitWatcher::new`
  registers a watch on `.git/index`, but `git_tracked_nix_dirs` (which derives
  the tracked-file set) is only ever called once, at construction. No code
  re-derives the set or updates registrations when `.git/index` changes.
  "Git-index awareness" is confirmed NOT implemented, not partially implemented.
- **No coalescing window exists**: `notify`'s callback sends every matching
  event straight to the channel (`event_tx.send(...)`) with no buffering, no
  timer, no 200ms window anywhere in the file.
- **`WatcherDegraded` is never triggered from this file**: the daemon's state
  machine (`state_machine.rs::TransitionEvent::WatcherDegraded`) supports the
  transition, but `watcher.rs` only ever logs watch-registration failures via
  `debug!("Cannot watch {}: {e}", ...)` and never emits a `TransitionEvent` of
  any kind. The state-machine primitive exists; the watcher side has zero wiring
  to it.

## What Changes

- Introduce capability `xi-devshell-watcher` that owns:

  - **Default watch set** (decision needed, see Assumptions): either (a) keep
    today's narrow default (`flake.nix`, `flake.lock`, plus `.git/index`) and
    correct the long-standing "every git-tracked `*.nix` file" documentation to
    match, or (b) widen the implementation to actually watch every git-tracked
    `*.nix` file as originally intended. This delta specifies (b) as the target
    requirement below, since that is the documented intent being formalised, but
    flags it as a real, not-yet-implemented behaviour change, not a current
    fact.

  - **User-extension surface**: `[develop.watch] extra = [<pattern>, ...]` in
    `.xi.toml`. Patterns are gitignore-style globs, matched against paths
    relative to the flake root. Patterns MUST be evaluated in addition to (not
    in place of) the default set.

  - **NonRecursive watch of parent directories**, not recursive watch of the
    flake root. The watcher SHOULD compute the minimal set of directories that
    contain at least one watched file and register each at
    `RecursiveMode::NonRecursive`. Recursive watching of a monorepo root can
    blow the inotify limit. (Already true today for the narrow default set.)

  - **Coalescing window** (not yet implemented): filesystem events MUST be
    coalesced within a 200 ms window before being reported to the state machine.
    Rapid save-bursts (a formatter rewriting 50 files) MUST produce at most one
    re-eval trigger per window.

  - **Degradation conditions** (not yet wired): the watcher MUST transition the
    daemon to `WatcherDegraded` (a state already defined by
    `xi-devshell-daemon`'s state machine, but never reached from `watcher.rs`
    today) when any of these conditions occur:
    - inotify watch limit exhausted (Linux; returns EMFILE or ENOSPC)
    - macOS FSEvents stream drops
    - a watched directory becomes unreadable
    - the `notify` crate returns a fatal error

  - **Recovery**: on entering `WatcherDegraded`, the daemon MUST continue to
    serve queries (state responder is unaffected; see `xi-devshell-daemon`) and
    MUST accept an explicit `InvalidateCache` command to force a re-eval. It
    MUST NOT retry watch registration in a busy loop; a single retry on the next
    command is sufficient.

  - **Git-index awareness** (not yet implemented): when `.git/index` changes
    (staging area mutated), the watcher MUST re-derive the set of git-tracked
    `*.nix` files and update its registrations. A new staged `*.nix` file MUST
    become watched without daemon restart. Today the initial watch on
    `.git/index` exists but its change events are not connected to any
    re-derivation logic.

  - **Non-goals**: watching arbitrary imported nix paths that live outside the
    flake root (via `builtins.readFile`, absolute-path imports) is out of scope.
    Users who need that add explicit patterns to `extra`.

## Impact

- **Affected specs**: `xi-devshell-watcher` (new capability; first delta
  introduces its requirements).
- **Affected code**: `crates/xi-develop/src/daemon/watcher.rs` entirely; this is
  a bigger behaviour change than "mainly coalescing" implies. Widening the
  default set (if option (b) above is chosen), adding the coalescing buffer,
  wiring `WatcherDegraded` emission, and implementing git-index re-derivation
  are four separate, currently-absent pieces of logic, not refinements of
  existing code.
- **Coordination with `xi-devshell-daemon`**: the `WatcherDegraded` variant of
  `DaemonState` and the `InvalidateCache` command are already defined by
  `async-daemon-cqrs` at the state-machine level. This spec owns the conditions
  that should trigger the transition; today nothing in `watcher.rs` calls into
  that machinery to do so.
- **Breaking?**: Mixed. The coalescing window, degradation wiring, and git-index
  re-derivation are purely additive (nothing regresses). Widening the default
  watch set to every git-tracked `*.nix` file (if chosen) is a real behaviour
  change: flakes with non-default-attribute `.nix` files that are intentionally
  not watched today will start triggering re-evals.
- **Migration**: None for the additive pieces. If the default set is widened, no
  user action is required, but daemon CPU usage may increase on repos with many
  tracked `.nix` files outside the flake root.

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
- The choice between keeping the narrow current default (`flake.nix`,
  `flake.lock`, `.git/index`) and widening it to every git-tracked `*.nix` file
  is a product decision, not a technical one; this proposal specifies the wider
  set as the target but the narrower set is also internally consistent and
  cheaper. Whichever is chosen, `SC-W-001`'s "50 tracked `.nix` files" scenario
  only applies once that decision lands in favour of the wider set.
