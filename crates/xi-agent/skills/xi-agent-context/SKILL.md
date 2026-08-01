---
name: xi-agent-context
description: One-call opening turn for any coding session in a xi/Nix project. Use when the agent needs to understand where it is, what the flake exposes, whether the devshell is warm, what the git working tree looks like, and what validation the project supports — all before running language toolchains or proposing edits. Triggers on "what can I build here?", "is this a xi project?", "am I in the right shell?", "what should I check?", opening turns in unfamiliar Nix repos. Do NOT trigger for pure code questions where the workspace state is irrelevant.
---

# xi-agent-context

## Purpose

Load the entire project surface in one call. Returns:

- workspace root, current system, xi version
- flake summary (path, lock hash) or `null` if not a flake
- devshell state (from the running `xi develop` daemon if any)
- git state (head, branch, dirty, untracked count)
- validation plan for this project (dry — nothing runs)
- `.xi.toml` sections and configured formatter backend

## When to use

- First turn in a Nix-flake project you have not seen before.
- Any time the user's request depends on "what does this project support?" — build targets, checks, formatters, deploy backends.
- Before proposing edits to `flake.nix`, `modules/`, or `.xi.toml`.

## How to call

```bash
xi agent context
```

Optional flags:

```bash
xi agent context --system aarch64-linux    # override current system
xi agent context --no-daemon                # skip socket call, force NotRunning
```

## Reading the response

Response is one JSON object on stdout. Key fields:

- `data.workspace.root` — cwd of the project
- `data.flake` — `null` iff the cwd is not a flake; then treat as a plain project
- `data.devshell.state` — one of `ready`, `evaluating`, `stale`, `degraded`, `not-running`. If not `ready`, propose `xi develop` before running toolchain commands
- `data.git.dirty` — if `true`, some edits are unstaged
- `data.validation.steps[]` — the ordered plan of checks; each step has `id`, `command`, `purpose`, `blocking`
- `data.xi-config.fmt-backend` — the configured formatter (`nixfmt`, `alejandra`, `flake`, custom)

`errors[]` may be non-empty with `data` present — that means partial success. The `code` field is stable (e.g. `flake.eval.failed`, `daemon.not-running`).

## Failure surface

| Condition | Behaviour |
| --- | --- |
| Not a flake | `data.flake = null`; other reads still populated |
| Daemon not running | `data.devshell.state = "not-running"`; hint field points at `xi develop` |
| Slow flake eval | Partial `data` + `errors: [{ code: "flake.eval.failed", ... }]`, exit 0 |

## Follow-up skills

After `xi agent context`, reach for:

- `xi-flake-outputs` when the user asks what specific outputs exist.
- `xi-devshell-state` when devshell.state is anything other than `ready`.
- `xi-validate-changes` before saying "done".
- `xi-stage-and-manifest` before proposing `git add` or `nix build`.
