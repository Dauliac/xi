# Design: ci-nix-publish

## Context

Existing CI (`build.yaml`, `check.yaml`, `test.yaml`, `tag.yaml`,
`nixos-search.yaml`, `changelog.yaml`, `cleanup.yaml`) evaluates `xi` under Nix
with `cachix/install-nix-action` and throws away every store path at end-of-job.
No shared cache, no downstream distribution channel, and no way to iterate on
workflow YAML without pushing.

Two upstream shifts inform this design:

- `DeterminateSystems/magic-nix-cache-action` was archived in Feb 2025; its
  backend went away. Any new "magic cache" wiring must pick a live successor.
- `nix-installer-action` explicitly supports `nektos/act` (auto-detects, skips
  systemd, works with `--privileged --userns=host`). `cachix/install-nix-action`
  does not, per its own issue tracker.

This is a CI-infra change. It does not touch runtime crates or the daemon
protocol.

## Goals

- PRs from repo members hit a warm cache (~4 min p50) instead of building from
  scratch.
- The published `xi` binary — the _exact_ store path that CI validates — is what
  Nix, NUR, and Docker Hub consumers pull. No parallel build paths that can
  drift.
- Fork PRs pass CI without secrets — no red X for external contributors.
- Every new workflow can be run end-to-end on a developer laptop via
  `just act-<name>` before it is merged.
- OCI image is byte-reproducible from `nix build .#container`, with no
  `Dockerfile` and no reliance on the local Docker daemon at build time.

## Non-Goals

- Migrating existing workflows off `cachix/install-nix-action` (churn without a
  caching-parity gain — new workflows use `nix-installer-action` for `act`
  support, existing ones stay).
- Building a self-hosted binary cache (Cachix is enough; S3 is a follow-up if
  quota bites).
- Publishing a `home-manager` / `NixOS` module set to NUR (only the `xi` package
  is exposed; modules stay in `modules/`).
- Automating the one-time NUR registration PR (documented + tracked, not
  scripted — the PR needs a human maintainer's account).
- Reproducible-across-time container digests (dockerTools output is
  content-addressed but timestamps in inner layers may vary — good enough for
  MVP).
- Signing container images with `cosign` (deferred; can be layered later without
  changing this design).

## Decisions

### Decision: Cache stack — `cache-nix-action` (GHA cache) under `cachix-action` (durable)

**Choice**: Use both. `nix-community/cache-nix-action@v7` first (restores store
from GHA cache, saves at job end) and `cachix/cachix-action@v17` second (pushes
to `dauliac.cachix.org` on push-to-`master`, pulls on all events, skips push on
forks).

**Rationale**: They occupy different tiers. `cache-nix-action` gives cheap
per-repo speedup within GHA's 10 GB/repo budget (subject to LRU eviction) — good
for feedback on the same PR. Cachix is the durable, publicly shareable cache
that downstream users and NUR's aggregator pull from — persistent, no LRU per
branch. Empirical benchmarks put `cache-nix-action` first for pure CI-time
reduction and Cachix second, but Cachix is the only one that also serves
end-users, so we take both.

**Config detail**: `cache-nix-action` set with `gc-max-store-size: 5G` and
`purge-created: 604800` (7 days) — leaves headroom for Cachix's post-fetch adds
and avoids the runaway-eviction trap where old caches evict fresh ones.

**Alternatives considered**:

- `DeterminateSystems/flakehub-cache-action` (rejected — requires a paid
  FlakeHub subscription; blocks contributors without a Determinate account).
- `magic-nix-cache-action` (rejected — archived).
- Cachix only, no `cache-nix-action` (rejected — first push in a run cold-starts
  Cachix fetches for every fresh store path; adds ~2 min per job that GHA-cache
  eliminates for zero infra cost).
- Self-hosted S3 + `nix-serve` (rejected — operations burden with no measurable
  win over Cachix for a repo this size).

### Decision: Nix installer — split by generation

**Choice**: Existing workflows keep `cachix/install-nix-action` (no forced
churn). New workflows introduced by this change use
`DeterminateSystems/nix-installer-action@main`.

**Rationale**: `nix-installer-action` is the only installer that runs cleanly
under `act` — it auto-detects the runner and skips systemd. Since the whole
point of the `just act-*` recipes is to debug these _new_ workflows before
merge, they must be `act`-compatible. Old workflows are not the ones we intend
to debug locally; changing them is churn without a payoff.

**Alternatives considered**: install-nix-action everywhere (rejected — breaks
act debugging); nix-installer-action everywhere (rejected — churn on stable
workflows for zero user-visible gain).

### Decision: Fork safety — pull-only, no failed steps

**Choice**: `cachix-action` sets
`skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}`.
Container publish and NUR-build workflows guard the push step behind
`if: github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository`.
`DOCKERHUB_TOKEN` and `CACHIX_AUTH_TOKEN` are unreferenced in the build-only
path.

**Rationale**: Fork PRs run without secrets in GitHub Actions. If a step calls a
secret-shaped variable that is empty, it fails. Guarding at the workflow level
(not just the action) means a fork PR sees green checks on build steps and no
red X on skipped publish steps.

**Alternatives considered**: `pull_request_target` + `permissions: write`
(rejected — well-known code-execution-with-secrets footgun); disable CI on fork
PRs (rejected — hostile to contributors).

### Decision: OCI image — `dockerTools.buildLayeredImage` + `skopeo`, not `docker build` or `nix2container`

**Choice**: New `nix/container.nix` produces `packages.<system>.container` via
`pkgs.dockerTools.buildLayeredImage`. The publish workflow does
`nix build .#container` (produces a `docker-archive` tarball at `./result`) then
`skopeo copy docker-archive:./result docker://docker.io/dauliac/xi:<tag>`. No
`docker load`, no Docker daemon.

**Rationale**:

- `dockerTools` is in-tree in nixpkgs, no new input; layering matches the `xi`
  closure so pulls are incremental across versions.
- `skopeo` streams the tarball straight to the registry — no daemon dependency,
  faster in CI, and works identically under `act` (where a real Docker daemon is
  often missing).
- `nix2container` (nlewo) is technically cleaner (skips the intermediate
  tarball) but adds a flake input and its layering is less
  standard-tooling-friendly. Not worth it for MVP.

**Gotcha (must-fix)**: `dockerTools.buildLayeredImage` sets the manifest
`architecture` from the _host_ system, not the target. Cross-built images ship
with the wrong arch label unless `architecture` is passed explicitly
(nixpkgs#106695). `nix/container.nix` sets
`architecture = pkgs.go.GOARCH or "amd64"` (or an explicit string matched to the
current system).

**Alternatives considered**: `docker/build-push-action` with a `Dockerfile`
(rejected — duplicates the build the flake already knows how to do; drifts);
`nix2container` (rejected — extra input, not enough win); `podman + buildah`
(rejected — same daemon-dependency shape as `docker`, minor gain).

### Decision: Multi-arch — cross-compile with `pkgsCross`, not QEMU

**Choice**: For arm64 (post-MVP), the workflow runs on `ubuntu-latest` (amd64)
and does two builds: `nix build .#container` (amd64) and
`nix build .#packages.aarch64-linux.container` where the aarch64 package uses
`pkgsCross.aarch64-multiplatform`. Both are `skopeo copy`'d as `xi:<tag>-amd64`
/ `xi:<tag>-arm64`, then `docker manifest create/push` stitches a manifest list
at `xi:<tag>`.

**Rationale**: QEMU under `binfmt_misc` is 5–10× slower and triggers the
arch-mislabel bug. Cross-compilation via `pkgsCross` is well-trodden with
`crane` (already in this flake) and runs at native amd64 speed.

**Alternatives considered**: dedicated arm64 runner (rejected for MVP — costs
money on GH Actions unless self-hosted); QEMU (rejected — slow + buggy); ship
amd64-only forever (rejected — arm64 laptops are >30% of contributor base).

### Decision: NUR shape — root `default.nix` + `ci.nix`, no `flake-compat`

**Choice**: New `default.nix` at the repo root:

```nix
{ pkgs ? import <nixpkgs> {} }:
{
  xi = pkgs.callPackage ./package.nix {};
}
```

New `ci.nix`:

```nix
let
  pkgs = import <nixpkgs> {};
  nurAttrs = import ./default.nix { inherit pkgs; };
in
  nurAttrs // { recurseForDerivations = true; }
```

The `flake.nix` is untouched; NUR sees the repo through `default.nix`, which
delegates to the same `package.nix` that the flake consumes via crane.

**Rationale**: NUR evaluates with `restrict-eval = true` and
`allow-import-from-derivation = false`. A `flake-compat` shim works but pulls a
flake input in through IFD-adjacent paths and complicates NUR's evaluator's job.
A hand-written `default.nix` is 5 lines and evaluator-safe.

**Alternatives considered**: `flake-compat` (rejected — extra moving part, less
predictable under `restrict-eval`); publishing as a standalone `nur-packages`
repo (rejected — splits the source of truth and doubles CI).

**Constraint**: `package.nix` must remain IFD-free and must not use
`builtins.fetchGit` / `fetchTarball` on unpinned URLs. Current `package.nix`
already meets this (crane derives from `Cargo.lock` locally).

### Decision: NUR aggregator wiring — separate `nur-build.yaml`, own cachix, daily cron

**Choice**: New `.github/workflows/nur-build.yaml` runs on push, pull_request,
workflow_dispatch, and `schedule: '51 2 * * *'` (nightly, randomized minute to
avoid GH cron thundering herd). It runs
`nix-build-uncached ci.nix -build-flags "-L"` and pushes results to Cachix
`dauliac`. The one-line PR to `nix-community/NUR` `repos.json` points at this
repo and tags the cache as `dauliac.cachix.org`.

**Rationale**: NUR's evaluator hits this workflow's cache when serving
downstream users. Nightly cron matches NUR's own re-evaluation cadence (catches
nixpkgs channel drift). `nix-build-uncached` (not `nix-build`) is what the NUR
template uses — it skips derivations already in the substituter, avoiding wasted
rebuilds.

### Decision: Tag scheme — `docker/metadata-action@v5`, semver + sha + latest

**Choice**: Container publish uses `docker/metadata-action@v5` with:

- `type=ref,event=branch` → `master` → `latest`
- `type=semver,pattern={{version}}` → git tag `v1.2.3` → `1.2.3`, `1.2`, `1`
  (only for stable, not `-beta`)
- `type=semver,pattern={{version}},suffix=-beta` for beta tags
- `type=sha,prefix=sha-,format=short` on every push → `sha-abc1234`

No latest on beta tags. PRs build but do not push.

**Rationale**: Matches the existing tag workflow's beta/stable split (`tag.yaml`
already distinguishes them). Semver expansion is idiomatic and lets users pin to
`dauliac/xi:1` or `:1.2` for minor-track pins.

**Alternatives considered**: `pr-<num>` tags on PRs (rejected — clutters the
registry, no clear use case).

### Decision: `act` — installer swap + `--privileged --userns=host`, runner image pinned

**Choice**: `.actrc` sets `-P ubuntu-latest=catthehacker/ubuntu:full-latest` and
`-P ubuntu-24.04=catthehacker/ubuntu:full-24.04`. Justfile recipes invoke `act`
with `--privileged --container-options="--userns=host" --secret-file .secrets`.
All new workflows use `DeterminateSystems/nix-installer-action@main` (works
under `act` per `nix-installer-action#33`).

**Rationale**: `install-nix-action` cannot create a user namespace under `act` —
hard failure. `nix-installer-action` auto-detects.
`catthehacker/ubuntu:full-latest` is the largest of the three official act
runners and has the closest package set to real `ubuntu-latest`. FlakeHub Cache
and GHA cache are known-broken under act — the local run will cold-build the
store, but the point is to prove the workflow's shape works, not to bench it.

**Alternatives considered**: use `act-latest` (smaller, missing some deps that
Nix needs); rewrite workflows to gate on `env.ACT` (rejected — special-casing
act in the primary path adds review burden).

## Verification strategy

Concrete scenarios that prove the change works — mapped to `tasks.md` items:

- **S1 (cache — GHA layer)**: Push to a branch twice. Second push's
  `nix build -L --no-link` completes in ≥ 60% less wall-time than the first.
  Verified in the workflow log's step timing.
- **S2 (cache — Cachix push)**: After a merge to `master`,
  `nix path-info --store https://dauliac.cachix.org $(nix build --print-out-paths .#xi)`
  returns a valid narinfo. Verified from a laptop with no local build.
- **S3 (cache — fork safety)**: A PR from a fork completes CI green with no
  attempted push. Verified in a scratch fork's Actions log — no red X, no error
  text about missing secrets.
- **S4 (container — build)**: `nix build .#container` produces a tarball at
  `./result`. `skopeo inspect docker-archive:./result` reports
  `Architecture: amd64` and the version label. Verified via `nix run` + `skopeo`
  locally.
- **S5 (container — publish)**: After merging a PR that bumps version,
  `docker pull dauliac/xi:latest && docker run --rm dauliac/xi --version` prints
  the new version. Verified from a clean host.
- **S6 (NUR — evaluator compat)**:
  `nix-instantiate --strict --eval-only ci.nix --arg pkgs 'import (fetchTarball https://nixos.org/channels/nixos-unstable/nixexprs.tar.xz) {}' --option restrict-eval true --option allow-import-from-derivation false`
  returns a valid derivation set. Verified locally and in the NUR-build
  workflow.
- **S7 (NUR — cache serves)**: After NUR registration,
  `nix build github:nix-community/NUR --refresh #repos.dauliac.xi` completes
  without a local rebuild (all paths in Cachix). Verified from a clean host.
- **S8 (act — smoke)**: `just act-container-publish` runs the workflow
  end-to-end locally on a Linux dev box, produces a tarball, and (with
  `.secrets` present) pushes to a scratch tag. `just act-cache-warm` completes
  without error.
- **S9 (existing workflows unaffected)**: `just act-build` (or the equivalent GH
  Actions run on a PR) still passes with the modified `build.yaml` /
  `check.yaml` / `test.yaml`. No new required check that blocks fork PRs.

## Open questions (call out before implementation starts)

1. **Cachix cache name**: The design assumes `dauliac`. Confirm the name is
   available (or use another) before workflow YAML lands — the token and URL
   both depend on it.
2. **Docker Hub org**: The design assumes `dauliac/xi`. If a `dauliac` org
   exists but this project should live under a different one (e.g. `xi-nix/xi`),
   the metadata-action `images` list needs to match.
3. **Multi-arch scope**: MVP is amd64. Include arm64 (`pkgsCross`) in v1 or ship
   it as a follow-up bead? Design allows either.

None of the three block the MVP fan-out — the tasks decompose independently.
