# The Develop Model

Xi's develop system replaces raw `nix develop` with a daemon-driven model
that provides live reload, trust management, nested flake support, and
shell-preserving subshells. This document explains why and how.

## The problem with nix develop

`nix develop` evaluates the flake, builds the devshell, and drops you into a
bash shell with the environment applied. Every time you want to pick up
changes, you exit and re-enter. Your shell configuration (aliases, prompt,
history) is replaced by a bare bash session.

This creates friction:

- Slow feedback loop: exit, re-enter, wait for evaluation
- Loss of shell personalisation
- No awareness of file changes
- No support for nested flakes in monorepos
- No trust model — any flake can run arbitrary shellHook code

## Xi's solution: daemon + subshell + prompt hook

Xi splits the problem into three components:

### 1. Background daemon

A per-flake daemon runs as a background process, communicating over a Unix
socket at `$XDG_RUNTIME_DIR/xi-develop/daemon.sock`.

The daemon:

- Evaluates the flake's devshell
- Writes environment and hook files to disk
- Watches for file changes (`flake.nix`, `flake.lock`, `*.nix`, configured
  extra patterns)
- Re-evaluates when changes are detected (rate-limited by `eval_interval`)
- Handles async cache push requests (spawning background push threads)
- Periodically drains the persistent cache push queue (retrying failed pushes)
- Manages a notification bus for all connected terminals
- Tracks consumer shells via a registry
- Uses generation counters and content hashes to avoid unnecessary updates

### 2. Subshell model

Instead of replacing your shell, xi spawns a subshell that inherits your
configuration:

| Shell | Method |
|-------|--------|
| bash | `--rcfile` with injected env sourcing |
| zsh | Custom `ZDOTDIR` that sources `.zshrc` then env |
| fish | `--init-command` that sources env then config |

Your prompt, aliases, functions, and history are preserved. The devshell
environment is layered on top.

### 3. Prompt hook

A minimal prompt hook (under 10 lines) is installed in your shell. On every
prompt display, it calls `xi develop prompt`, which:

1. Detects if the current directory is inside a flake
2. Checks the trust database
3. Connects to the daemon (starting it if needed)
4. Receives a `PromptResponse` with instructions:
   - `should_source_env`: source the updated environment file
   - `should_source_hook`: source the updated hook file
   - `should_exit`: leave the subshell (e.g. untrusted)
   - `should_spawn`: enter a nested subshell
   - `notifications`: messages to display

This polling model is simple and robust: no background processes in the
shell, no signal handlers, no race conditions.

## Trust management

Xi never auto-activates a devshell without explicit trust. The trust model:

- `xi develop trust` creates a marker file at
  `$XDG_CONFIG_HOME/xi/develop/trusted/<flake_id>`
- The flake ID is deterministic, derived from the absolute path
- Trust is checked on every prompt (fast file existence check)
- Untrusted flakes show a per-terminal warning: "run `xi develop trust`"
- No daemon is spawned for untrusted flakes
- `xi develop untrust` revokes trust and causes immediate subshell exit

This prevents shellHook injection attacks from cloned repositories.

## Live reload

The daemon watches for file changes using a polling-based watcher:

**Default watched patterns:**
- `flake.nix`
- `flake.lock`
- `.git/index`

**Configurable extras** (via `config.toml`):
```toml
[develop]
watch_extra = ["*.yaml", "version.txt", "Cargo.lock"]
```

When a change is detected:

1. The daemon starts re-evaluation (rate-limited by `eval_interval`)
2. The new output is content-hashed against the previous output
3. If the hash differs, the generation counter is bumped
4. On the next prompt, consumers see `should_source_env: true`
5. The shell sources the new environment file

If evaluation fails:
- The daemon enters `BuildFailed` state
- The last-good environment remains usable
- Exponential backoff: 30s, 60s, 120s, 240s, 300s cap
- File changes reset the backoff immediately
- A notification is shown: "evaluation failed: <error>"

## Nested flakes

Xi supports monorepo layouts with nested flakes:

```
monorepo/
├── flake.nix          # outer
└── services/
    └── api/
        └── flake.nix  # inner
```

When you `cd services/api/`:

1. The prompt hook detects a nested flake
2. A new subshell is spawned with the inner devshell
3. The outer shell is blocked (waiting for the inner subshell)
4. PATH is composed: inner paths prepended to outer paths
5. When you leave the inner directory, the inner subshell exits
6. You return to the outer devshell

Each flake has its own daemon instance. The flake stack is detected as
`[outermost, ..., innermost]`, with `find_flake_root()` returning the
nearest (innermost) flake.

## Multi-terminal support

Multiple terminals can use the same devshell simultaneously:

- All terminals connect to the same daemon
- Global notifications (e.g. "packages updated") are broadcast to all
- Each terminal has a per-consumer cursor in the notification bus,
  preventing duplicates
- New terminals skip old notifications
- Dead consumers are reaped via `/proc/{pid}` checks

## GC root protection

The daemon uses `nix --profile <state_dir>/profile-<target>` to create GC
roots. This is the same approach as nix-direnv: the profile symlink is a GC
root that Nix manages automatically, protecting the entire devshell closure
from `nix-collect-garbage`.

Previous versions used manual symlinks with `nix-store --add-root`, which
silently failed when store path resolution errored. The profile-based
approach is robust and self-managing.

## Version upgrades

When xi is upgraded, the client detects a version mismatch:

1. Client sends `StatusRequest` to the daemon
2. Daemon returns its version in `StatusResponse`
3. If versions differ, client sends `ShutdownRequest`
4. Client spawns a new daemon with the current version
5. Cache version check: same version = warm start from `meta.json`;
   different version = state directory is nuked and rebuilt

## State locations

| Path | Content |
|------|---------|
| `$XDG_RUNTIME_DIR/xi-develop/daemon.sock` | Daemon Unix socket |
| `$XDG_STATE_HOME/xi/develop/` | Environment files, metadata |
| `$XDG_CONFIG_HOME/xi/develop/trusted/<id>` | Trust markers |

## Daemon states

```
Starting       → Daemon initialising
Evaluating     → Running nix evaluation
Ready          → Serving current environment
BuildFailed    → Evaluation failed, serving cached env
WatcherDegraded → File watcher failed, daemon still works
ConfigError    → Bad configuration detected
ShuttingDown   → Graceful shutdown in progress
```

The daemon never crashes on errors. Fatal evaluation failures are caught,
the last-good environment is preserved, and a notification is shown. The
daemon waits for file changes before retrying.

## Design rationale

**Why a daemon?** Without a daemon, every prompt would re-evaluate the flake
or re-read cached state. A daemon amortises evaluation across all terminals
and provides genuine live reload.

**Why polling (prompt hook)?** Prompt hooks are universal across bash/zsh/fish,
require no background processes in the shell, and avoid race conditions with
signal-based approaches. The latency (one prompt cycle) is imperceptible.

**Why subshells?** Subshells preserve the user's shell configuration. In-place
environment modification (`eval` in the current shell) is fragile and hard
to undo. Subshells provide a clean entry/exit boundary.

**Why content hashing?** Without content hashing, editing a comment in
`flake.nix` would trigger a re-source. Content hashing ensures that only
meaningful changes bump the generation counter.

**Why A/B slots?** If the shell sources an env file while the daemon is
writing to it, the shell gets a partial read. A/B slot switching writes to
the inactive slot and atomically swaps the pointer, ensuring the shell
always reads a complete file.
