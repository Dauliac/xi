# xi-devshell-flake-discovery (delta)

## ADDED Requirements

### Requirement: Ancestor walk-up from canonical CWD

Flake discovery MUST start from the canonical (symlink-resolved) path of the
caller's CWD and MUST walk each parent directory up to the filesystem root,
collecting every ancestor that contains a `flake.nix`.

#### Scenario: Discovery from a canonical CWD

- **WHEN** discovery runs from a CWD whose canonical path is
  `/mnt/nvme/repo/pkg/src`
- **THEN** the walk visits `/mnt/nvme/repo/pkg/src`, `/mnt/nvme/repo/pkg`,
  `/mnt/nvme/repo`, `/mnt/nvme`, `/mnt`, `/` in order, checking each for
  `flake.nix`

#### Scenario: Discovery from a symlinked CWD alias

- **WHEN** the user's shell CWD is `~/repo/pkg/src` where `~/repo` is a symlink
  to `/mnt/nvme/repo`
- **THEN** discovery starts from `/mnt/nvme/repo/pkg/src` and returns paths
  rooted at `/mnt/nvme/repo`, not at `~/repo`

### Requirement: Innermost flake wins for single-root callers

`find_flake_root` MUST return the innermost (deepest) discovered flake
directory. If nested flakes exist along the ancestor path, the deepest one takes
precedence.

#### Scenario: Nested flakes

- **WHEN** both `/a/flake.nix` and `/a/b/flake.nix` exist and discovery runs
  from `/a/b/c/src`
- **THEN** `find_flake_root` returns `/a/b`

#### Scenario: Single flake

- **WHEN** exactly one `flake.nix` exists at `/a` and discovery runs from
  `/a/b/c`
- **THEN** `find_flake_root` returns `/a`

### Requirement: Full stack exposed for nested orchestration

`find_flake_stack` MUST return the full list of discovered flake roots ordered
outermost-first (root-most flake at index 0, innermost at the last index).
Callers doing nested orchestration MUST use the stack; `find_flake_root` is a
convenience wrapper over its last element.

#### Scenario: Stack ordering

- **WHEN** `/a/flake.nix` and `/a/b/flake.nix` exist and discovery runs from
  `/a/b/c`
- **THEN** `find_flake_stack` returns `[/a, /a/b]`

### Requirement: Excluded ancestor directories

Discovery MUST ignore `flake.nix` files whose parent directory is `.git`,
`node_modules`, or `target`. These directories MAY still lie along the walk path
(the walk goes _through_ them via `parent()`), but they MUST NOT be recorded as
flake roots.

#### Scenario: Flake inside node_modules

- **WHEN** `/proj/node_modules/some-pkg/flake.nix` exists and discovery runs
  from `/proj/node_modules/some-pkg/src`
- **THEN** it is not recorded; if `/proj/flake.nix` exists, discovery returns
  `/proj`; if not, discovery returns `None`

#### Scenario: Flake inside .git

- **WHEN** `/proj/.git/hooks/flake.nix` exists (accidentally) and discovery runs
  from `/proj/src`
- **THEN** it is not recorded

### Requirement: Filesystem-root termination without fallback

Discovery MUST terminate at the filesystem root (`/`). It MUST NOT fabricate a
synthetic flake root when no ancestor contains a `flake.nix`. The empty result
MUST be explicit (`None` / empty stack) so callers handle it deliberately.

#### Scenario: No flake anywhere

- **WHEN** no `flake.nix` exists in any ancestor of the CWD
- **THEN** `find_flake_root` returns `None` and `find_flake_stack` returns an
  empty vector

#### Scenario: Walk does not stop at mount or home

- **WHEN** the CWD is `/mnt/nfs/proj/src`, `$HOME` is `/mnt/nfs/user`, and
  `/proj/flake.nix` does not exist but `/mnt/nfs/proj/flake.nix` does
- **THEN** discovery returns `/mnt/nfs/proj` without pausing at the mount
  boundary or at `$HOME`

### Requirement: Bounded per-ancestor cost

The walk MUST perform at most one `stat` per ancestor directory (checking for
`flake.nix` existence). It MUST NOT scan directory contents, MUST NOT recurse,
and MUST NOT probe additional files.

#### Scenario: Deep CWD stat count

- **WHEN** discovery runs from a CWD ten directories below a flake root
- **THEN** exactly ten `stat`-equivalent existence checks are performed (one per
  ancestor including the CWD itself), and no directory-content reads occur

### Requirement: Canonicalisation failure yields empty result

If the CWD cannot be canonicalised (broken symlink, permission error), discovery
MUST return an empty result rather than silently falling back to the
un-canonicalised path.

#### Scenario: Broken symlink in CWD chain

- **WHEN** the CWD lies under a broken symlink and `fs::canonicalize` fails
- **THEN** `find_flake_root` returns `None` and the caller receives no flake
  root

### Requirement: Flake ownership is not overridable by .envrc

Presence of a `.envrc` file MUST NOT change the outcome of flake discovery.
Flake ownership is determined solely by the walk-up algorithm defined above.

#### Scenario: .envrc present alongside flake.nix

- **WHEN** `/proj/.envrc` and `/proj/flake.nix` both exist
- **THEN** discovery from `/proj/src` returns `/proj` (unchanged from the case
  without `.envrc`)

#### Scenario: .envrc present without a flake

- **WHEN** `/proj/.envrc` exists but no `flake.nix` exists in any ancestor
- **THEN** discovery returns `None`; the `.envrc` does not create a synthetic
  flake root
