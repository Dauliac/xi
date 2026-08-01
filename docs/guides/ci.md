# How to run CI

`xi ci` validates and builds a flake in one command. It is more thorough than
`nix flake check`: it verifies your `flake.lock` is in sync, evaluates every
system, runs your eval-time tests, checks your `lib` deeply, verifies
materialized artefacts are fresh, and only then builds everything.

## Run it

```sh
xi ci
```

By default this runs against `.` in your current directory. Pass a flake ref to
target something else: `xi ci github:you/repo`.

If any step reports FAIL, `xi ci` exits non-zero.

## What `xi ci` does for you

`xi ci` groups a validation stage (fast, parallel) and a build stage (slower):

| Step              | What it checks                                       | Skip flag                               |
| ----------------- | ---------------------------------------------------- | --------------------------------------- |
| Lock check        | `flake.lock` is in sync with `flake.nix`             | `--no-lock-check`                       |
| Eval all systems  | Every declared system evaluates cleanly              | `--no-eval`                             |
| Health check      | `xi doctor` diagnostics (input freshness, branch)    | `--no-health-check`                     |
| Eval tests        | `lib.runTests` evaluates (assertion failures caught) | `--no-test`                             |
| Lib eval          | `lib` deep-evaluates                                 | `--no-lib-eval`                         |
| Materialize check | Committed materialized files are fresh               | Set `check-in-ci = false` in `.xi.toml` |
| Build             | Every buildable output builds                        | `--no-build`                            |

Each step reports name, status (ok / warn / FAIL), duration, and detail.

## Choose a build backend

| Backend          | Strategy                                       |
| ---------------- | ---------------------------------------------- |
| `auto` (default) | nix-fast-build if installed, else devour-flake |
| `devour-flake`   | Single evaluation, builds everything           |
| `nix-fast-build` | Parallel evaluation with pipelined builds      |

```sh
xi ci --backend nix-fast-build
```

Persist the default in `.xi.toml`:

```toml
[ci]
backend = "nix-fast-build"
```

Or user-wide in `config.toml`:

```toml
[build]
ci_backend = "nix-fast-build"
```

## Discover extra outputs

`xi ci` builds packages, checks, devShells, apps, and system configurations
automatically. For custom outputs (`containers`, `images`, etc.), list them in
`.xi.toml`:

```toml
[ci]
extra-outputs = ["containers", "images"]
```

## Validate a monorepo

```sh
xi ci --recursive
```

Walks every `flake.nix` under the project root, runs CI on each, and reports: "N
of M subflake(s) failed CI".

## Fast pre-merge check (no build)

```sh
xi ci --no-build
```

Runs validation only. Useful in pull-request pipelines where a downstream job
does the full build.

## Collect every failure

```sh
xi ci --continue-on-error
```

By default `xi ci` stops at the first FAIL. This flag runs every step and
reports all failures at the end.

## Restrict to the current system

```sh
xi ci --current-system-only
```

Skip cross-system evaluation. Right for local dev; wrong for release CI.

## Disallow import-from-derivation

```sh
xi ci --no-ifd
```

## Materialization

Xi can pre-compute expensive evaluations (Cargo hashes, generated Nix files,
prefetch outputs) and commit the results to git so CI doesn't have to redo them.

### Configure targets in `.xi.toml`

```toml
[materialize]
commit-path = "nix/materialized"   # where committed files go
check-in-ci = true                 # fail CI if stale
pre-build = true                   # rebuild stale before build
git-hide = true                    # apply skip-worktree
auto-stage = true                  # git add after commit
auto-stage-branches = ["main"]     # restrict auto-stage to specific branches

[[materialize.target]]
name = "cargo-hash"
command = "nix eval .#cargoHash --json"
output = "cargo-hash.json"
sources = ["Cargo.lock", "Cargo.toml"]
```

### Materialization commands

```sh
xi materialize                # run stale targets
xi materialize --force        # ignore cache, re-run every target
xi materialize --commit       # also write to nix/materialized/
xi materialize --check        # verify freshness (exit 1 if stale)
xi materialize --list         # show targets and their staleness
xi materialize --setup        # apply git skip-worktree + merge driver
xi materialize --clean        # remove the cache directory
```

Freshness is computed from a SHA-256 of every file matched by `sources`. If the
hash matches, the target is skipped.

With `check-in-ci = true`, `xi ci` fails when any materialized target is stale.
With `pre-build = true`, `xi ci` re-runs the stale ones before building.

## Standalone checks

If you only want `nix flake check`:

```sh
xi check
```

## Build without CI validation

```sh
xi build .#hello                # single package
xi build --all                  # all outputs via devour-flake
xi build --recursive            # subflakes too
```

## Useful CI flags

```sh
xi ci --keep-going           # keep going on build failures
xi ci --show-trace           # detailed eval errors
xi ci --no-nom               # plain output (better for CI log capture)
xi ci --max-jobs 4           # limit parallelism
xi ci --dry-run              # print the plan without executing
xi ci --offline              # no network access
```

## See also

- [CLI Reference: `xi ci`](../reference/cli.md#xi-ci--ci-pipeline)
- [Configuration Reference: `[ci]`, `[materialize]`](../reference/configuration.md)
