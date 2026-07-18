# How to Run CI with Xi

`xi flake ci` runs a multi-phase validation and build pipeline for your
flake. It is not just "build everything" — it validates, evaluates, tests,
checks lib outputs, verifies materialized files, and then builds.

## The pipeline

### Phase 1 — Validation (parallel)

These steps run concurrently with progress spinners:

| Step | What it does | Skip flag |
|------|-------------|-----------|
| Lock check | Verify `flake.lock` is in sync | `--no-lock-check` |
| Eval all systems | `nix flake show --json` to discover outputs | `--no-eval` |
| Health check | Run `xi flake doctor` diagnostics | `--no-health-check` |
| Eval tests | Evaluate `lib.runTests` (catches assertion failures) | `--no-test` |
| Lib eval | Deep-evaluate `lib` with `builtins.deepSeq` | `--no-lib-eval` |
| Materialize check | Verify materialized files are fresh | (via `.xi.toml` `check-in-ci`) |

Each step reports: name, status (ok/warn/FAIL), duration, and detail.

### Phase 2 — Build (sequential)

Builds all flake outputs using the selected backend, plus any extra outputs
discovered from the flake show JSON.

## Run it

```sh
xi flake ci
```

## Choose a CI backend

| Backend | Strategy |
|---------|----------|
| `auto` (default) | nix-fast-build if available, else devour-flake |
| `devour-flake` | Single evaluation, builds everything |
| `nix-fast-build` | Parallel evaluation with pipelined builds |

```sh
xi flake ci --backend nix-fast-build
```

Configure the default in `.xi.toml`:

```toml
[ci]
backend = "nix-fast-build"
```

Or in `config.toml`:

```toml
[build]
ci_backend = "nix-fast-build"
```

## Discover extra outputs

By default, devour-flake handles packages, checks, devShells, apps, and
system configurations. If your flake has custom outputs (e.g.
`containers`, `images`), add them to `.xi.toml`:

```toml
[ci]
extra-outputs = ["containers", "images"]
```

Xi discovers derivation nodes in these paths from the flake show JSON and
builds them separately.

## Recursive subflakes

Validate and build all subflakes in a monorepo:

```sh
xi flake ci --recursive
```

Xi discovers all `flake.nix` files under the project root, runs CI on each,
and reports a summary: "N of M subflake(s) failed CI".

## Validation-only mode

Skip the build phase entirely:

```sh
xi flake ci --no-build
```

This runs Phase 1 only — useful for fast pre-merge checks.

## Continue on errors

By default, the pipeline stops at the first failure. To collect all errors:

```sh
xi flake ci --continue-on-error
```

## Restrict to current system

Skip cross-system evaluation:

```sh
xi flake ci --current-system-only
```

## Disallow import-from-derivation

```sh
xi flake ci --no-ifd
```

## Materialization in CI

If `.xi.toml` has `check-in-ci = true`, Phase 1 verifies that all
materialized targets are fresh. If any are stale, the step fails with:
"N of M target(s) are stale".

If `pre-build = true`, stale targets are rebuilt before Phase 2 starts.

See [Materialization](#materialization) below.

## Run flake checks standalone

```sh
xi flake check
```

Runs `nix flake check` piped through nom for pretty output.

## Build specific outputs

```sh
xi flake build .#hello                # single package
xi flake build --all                  # all outputs via devour-flake
xi flake build --recursive            # including subflakes
```

## Materialization

Xi can pre-compute expensive evaluations, cache results based on source
file hashes, and optionally commit them to git.

### Configure targets in `.xi.toml`

```toml
[materialize]
commit-path = "nix/materialized"   # where committed files go
check-in-ci = true                 # fail CI if stale
pre-build = true                   # rebuild stale before build
git-hide = true                    # apply skip-worktree
auto-stage = true                  # git add after commit
auto-stage-branches = ["main"]     # restrict auto-stage to branches

[[materialize.target]]
name = "cargo-hash"
command = "nix eval .#cargoHash --json"
output = "cargo-hash.json"
sources = ["Cargo.lock", "Cargo.toml"]
```

### Run materialization

```sh
xi flake materialize                 # run stale targets
xi flake materialize --force         # ignore cache, re-run all
xi flake materialize --commit        # also write to nix/materialized/
xi flake materialize --check         # verify freshness (exit 1 if stale)
xi flake materialize --list          # show targets and staleness
xi flake materialize --setup         # apply git skip-worktree + merge driver
xi flake materialize --clean         # remove cache directory
```

### How freshness works

Each target's sources are glob-matched, and their contents are SHA-256
hashed. The hash is stored in `.xi/materialized/<target>.hash`. On the next
run, if the hash matches, the target is skipped.

### Git lifecycle

With `--commit` and `git-hide = true`:

1. Lift skip-worktree from existing files
2. Run targets, write to cache and commit directories
3. If `auto-stage`, run `git add` on committed files
4. Re-apply skip-worktree to hide files from `git status`

The `--setup` command also adds a `.gitattributes` merge driver
(`merge=ours`) to prevent merge conflicts on materialized files.

## Common CI flags

```sh
xi flake ci --keep-going           # continue on build failures
xi flake ci --show-trace           # detailed eval errors
xi flake ci --no-nom               # plain output (for CI logs)
xi flake ci --max-jobs 4           # limit parallelism
xi flake ci --dry-run              # print actions without executing
xi flake ci --offline              # no network access
```
