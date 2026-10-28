# Capability: xi-sources-model

This capability owns the data model that every other `xi-sources-*` capability
reads from. It defines the three on-disk concepts (sources dir, state dir,
working dir), the ref lineage, the scope resolver, and the public
`xi <scope> sources status` surface.

## ADDED Requirements

### Requirement: Three named on-disk concepts

The capability MUST define exactly three named concepts and no more:

- **sources dir** - the managed clone of the user's config repo. Owned by xi (or
  by the gitops daemon when enabled). Users SHOULD NOT edit this clone directly;
  it is the "what is currently deployed" view.
- **state dir** - a flat directory of small metadata files. Never contains
  source code. Owned by xi exclusively.
- **working dir** - an _optional_ user-chosen directory containing an editable
  clone of the same repo. Its path is recorded in the state dir so
  `xi {nixos,home,darwin,system} switch` executed from any CWD can find it.

The concrete filesystem paths for sources dir and state dir per scope are owned
by [[xi-sources-config]]. This capability only names the concepts.

#### Scenario: Three concepts, no fourth

- **WHEN** any xi code refers to the user's config repo on disk
- **THEN** it MUST refer to one of {sources dir, state dir, working dir}
- **AND** no new fourth concept is introduced without amending this spec

#### Scenario: No source code in state dir

- **WHEN** `xi <scope> sources status` or any other xi code writes to the state
  dir
- **THEN** every file written is <= 64 KiB and matches a known metadata filename
  (`remote-head`, `working-head`, `sources-head`, `working-dir`,
  `active-generation`, `lock`)

### Requirement: Sanctioned write-flow set

The capability MUST enumerate every direction in which xi code may modify a
user-visible tree. The allowed set is exactly:

1. remote `->` sources dir, via the gitops daemon's fetch
2. sources dir `->` working dir, via the hermetic sync verb
   ([[xi-sources-conflict]], `xi <scope> sources sync`)
3. state dir metadata writes (owned by this capability)

Writes NOT in this set are forbidden. In particular:

- xi code MUST NOT write from the working dir into the sources dir (reverse of
  flow 2). The reverse path is the user's `git push` to the remote followed by
  flow 1 on the next tick.
- xi code MUST NOT push to the remote on behalf of the user. The user owns
  `git push` from the working dir.
- The gitops daemon MUST NOT write to the working dir. It writes only to the
  sources dir and to state dir metadata.

#### Scenario: Daemon fetches, working dir untouched

- **GIVEN** the gitops daemon advances the sources dir past the working dir
- **WHEN** the tick completes
- **THEN** no file in the working dir has been modified
- **AND** the state dir records `sources_head` as advanced

#### Scenario: Sync writes only the working dir

- **GIVEN** `xi <scope> sources sync` runs with a clean working dir
- **WHEN** the sync completes
- **THEN** the working dir reflects the sources dir tree at `sources_head`
- **AND** the sources dir is byte-identical to its pre-sync state
- **AND** no file in `$HOME` outside the working dir has been modified

### Requirement: Scope resolver

The capability MUST provide a `Scope` enum with exactly two variants - `System`
and `User` - and a scope resolver that each top-level switch-style subcommand
consults at parse time.

The mapping is fixed:

| Command            | Scope    |
| ------------------ | -------- |
| `xi nixos switch`  | `System` |
| `xi darwin switch` | `System` |
| `xi system switch` | `System` |
| `xi home switch`   | `User`   |

`xi switch` with no verb MUST be a parse error that lists the four concrete
verbs.

#### Scenario: Each switch verb maps to exactly one scope

- **WHEN** a user runs any of `xi {nixos,darwin,system} switch`
- **THEN** the resolver returns `Scope::System`
- **AND** when the user runs `xi home switch`, the resolver returns
  `Scope::User`

#### Scenario: Verb-less switch is a parse error

- **WHEN** a user runs `xi switch` with no further verb
- **THEN** xi exits non-zero with a message naming the four concrete verbs
- **AND** no sources state is touched

### Requirement: SourcesId identity

The capability MUST define `SourcesId { scope: Scope, canonical_url: String
}`
where `canonical_url` is the user's remote URL after a documented
canonicalisation (lowercase scheme + host, trailing `.git` stripped, userinfo
discarded). Two commands referring to the same `(scope,
canonical_url)` pair
MUST resolve to the same sources dir and state dir on disk.

#### Scenario: Trailing .git is irrelevant

- **GIVEN** a user configures sources with `https://git.example/repo.git`
- **WHEN** the same user configures sources with `https://git.example/repo`
- **THEN** both configurations resolve to the same `SourcesId`

#### Scenario: Scope separates identical URLs

- **GIVEN** a user configures `https://git.example/dotfiles` at user scope
- **WHEN** the same URL is also configured at system scope on the same host
- **THEN** two distinct `SourcesId` values exist and two independent sources
  dirs and state dirs are managed

### Requirement: RefLineage

The capability MUST expose a `RefLineage` carrying four optional git object ids:

- `remote_head` - the sha the gitops daemon last successfully fetched from the
  remote. `None` on a machine with no daemon or before the first fetch.
- `working_head` - the sha of the working dir's HEAD, if a working dir is
  configured. `None` otherwise.
- `sources_head` - the sha of the sources dir's HEAD. `None` only on a
  freshly-initialised sources dir before any clone.
- `active_generation` - the generation id of the most recent successful
  `nixos-rebuild` / `darwin-rebuild` / `home-manager switch`. `None` before the
  first successful switch.

Conflict rules between these refs are owned by [[xi-sources-conflict]]. This
spec only mandates that all four are OBSERVABLE via the model.

#### Scenario: All four refs observable

- **GIVEN** a configured sources repo with gitops disabled and no working dir
- **WHEN** `xi home sources status` runs
- **THEN** the response carries
  `remote_head: None, working_head: None,
  sources_head: Some(..), active_generation: Some(..)`

### Requirement: Public status surface

The capability MUST add `xi <scope> sources status` as a JSON command. The
response MUST match the house envelope (schema version + status
`Ready | Pending | Missing | Degraded`), with status computed as:

- `Missing` - no sources dir on disk for this scope
- `Pending` - sources dir exists but `active_generation` is unknown (never
  switched)
- `Ready` - sources dir exists AND `active_generation` is known AND (if gitops
  is enabled) the last fetch succeeded
- `Degraded` - sources dir exists AND the last gitops tick failed OR the working
  dir path recorded in state no longer exists on disk

#### Scenario: Fresh machine returns Missing

- **GIVEN** no sources dir exists for the resolved scope
- **WHEN** `xi home sources status` runs
- **THEN** the status is `Missing`
- **AND** the body names which env var / config key / CLI flag could configure
  sources

#### Scenario: Switched-at-least-once machine returns Ready

- **GIVEN** sources dir exists, `active_generation` is known, and gitops is
  disabled OR the last fetch succeeded
- **WHEN** `xi home sources status` runs
- **THEN** the status is `Ready`

#### Scenario: Stale working-dir pointer surfaces as Degraded

- **GIVEN** sources dir exists AND `state_dir/working-dir` points at a path that
  no longer exists on disk
- **WHEN** `xi home sources status` runs
- **THEN** the status is `Degraded` with a reason naming the stale working dir
  path

### Requirement: State dir file set is fixed

The state dir MUST contain at most the following files, each individually
optional, each readable only by the owning UNIX user (mode `0600` on files,
`0700` on the directory):

- `remote-url` - the remote this scope is adopted to, written by
  `xi <scope> switch <remote-url>`. This is what lets a bare `xi <scope> switch`
  find its source with no arguments.
- `remote-head` - hex sha, one line
- `working-head` - hex sha, one line
- `sources-head` - hex sha, one line
- `working-dir` - a canonical path, one line
- `active-generation` - generation id, one line
- `lock` - advisory lockfile for mutating operations

Any file outside this set found in the state dir MUST be reported in
`xi <scope> sources status` as a `Degraded` reason, not silently ignored.

#### Scenario: Unknown file in state dir is reported

- **GIVEN** the state dir contains `random-file.txt`
- **WHEN** `xi home sources status` runs
- **THEN** the status is `Degraded` with a reason naming the unknown file
