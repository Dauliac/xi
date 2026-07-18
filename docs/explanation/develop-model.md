# The Develop Model

Xi's develop system replaces raw `nix develop` with a daemon-driven model that
provides live reload, trust management, nested flake support, and
shell-preserving subshells. This document explains why and how.

## The problem with nix develop

`nix develop` evaluates the flake, builds the devshell, and drops you into a
bash shell with the environment applied. Every time you want to pick up changes,
you exit and re-enter. Your shell configuration (aliases, prompt, history) is
replaced by a bare bash session.

This creates friction:

- Slow feedback loop: exit, re-enter, wait for evaluation
- Loss of shell personalisation
- No awareness of file changes
- No support for nested flakes in monorepos
- No trust model — any flake can run arbitrary shellHook code

## Xi's solution: daemon + subshell + prompt hook

### Background daemon

A per-flake daemon runs in the background. It:

- Evaluates the flake's devshell and writes environment files
- Watches for file changes (`flake.nix`, `flake.lock`, `*.nix`, and configurable
  extra patterns)
- Re-evaluates when changes are detected (rate-limited by `eval_interval`)
- Notifies all connected terminals of updates
- Handles async cache push operations
- Only updates your shell when the output actually changes (content hashing)

### Subshell model

Instead of replacing your shell, xi spawns a subshell that inherits your
configuration. Your prompt, aliases, functions, and history are preserved. The
devshell environment is layered on top.

All three shells (bash, zsh, fish) behave identically at the feature level.

### Prompt hook

A minimal prompt hook is installed in your shell. On every prompt display, it
checks in with the daemon. If the environment has changed, it sources the new
files. If a nested flake is detected, it spawns a child subshell.

This polling model is simple and robust: no background processes in your shell,
no signal handlers, no race conditions.

## Trust management

Xi never auto-activates a devshell without explicit trust:

- `xi develop trust` marks a flake for auto-activation
- `xi develop untrust` revokes it (exits active subshells immediately)
- Untrusted flakes show a warning: "run `xi develop trust`"
- No daemon is spawned for untrusted flakes

This prevents shellHook injection attacks from cloned repositories.

## Live reload

The daemon watches for file changes:

**Default watched patterns:**

- `flake.nix`, `flake.lock`, `.git/index`

**Configurable extras** (via `config.toml`):

```toml
[develop]
watch_extra = ["*.yaml", "version.txt", "Cargo.lock"]
```

When a change is detected, the daemon re-evaluates. If the result differs, your
shell picks up the new environment on the next prompt — no restart needed.

If evaluation fails:

- The last-good environment remains usable
- Retries with increasing delays (30s to 5m)
- File changes reset the delay immediately
- A notification is shown: "evaluation failed: \<error\>"

## Nested flakes

Xi supports monorepo layouts:

```
monorepo/
├── flake.nix          # outer
└── services/
    └── api/
        └── flake.nix  # inner
```

When you `cd services/api/`, xi spawns a nested subshell with the inner
devshell. PATH is composed: inner paths prepended to outer paths. When you leave
the inner directory, you return to the outer devshell.

Each flake has its own daemon instance.

## Multi-terminal support

Multiple terminals can use the same devshell simultaneously. All connect to the
same daemon and receive notifications (e.g. "packages updated"). Each terminal
sees each notification exactly once.

## GC root protection

The daemon creates GC roots using `nix --profile`, the same approach as
nix-direnv. This protects the entire devshell closure from `nix-collect-garbage`
automatically.

## Version upgrades

When xi is upgraded, the daemon detects the version mismatch and restarts
automatically. No manual intervention needed.

## State locations

| Path                                       | Content             |
| ------------------------------------------ | ------------------- |
| `$XDG_RUNTIME_DIR/xi-develop/daemon.sock`  | Daemon socket       |
| `$XDG_STATE_HOME/xi/develop/`              | Cached environments |
| `$XDG_CONFIG_HOME/xi/develop/trusted/<id>` | Trust markers       |

## Design rationale

**Why a daemon?** Without a daemon, every prompt would re-evaluate the flake. A
daemon amortises evaluation across all terminals and provides genuine live
reload.

**Why subshells?** Subshells preserve your shell configuration. In-place
environment modification is fragile and hard to undo. Subshells provide a clean
entry/exit boundary.

**Why content hashing?** Without content hashing, editing a comment in
`flake.nix` would trigger a re-source. Only meaningful changes update your
environment.
