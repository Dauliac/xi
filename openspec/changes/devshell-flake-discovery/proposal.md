# Change: devshell-flake-discovery

## Why

`xi develop` operates on "the flake that owns this directory". Determining which
flake that is — from a user's CWD — is the first thing the prompt hook,
`xi develop enter`, and every daemon-consulting subcommand does. Today the
implementation is a 27-line function (`flake_detect.rs:
find_flake_stack`) that
walks up the filesystem collecting every `flake.nix`, with the nearest
(innermost) flake taking precedence via `find_flake_root`.

The rules are intuitive but unspecified:

- Which flake wins when directories are nested?
- What happens inside a git worktree whose parent has a `flake.nix`?
- Does a `flake.nix` inside `.git/` or `node_modules/` count?
- What if the CWD is a symlink to somewhere with a different flake?
- What's the boundary condition — do we walk past `/`, past `$HOME`, past a
  mount boundary?

Without a spec, each caller re-invents the rules and the prompt-hook budget
becomes unpredictable when the walk crosses network mounts or symlinked home
dirs.

## What Changes

- Introduce capability `xi-devshell-flake-discovery` that owns:

  - The walk-up algorithm: start from CWD (canonicalised), collect every
    ancestor directory containing a `flake.nix`, order innermost-last. The
    **innermost** flake wins for `find_flake_root`; the full stack is available
    via `find_flake_stack` for callers that need nested orchestration.

  - **Canonicalisation is mandatory**: the walk starts from the canonical
    (symlink-resolved) path of the CWD so that `~/repo` symlinked to
    `/mnt/nvme/repo` produces the same flake id and identical trust / activation
    behaviour regardless of which path the user typed.

  - **Boundary conditions**: the walk stops at the filesystem root (`/`). It
    MUST NOT stop at `$HOME`, at mount points, or at git worktree boundaries —
    those are not authoritative for flake ownership.

  - **Excluded directories**: `flake.nix` files inside `.git/`, `node_modules/`,
    or `target/` MUST NOT be considered flake roots. Ancestor walks MUST NOT
    descend into those either — they are only walked _through_ (via `parent()`),
    never scanned.

  - **Bounded walk cost**: the walk is O(depth × 1 stat) and MUST NOT exceed the
    prompt-hook budget. On network-backed filesystems where a single `stat` can
    block indefinitely, the discovery caller MUST provide its own timeout; the
    walk itself does not add one.

  - **`.envrc` awareness (informational, not authoritative)**: presence of a
    `.envrc` MUST NOT change flake discovery. Direnv integration lives in a
    separate proposal; this spec establishes that flake discovery is the ground
    truth and `.envrc` cannot override it.

  - **Non-flake fallback**: if no ancestor contains a `flake.nix`, the discovery
    result is empty (`None` / empty stack). Callers MUST handle the empty case
    explicitly — treating "no flake" as "the root filesystem's flake" is a bug.

## Impact

- **Affected specs**: `xi-devshell-flake-discovery` (new capability — first
  delta introduces its requirements).
- **Affected code**: `crates/xi-develop/src/flake_detect.rs` (canonicalise the
  start path, add excluded-directory filter, add tests) — the behavioural change
  is small; most of this delta specifies existing intent.
- **Coordination with `devshell-trust-model`**: the trust marker key is derived
  from the canonical flake root. This spec ensures that the discovery layer
  produces canonical paths so `dirs::flake_id` receives a stable input.
- **Coordination with `async-daemon-cqrs`**: one daemon per flake id; discovery
  determines which daemon a given CWD binds to.
- **Breaking?**: No. Canonicalisation, the excluded-directory list, and the
  boundary rules match the current implementation's _intent_; edge cases that
  today produce inconsistent behaviour (symlinked CWD, stray `flake.nix` in
  `node_modules`) will be corrected — a very narrow behaviour change.
- **Migration**: None required for the common case. Users who currently rely on
  a `node_modules/some-pkg/flake.nix` being detected as their project's flake
  root will need to move it — an unlikely case.

## Success Criteria

- **SC-D-001**: `find_flake_root` returns `Some(<canonical>)` for a CWD inside a
  flake; the returned path is symlink-resolved and identical regardless of which
  alias the user's shell CWD reflects.
- **SC-D-002**: For nested flakes `/a/flake.nix` and `/a/b/flake.nix`,
  `find_flake_root` from `/a/b/c` returns `/a/b` and `find_flake_stack` returns
  `[/a, /a/b]` in that order.
- **SC-D-003**: A `flake.nix` inside `.git/`, `node_modules/`, or `target/` is
  ignored; discovery returns as if it did not exist.
- **SC-D-004**: A CWD with no `flake.nix` in any ancestor produces `None` / an
  empty stack; no directory (including `/`) is fabricated as a fallback flake
  root.
- **SC-D-005**: The walk-up cost is one `stat` per ancestor directory. On a CWD
  10 levels deep, discovery performs 10 stats and no directory scans.
- **SC-D-006**: Symlinked home directories (`~/repo` → `/mnt/nvme/repo`) produce
  the same `flake_id` from both aliases; trust granted via one alias applies to
  the other.

## Assumptions

- The filesystem root (`/`) is a valid termination point on all supported
  platforms (Linux, macOS).
- Users do not intentionally place a `flake.nix` inside `.git/`,
  `node_modules/`, or `target/`. The exclusion list is a fixed set; it is not
  user-configurable in v1.
- Canonicalisation via `fs::canonicalize` is acceptable overhead (typically < 1
  ms even on network filesystems for cached paths).
- Multi-flake orchestration (a single command touching multiple flakes in the
  stack) uses `find_flake_stack`, not `find_flake_root` — those callers
  explicitly opt in.
- Callers that operate on network-backed filesystems are responsible for their
  own I/O timeouts. Discovery does not add per-stat timeouts to keep the
  algorithm portable.
