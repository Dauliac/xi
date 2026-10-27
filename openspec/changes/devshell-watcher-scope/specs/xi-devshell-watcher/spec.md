# xi-devshell-watcher (delta)

## ADDED Requirements

### Requirement: Default watched file set

The watcher MUST, by default, watch `flake.nix` and `flake.lock` at the flake
root plus every git-tracked file matching `*.nix` inside the flake root, plus
`.git/index` (to detect staged-set changes). No other files MAY be watched by
default.

#### Scenario: Default set on a fresh flake

- **WHEN** a daemon starts on a flake root containing `flake.nix`, `flake.lock`,
  three tracked `*.nix` files in one subdirectory, and no configuration
  overrides
- **THEN** the watcher's effective watch set contains exactly those five files
  plus `.git/index`

### Requirement: User-extended watch patterns

A user configuration key `[develop.watch] extra = [<pattern>, ...]` in
`.xi.toml` at the flake root MUST cause the listed patterns (gitignore-style
globs relative to the flake root) to be added to the watched set in addition to
the default set. Extra patterns MUST NOT replace the default set.

#### Scenario: Extra pattern additive

- **WHEN** `[develop.watch] extra = ["Cargo.lock", "*.yaml"]` is set and the
  daemon starts
- **THEN** modifications to `Cargo.lock` and to any `.yaml` file inside the
  flake root trigger re-eval, and default modifications (`flake.nix`,
  `flake.lock`, tracked `.nix` files) also continue to trigger re-eval

#### Scenario: Empty extra list is a no-op

- **WHEN** `[develop.watch] extra = []` is set
- **THEN** the effective watch set is identical to the default set

### Requirement: NonRecursive watch of minimal parent-dir set

The watcher MUST compute the minimal set of unique parent directories that
contain at least one watched file and MUST register each at
`RecursiveMode::NonRecursive`. It MUST NOT register the flake root recursively.

#### Scenario: Minimal directory registration

- **WHEN** the watched-file set spans 100 files across 4 distinct
  subdirectories, plus `.git/index`
- **THEN** the watcher registers exactly 5 directory watches (the 4 distinct
  parent directories plus the `.git` directory containing `index`), each at
  `RecursiveMode::NonRecursive`

### Requirement: Coalescing window for save-bursts

Filesystem events reported by the underlying notify implementation MUST be
coalesced within a bounded window (default 200 ms) before being forwarded as a
re-eval trigger to the state machine.

#### Scenario: Formatter rewrites many files

- **WHEN** a formatter rewrites 50 watched `.nix` files inside a 100 ms window
- **THEN** the watcher forwards exactly one re-eval trigger for that window,
  containing the union of affected paths

#### Scenario: Sparse edits produce one trigger each

- **WHEN** a user modifies a single watched file every 5 s over 30 s
- **THEN** the watcher forwards six re-eval triggers, one per modification (each
  in its own coalescing window)

### Requirement: Degradation on watch failure

The watcher MUST transition the daemon to `WatcherDegraded` (a state owned by
`xi-devshell-daemon`) when any of the following occur: the underlying inotify
(or platform equivalent) returns an "out of watch capacity" error (EMFILE,
ENOSPC), the macOS FSEvents stream drops, a watched directory becomes
unreadable, or the `notify` crate returns a fatal error.

#### Scenario: inotify watch-limit exhaustion

- **WHEN** a Linux daemon starts on a flake whose default watch set requires
  more watches than `fs.inotify.max_user_watches` allows
- **THEN** the daemon transitions to `WatcherDegraded` and does NOT crash; state
  queries continue to answer within the state-query budget defined by
  `xi-devshell-daemon`

#### Scenario: Watched directory unreadable

- **WHEN** a watched directory's permissions are changed to `0000` after daemon
  startup
- **THEN** the watcher transitions to `WatcherDegraded` on the next read attempt
  for that directory

### Requirement: Recovery on explicit invalidation

When the daemon is in `WatcherDegraded`, it MUST accept an `InvalidateCache`
command (defined by `xi-devshell-daemon`) that forces one re-evaluation and
attempts one re-registration of the watcher. It MUST NOT retry watch
registration in a busy loop.

#### Scenario: Invalidate recovers a degraded watcher

- **WHEN** the daemon is `WatcherDegraded` and a client issues `InvalidateCache`
- **THEN** the daemon triggers one re-eval, attempts exactly one re-registration
  of the watcher, and either transitions back to `Ready` (registration
  succeeded) or remains `WatcherDegraded` (registration failed again)

#### Scenario: No busy-loop retry

- **WHEN** the watcher is degraded and no client sends `InvalidateCache`
- **THEN** the daemon does not attempt watcher re-registration on any timer or
  interval; the degraded state persists until an explicit invalidation

### Requirement: Git-index awareness

The watcher MUST watch `.git/index` and, on any modification, MUST re-derive the
set of git-tracked `*.nix` files and update its registrations accordingly. New
tracked files become watched without daemon restart; removed tracked files are
unwatched.

#### Scenario: Newly staged file becomes watched

- **WHEN** a user runs `git add foo.nix` and then modifies `foo.nix`
- **THEN** the modification triggers a re-eval within the coalescing window,
  without requiring the daemon to restart

#### Scenario: Untracked file becomes ignored

- **WHEN** a user runs `git rm --cached bar.nix` and then modifies `bar.nix`
- **THEN** the modification does NOT trigger a re-eval (unless the file also
  matches an entry in `[develop.watch] extra`)

### Requirement: External imported paths are out of scope

The watcher MUST NOT follow nix imports outside the flake root (absolute-path
imports, `builtins.readFile` on paths outside the flake, etc.). Users who need
those paths tracked MUST add them explicitly to `[develop.watch] extra`.

#### Scenario: Absolute import unwatched by default

- **WHEN** `flake.nix` contains `import /etc/nix/config.nix` and
  `/etc/nix/config.nix` is modified
- **THEN** the modification does NOT trigger a re-eval; the file is not part of
  the default watch set

#### Scenario: Explicit extra pattern picks up external path

- **WHEN** `[develop.watch] extra = ["/etc/nix/config.nix"]` is set and
  `/etc/nix/config.nix` is modified
- **THEN** the modification triggers a re-eval within the coalescing window

### Requirement: Startup summary observability

On daemon startup, the watcher MUST emit exactly one structured log line
summarising the effective watch configuration: total watched files, total
registered directories, coalescing window in milliseconds, and (on Linux) the
daemon's inotify usage as a fraction of `fs.inotify.max_user_watches`.

#### Scenario: Startup line present

- **WHEN** a daemon starts successfully
- **THEN** exactly one info-level structured log entry appears with the fields
  `watched_files`, `watched_dirs`, `coalesce_ms`, and (on Linux) `inotify_usage`
