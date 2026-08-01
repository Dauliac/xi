---
name: xi-validate-changes
description: Run the exact set of checks the project supports before declaring a change "done" in a xi/Nix project. Use before commit, before push, before saying "I'm done", or whenever the user asks "is this good?" / "did I break anything?". Emits an ordered plan (formatter check, xi doctor, flake check, cargo test, xi ci) tailored to what THIS project actually configures, and optionally executes it and streams JSONL results per step. Triggers on "validate", "check my work", "before I commit", "run the tests", "is this ready?", "before I say done". Do NOT trigger for editor lint fixes or explicit single-command requests ("just run cargo test") — respect the user's specificity.
---

# xi-validate-changes

## Purpose

Turn "have I broken anything?" into a definitive yes/no. Every step:

- has a stable `id` (`fmt-check`, `doctor`, `flake-check`, `cargo-test`, `ci`)
- names its `command` argv, `purpose`, and `blocking` flag
- lists the `depends-on` steps that must pass first

## When to use

- Before proposing a commit.
- Before saying "done" to the user on any change that touches code, config, or modules.
- After an edit sequence, when the user says "check it".

## How to call

**Preview only** (no execution):

```bash
xi agent validate
```

**Execute and stream results**:

```bash
xi agent validate --run
xi agent validate --run --fail-fast
```

Execution emits one JSON line per event:

- `event: "started"` — step is running (with timestamp)
- `event: "finished"` — step done (`status: passed | failed | skipped | blocked`, `duration-ms`, optional `error` summary)
- `event: "complete"` — final summary (`total-ms`, `all-blocking-passed`)

Exit code: `0` iff every blocking step passed. `1` if any blocking step failed. `130` if the run was interrupted.

## Interpreting results

- `passed` — done, move on.
- `failed` — report the `error` field to the user; do not attempt to re-run the same step without a fix.
- `blocked` — a dependency failed; not a real failure of this step. Fix the dependency and re-run.
- `skipped` — the step opted out (rare; e.g. no changes for a scoped check).

## When to use `--fail-fast`

Prefer it when the developer is iterating: no point running `xi ci` if `xi fmt --check` already failed. Do NOT use `--fail-fast` when the developer explicitly wants a full report before deciding.

## Failure surface

- No `flake.nix`, no `Cargo.toml`, no `.xi.toml [ci]` → empty plan. Still emits an envelope; the agent should report "no configured validation for this project" rather than claim success.
- A step's binary is missing (`xi` not on PATH) → the step reports `failed` with a clear `error`. Do not suppress; surface it.
