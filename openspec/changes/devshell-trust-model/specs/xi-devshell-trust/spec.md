# xi-devshell-trust (delta)

## ADDED Requirements

### Requirement: Non-devshell flakes are silent

Before any trust decision is made, the prompt hook MUST check whether the
discovered flake's `flake.nix` plausibly declares a devshell (contains
`devShells`, `devShell`, or `mkShell`). If none of those are present, the hook
MUST return without checking trust, without printing any prompt, and without
touching the daemon. This gate applies regardless of whether the flake is
trusted or untrusted.

#### Scenario: Flake with no devshell keywords, untrusted

- **WHEN** the prompt fires inside a flake whose `flake.nix` contains none of
  `devShells`, `devShell`, or `mkShell`, and no trust marker exists for it
- **THEN** the hook returns silently: no stderr prompt, no daemon connection, no
  trust check performed

#### Scenario: Flake with no devshell keywords, trusted

- **WHEN** the prompt fires inside a flake whose `flake.nix` contains none of
  the devshell keywords, and a trust marker exists for it
- **THEN** the hook still returns silently; the devshell-likelihood gate is
  checked before trust, not after

### Requirement: Refuse-by-default at the prompt hook

The prompt hook MUST NOT spawn the daemon, connect to an existing daemon, or
invoke `nix eval` on behalf of a flake that has no trust marker. Untrusted
flakes MUST be handled by a fast local heuristic that returns from the tick
without doing evaluation work.

#### Scenario: Prompt fires in an untrusted flake with hook installed

- **WHEN** the user's shell prompt fires inside a directory containing a
  `flake.nix` for which no trust marker exists
- **THEN** the hook returns without connecting to any daemon and without
  invoking `nix eval`

#### Scenario: Untrusted flake never triggers daemon spawn

- **WHEN** the prompt fires in an untrusted flake and no daemon is currently
  running for it
- **THEN** the hook does not start a daemon process

### Requirement: Single throttled trust prompt per shell per flake

When the hook encounters an untrusted flake, it MUST emit exactly one
interactive-stderr prompt telling the user how to run `xi develop trust <path>`.
The prompt MUST be throttled per shell process per flake id so that repeated
prompt ticks (or repeated `cd` in and out) do not re-emit it.

#### Scenario: First untrusted tick in a shell

- **WHEN** the shell's first prompt tick after entering an untrusted flake fires
- **THEN** exactly one interactive-stderr line is written explaining the
  `xi develop trust` command

#### Scenario: Subsequent ticks in the same shell

- **WHEN** the same shell process runs multiple prompt ticks inside the same
  untrusted flake
- **THEN** no additional interactive-stderr writes occur for that flake id

#### Scenario: Known gap, current behaviour

- **GIVEN** `prompt.rs::handle_parent` as of 2026-11-07 has no throttle key
- **WHEN** the same shell runs N prompt ticks inside the same untrusted flake
- **THEN** today's build writes N stderr lines, one per tick, not one; this
  scenario documents the gap tracked by the throttling task in `tasks.md` and
  must flip to the "Subsequent ticks" scenario above once that task lands

### Requirement: Explicit trust grant is the only path to trusted state

A flake MUST become trusted only via an explicit user action
(`xi develop
trust`). No other command, hook, or daemon-side event MAY create a
trust marker.

#### Scenario: Automatic activation never grants trust

- **WHEN** the daemon activates a devshell as part of any command flow
- **THEN** it does not create, modify, or extend any trust marker

#### Scenario: Trust grant writes a single marker

- **WHEN** the user runs `xi develop trust <path>`
- **THEN** exactly one marker file is written under `dirs::trust_dir()` keyed by
  the canonical flake id, with no other filesystem side-effects

### Requirement: Trust markers are per-user and non-shareable

The trust directory MUST be created with mode `0700` and each marker file MUST
be created with mode `0600`. Trust granted by one UNIX user MUST NOT be readable
or usable by another UNIX user on the same host.

#### Scenario: Directory and marker permissions after grant

- **WHEN** `xi develop trust <path>` completes successfully
- **THEN** the trust directory has mode `0700` and the newly written marker file
  has mode `0600`

#### Scenario: Another user cannot read the trust store

- **WHEN** a second UNIX user attempts to read the first user's trust directory
- **THEN** the operation fails with a permission error and no marker content
  leaks

#### Scenario: Known gap, current behaviour

- **GIVEN** `trust_flake`/`untrust_flake` as of 2026-11-07 call plain
  `fs::write`/`fs::create_dir_all` with no `set_permissions` call
- **WHEN** `xi develop trust <path>` runs under a permissive umask (e.g. `0022`)
- **THEN** today's build leaves the directory and marker at whatever mode the
  umask produces (typically `0755`/`0644`), not the forced `0700`/`0600`; this
  scenario documents the gap tracked in `tasks.md` and must flip to the
  "Directory and marker permissions after grant" scenario above once that task
  lands

### Requirement: Trust survives flake source edits

Editing `flake.nix`, `flake.lock`, or any imported file inside a trusted flake
MUST NOT invalidate its trust marker. Trust is scoped to the flake identity (its
canonical directory), not to the content of the nix code.

#### Scenario: Edit and re-prompt

- **WHEN** the user modifies `flake.nix` in a trusted flake and the next prompt
  tick fires
- **THEN** the hook proceeds with normal activation without any re-prompt

### Requirement: Trust is bound to canonical flake identity

The trust marker key MUST be derived from the canonical path of the flake root
(symlinks resolved). Moving or renaming the flake directory MUST result in a new
flake id and the moved directory MUST be treated as untrusted until re-granted.

#### Scenario: Move a trusted flake

- **WHEN** a trusted flake at `/a/proj` is moved to `/b/proj` and the shell
  prompt fires in the new location
- **THEN** the hook treats `/b/proj` as untrusted and emits the standard
  untrusted-flake behaviour

### Requirement: Trust revocation is immediate

`xi develop untrust <path>` MUST remove the corresponding marker within the same
command invocation. Subsequent prompt ticks in that flake MUST observe the
untrusted behaviour without process restart.

#### Scenario: Revoke and re-prompt in the same shell

- **WHEN** `xi develop untrust <path>` completes and the shell's next prompt
  tick fires inside that flake
- **THEN** the tick behaves as untrusted (no daemon connect, one stderr prompt)

#### Scenario: Revoke a flake that was not trusted

- **WHEN** `xi develop untrust <path>` runs against a flake with no marker
- **THEN** the command exits successfully with a warning that the flake was
  already untrusted, and no file is created

### Requirement: Trust list surface

`xi develop trust --list` MUST return one line per known marker with the flake
id and its best-effort canonical path. Markers whose original path can no longer
be resolved MUST be reported explicitly rather than silently omitted.

#### Scenario: List after two grants

- **WHEN** the user grants trust to two distinct flakes and runs
  `xi develop trust --list`
- **THEN** the command prints two lines, sorted by flake id, each with the flake
  id and the canonical path

#### Scenario: Missing path reporting

- **WHEN** a trusted flake's directory has been deleted and
  `xi develop
  trust --list` runs
- **THEN** its entry is printed with a `(path missing)` marker in the path
  column

### Requirement: Nested child-flake trust is independent

When a shell's CWD moves from a trusted parent flake's active subshell into a
directory owned by a different, nested `flake.nix`, the hook MUST trust-check
the child independently of the parent's trust state. The hook MUST NOT inherit
the parent's trust for the child, and MUST NOT escalate with an additional
prompt when the child is untrusted.

#### Scenario: Nested trusted child spawns its own subshell

- **WHEN** a shell inside a trusted parent flake's subshell `cd`s into a nested
  directory owned by a different `flake.nix` that is also trusted
- **THEN** the hook spawns a nested subshell scoped to the child flake,
  incrementing `__XI_DEPTH`

#### Scenario: Nested untrusted child stays silent in the parent

- **WHEN** a shell inside a trusted parent flake's subshell `cd`s into a nested
  directory owned by a different, untrusted `flake.nix`
- **THEN** the hook does not spawn a nested subshell, does not print a trust
  prompt for the child, and the shell remains in the parent's subshell

#### Scenario: Suppress clears when returning to the parent

- **WHEN** a nested untrusted child's suppress state is set and the user `cd`s
  back out of the child directory into the parent flake root
- **THEN** the suppress state for that child is cleared on the next prompt tick
