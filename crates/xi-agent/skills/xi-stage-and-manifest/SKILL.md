---
name: xi-stage-and-manifest
description: Identify which files Nix will actually see for a build and which ones are still unstaged. Use before proposing `git add`, before running `xi build`/`nix build` after an edit, or when a change "did not take effect". Answers "which files must I stage?", "does Nix see this file?", "what files make up the flake?". Triggers on "why is my change not being built?", "I edited flake.nix — will `nix build` pick it up?", and any workflow where the developer edits `.nix` files. Do NOT trigger for pure code review or when the working tree is clean.
---

# xi-stage-and-manifest

## Purpose

Nix only sees files known to git. This skill exposes:

- **`stage`** — files git tracks or ignores that a Nix build path would touch, with the referring chain from `flake.nix`. Use it to propose the minimal correct `git add`.
- **`manifest`** — all files reachable from `flake.nix` via imports. Use it to answer "what is the flake, actually?"

## When to use

- The user says "my change is not being picked up by Nix."
- The user asks to stage files before a build.
- The user asks which files make up the flake (for review, refactor, or splitting).
- Before proposing `git add <path>` — check whether the flake even references the file.

## How to call

```bash
xi agent stage        # unstaged / untracked files (with flake-referrer info)
xi agent manifest     # every file reachable from flake.nix
```

## Reading `stage`

- `data.clean == true` and `data.entries == []` → nothing to stage. Move on.
- Each entry has `path`, `git-status` (`untracked` / `modified` / `ignored` / `deleted` / `renamed`), `staged` (bool), and `referenced-by[]`.
- If `referenced-by` is non-empty for an `untracked` entry, `git add <path>` is almost certainly required before `nix build`.
- Sorted deterministically by path — safe to diff across runs.

## Reading `manifest`

- `data.root` — always `flake.nix`.
- Each entry: `path`, `kind` (`flake-root` / `module` / `overlay` / `lib` / `package` / `other`), and `imported-by[]`.
- Use `kind == module` to find the effective modules of the project without evaluating.
- `imported-by` is empty for the root and populated for everything reachable — reversed edges from `flake.nix`.

## Common workflows

**"Nix ignored my change"**

1. `xi agent stage` — look for untracked files with non-empty `referenced-by`.
2. Propose exactly `git add <those-paths>`.
3. Re-run the build.

**"Split the flake into smaller modules"**

1. `xi agent manifest` — count entries per `kind`.
2. Group by directory prefix under `modules/`.
3. Refactor with confidence that the imports graph is what you think.

## Failure surface

- Not a git repo: `stage` returns `errors: [{ code: "git.spawn-failed" | "git.status-failed" }]`, `entries: []`, `clean: true`.
- Not a flake: `manifest` returns `errors: [{ code: "workspace.not-a-flake" }]`, `files: []`.
