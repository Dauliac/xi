# Change: xi-sources-nix-modules

## Why

[[xi-sources-config]] names three Nix modules (`modules/nixos/sources.nix`,
`modules/home-manager/sources.nix`, `modules/flake-parts/sources.nix`) and
describes their job (write the sources config at activation and own the
`xi.sources.*` option tree), but it does not enumerate the options at the
attribute level. Downstream specs ([[xi-sources-gitops]],
[[xi-sources-bootstrap]], [[xi-sources-push]], [[xi-sources-conflict]],
[[xi-sources-cache-prestage]]) all reference specific module keys
(`xi.sources.gitops.enable`, `xi.sources.bootstrap.installer`,
`xi.sources.secrets.<name>`, `xi.sources.sync-mode`) that have never been
declared in one place.

Without a complete option tree, each spec reader has to re-derive what the
module actually exposes, and the three parallel modules (per the house
convention established by `fmt.nix` and the devshell modules) will drift against
each other. This change collapses the tree into a single authoritative spec and
defines the parity rules across the three heads.

## What Changes

- Introduce capability `xi-sources-nix-modules` that owns:
  - The **full `xi.sources.*` option tree**, enumerated once, with types,
    defaults, and descriptions. Every downstream spec that mentions a module
    option amends this tree.
  - The **parity rule** across the three heads: the NixOS, home-manager, and
    flake-parts modules expose the SAME option tree. Any option that only makes
    sense in one scope is namespaced (`xi.sources.system.*` vs
    `xi.sources.user.*`) rather than duplicated.
  - A **shared library** under `modules/shared/lib/sources.nix` that carries the
    option definitions. Each head imports it. This mirrors
    `modules/shared/lib/wrapper.nix` already used by `fmt.nix`.
  - The **activation script** that writes `/etc/xi/sources.toml` (system) or
    `${XDG_CONFIG_HOME}/xi/sources.toml` (user), from the evaluated module
    options.
  - The **state-directory creation** assertion: when `xi.sources.enable = true`,
    the module's activation script creates the sources dir and state dir per the
    [[xi-sources-config]] path table with the correct permissions, before any xi
    binary runs.
  - **Mutual-exclusion assertions**: `xi.sources.gitops.enable = true` at user
    scope AND `xi.sources.gitops.enable = true` at system scope for the same
    remote URL raises an eval error; the two would race.

## Impact

- **Affected specs**: `xi-sources-nix-modules` (new capability).
- **Affected code (planned)**: new files
  `modules/{nixos,home-manager,flake-parts}/sources.nix`;
  `modules/shared/lib/sources.nix` carrying the shared option tree;
  activation-script helpers in each head.
- **Coordination**: every other `xi-sources-*` spec consumes this tree via its
  option names. This change is the authoritative registry.
- **Breaking?**: No. The modules are new; existing xi users without
  `xi.sources.enable = true` observe no change.
- **Migration**: None.

## Success Criteria

- **SC-N-001**: The three heads (`nixos`, `home-manager`, `flake-parts`) expose
  byte-identical option trees under `xi.sources.*`, confirmed by a test that
  imports each head and diffs the resulting option set.
- **SC-N-002**: `xi.sources.enable = true` at NixOS scope creates
  `/var/lib/xi/sources` and `/var/lib/xi/state` with mode `0700`, owned by
  `root:root`, during activation and before any xi binary runs.
- **SC-N-003**: `xi.sources.enable = true` at home-manager scope creates
  `$XDG_DATA_HOME/xi/sources` and `$XDG_STATE_HOME/xi` with mode `0700`, owned
  by the user, during activation.
- **SC-N-004**: `/etc/xi/sources.toml` (or XDG path) is a materialised snapshot
  of the evaluated options at activation time; running `xi sources status` after
  activation shows the module-provided values with provenance `nix-module`.
- **SC-N-005**: Setting the same remote at BOTH system and user scope with
  `gitops.enable = true` triggers a build-time assertion naming the conflict.
- **SC-N-006**: Every option referenced by any other `xi-sources-*` spec
  resolves against this tree; a CI check (`openspec validate`) cross-references
  them.
