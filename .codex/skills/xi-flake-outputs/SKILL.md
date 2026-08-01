---
name: xi-flake-outputs
description: Enumerate the actual outputs a xi/Nix flake exposes — packages, devShells, checks, apps, formatters, NixOS/home/darwin configurations, overlays, libs, deploy targets. Use when the user asks "what can I build/run/deploy here?", "which packages does this flake have?", "is there a devShell?", "what does this project deploy?", or before running `nix build`, `nix run`, `xi build`, or `xi deploy` with a name the agent might otherwise guess. Do NOT trigger for "what changed?" or "how do I install this?" — those are different skills.
---

# xi-flake-outputs

## Purpose

Return every reachable flake output classified by semantic kind (derivation, module, configuration, overlay, lib, app, ...). One call, no guessing.

## When to use

- User asks about buildable targets, runnable apps, or deploy nodes without naming them.
- Before invoking `xi build .#foo`, `xi run .#bar`, `xi deploy` — verify `foo`/`bar` exists.
- When distinguishing `packages` from `checks` or `formatter` matters (e.g. "run the formatter" vs "run the checks").

## How to call

```bash
xi agent outputs                    # current system only
xi agent outputs --all-systems      # every system the flake declares
xi agent outputs --system aarch64-darwin
xi agent outputs --include-hidden   # include debug, allSystems, etc.
```

## Reading the response

`data.outputs[]` is sorted by `category` then `name`. Each entry:

- `category` — matches `xi flake show` categories (`packages`, `devShells`, `checks`, `apps`, `formatter`, `nixosConfigurations`, `homeConfigurations`, `overlays`, `lib`, `deploy`, `colmenaHive`, `diskoConfigurations`, ...).
- `kind` — semantic type (`derivation`, `app`, `nixos-configuration`, `overlay`, `module`, `configuration`, `lib`, `template`, `function`, `test`, `unknown`).
- `name` — attribute name.
- `installable` — a ready-to-use flake reference like `.#packages.x86_64-linux.hello`. Pass this straight to `xi build`, `nix build`, etc.

`data.hidden[]` lists categories that were suppressed unless `--include-hidden` was passed.

## Common questions this skill answers

| User asks | Where to look |
| --- | --- |
| "What can I build?" | `outputs[].category == "packages"` |
| "What can I run?" | `outputs[].category == "apps"` |
| "What does the CI check?" | `outputs[].category == "checks"` |
| "Which hosts can I deploy?" | `outputs[].category == "nixosConfigurations"` or `"deploy"` or `"colmenaHive"` |
| "What formatter does this use?" | `outputs[].category == "formatter"` |

## Failure surface

- `nix flake show` failed: `data.outputs = []`, `errors: [{ code: "flake.eval.failed", source: "nix flake show", message }]`. Exit 0. Do not retry immediately — read the message.
- Not a flake: same shape as above with a `workspace.not-a-flake` diagnostic.
