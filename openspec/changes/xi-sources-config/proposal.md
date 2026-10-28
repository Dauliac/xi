# Change: xi-sources-config

## Why

[[xi-sources-model]] names three on-disk concepts (sources dir, state dir,
working dir) and a scope resolver, but defers the actual filesystem paths and
the full config schema to this change. That split is deliberate: the model is
OS-neutral, the paths are not.

Users additionally want a **working dir pointer** that lets them run
`xi {system,home,darwin,nixos} switch` from any CWD. Without a concrete config
surface, the only way to point xi at a working dir is a CLI flag every time -
the user wrote: "the idea is to be able to run xi <xx> switch or other nix
distributions properties from any path of your machine, because xi know the
local source".

## What Changes

- Introduce capability `xi-sources-config` that owns:
  - The precedence ladder (CLI flag > env var > `.xi.toml [sources]` >
    Nix-module default > built-in default), matching the house convention
    established by `xi fmt` and `xi develop`.
  - The concrete filesystem paths per scope:

    | Concept | NixOS system            | nix-darwin system       | home-manager / user                                         |
    | ------- | ----------------------- | ----------------------- | ----------------------------------------------------------- |
    | sources | `/var/lib/xi/sources`   | `/var/lib/xi/sources`   | `$XDG_DATA_HOME/xi/sources`                                 |
    | state   | `/var/lib/xi/state`     | `/var/lib/xi/state`     | `$XDG_STATE_HOME/xi`                                        |
    | working | not applicable (system) | not applicable (system) | user-chosen path pinned in `$XDG_STATE_HOME/xi/working-dir` |

  - The `.xi.toml` schema for the `[sources]` section (remote URL, branch,
    optional working-dir path, poll interval, trust requirements).
  - The three Nix-module option trees (`xi.sources.*` under
    `modules/{nixos,home-manager,flake-parts}/sources.nix`).
  - The env-var surface (`XI_SOURCES_REMOTE`, `XI_SOURCES_BRANCH`,
    `XI_SOURCES_WORKING_DIR`, `XI_SOURCES_POLL_INTERVAL`,
    `XI_SOURCES_STATE_DIR`) matching the house `XI_*` prefix.
  - A `xi sources init <remote-url> [--branch <name>] [--working-dir <path>]`
    bootstrap command that writes the local config and performs the first clone.

## Impact

- **Affected specs**: `xi-sources-config` (new capability).
- **Affected code (planned)**: `crates/xi-flake/src/project_config.rs` grows a
  `SourcesConfig` section; a new config loader in the planned
  `crates/xi-sources/src/config.rs`;
  `modules/{nixos,home-manager,flake-parts}/sources.nix` new files.
- **Coordination**: downstream [[xi-sources-gitops]], [[xi-sources-push]],
  [[xi-sources-bootstrap]] and [[xi-sources-cache-prestage]] all read config via
  this capability. Any new option added by a downstream change amends this spec.
- **Breaking?**: No. `.xi.toml` without a `[sources]` section continues to work;
  sources commands return `status: Missing` and name the config key that would
  configure them (per the model spec's SC-M-001).
- **Migration**: None. Opt-in via `xi sources init`.

## Success Criteria

- **SC-C-001**: Precedence ladder observable: setting the same key via all five
  surfaces (flag, env, `.xi.toml`, Nix module, default) with distinct values and
  reading via `xi sources status` returns the CLI flag's value.
- **SC-C-002**: `xi sources init https://git.example/dotfiles` writes a minimal
  `.xi.toml [sources]` block, clones the repo into the sources dir per scope,
  and emits a `Ready` status.
- **SC-C-003**: On a system where `XDG_DATA_HOME` is unset, user-scope sources
  dir falls back to `~/.local/share/xi/sources` (XDG spec default).
- **SC-C-004**: On a system where xi.sources.working-dir is unset and the state
  dir has no `working-dir` pointer, `xi home switch` run from a non-working-dir
  CWD reports a `Pending` status with a one-line remediation (point via flag,
  env, or state file).
- **SC-C-005**: Nix module `xi.sources.enable = true;` at `nixos` scope creates
  `/var/lib/xi/{sources,state}` with `0700` perms owned by root at activation
  time, before any `xi` command runs.
- **SC-C-006**: `.xi.toml [sources]` with an invalid remote URL fails at
  config-load time with a message naming the file+line (not at first fetch).
