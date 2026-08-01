---
name: xi-devshell-state
description: Report whether the xi/Nix devshell is loaded, evaluating, stale, or missing before running language toolchains (cargo, npm, python, ...). Use when the user asks "why is 'cargo' not found?", "am I in the shell?", "is my devshell current?", or when the agent is about to run a toolchain command that lives inside the devshell. Triggers on "command not found", "wrong rustc version", "did the shell reload?". Do NOT trigger when the toolchain is clearly on PATH (system-wide install) or when the project has no flake.
---

# xi-devshell-state

## Purpose

Answer "is the devshell current and can I use its tools right now?" in one call. Backed by the running `xi develop` daemon; no `nix eval` in the read path.

## When to use

- The agent is about to invoke `cargo`, `npm`, `python`, or any tool provided by the devshell.
- The user sees `command not found` or a version mismatch.
- The user just edited `flake.nix` and the toolchain may be stale.

## How to call

```bash
xi agent devshell
xi agent devshell --timeout-ms 200   # reserved: sub-second daemon deadline
```

## Reading the response

`data.state` is the source of truth:

| State | Meaning | Recommend |
| --- | --- | --- |
| `ready` | Daemon reports Ready, target matches | Run the tool. |
| `evaluating` | Daemon is mid-eval | Wait a few seconds or ask the user to retry. Do not race. |
| `stale` | Daemon knows a newer flake exists | Ask user to re-enter (`xi develop`) or auto-suggest it. |
| `degraded` | Daemon is up but reports an issue | Read `daemon-state` for the specific phase; may still be usable. |
| `not-running` | No daemon for this project | Suggest `xi develop`. |

Also:

- `data.target` — the flake output the shell was entered for (e.g. `.#devShells.x86_64-linux.default`).
- `data.package-count` — how many packages the shell contains.
- `data.active-cache-pushes` — store paths currently being pushed. Nonzero means eval succeeded and a background push is running.

## Failure surface

Never blocks. If the socket does not exist, returns `state: not-running` immediately. If the socket transport fails, returns `state: degraded` with a `daemon.timeout` or `daemon.transport` diagnostic.

## Follow-ups

- If `state != ready` and the user's next step needs the toolchain: propose `xi develop` (project) or `xi develop enter` (nested shell). Do not run `nix develop` directly — it bypasses the daemon.
- If `state == ready` but the user hit `command not found` anyway, the daemon's PATH and the caller's PATH have drifted. Suggest the user runs `xi develop prompt` in their shell to re-source.
