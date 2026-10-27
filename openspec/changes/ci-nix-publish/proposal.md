# Change: ci-nix-publish

## Why

Today's CI (`build.yaml`, `check.yaml`, `test.yaml`, `tag.yaml`) evaluates and
builds the flake from scratch on every push, then throws the store paths away.
Nothing is shared:

- Every PR pays the full Rust + Nix build cost (10–15 min); reviewers wait,
  contributors are discouraged.
- There is no public binary cache — users who install `xi` from the flake
  rebuild the whole world on their first pull.
- The project is not discoverable via **NUR** (`nix-community/NUR`), so Nix
  users cannot
  `nixpkgs.overlays = [ nur.overlay ]; environment.systemPackages = [ nur.repos.dauliac.xi ];`.
- There is no **OCI image** — the daemon + hook cannot be tried out from
  `docker run …` for CI-hostile environments or documentation demos.
- Debugging any new workflow locally requires either pushing to a scratch branch
  or trusting the diff blind; `nektos/act` is not wired up.

The `magic-nix-cache-action` referenced in older Nix CI guides was archived in
Feb 2025 (backend shut down). Any new caching layer must pick its successor, and
the choice interacts with fork-safety (secrets are unavailable in fork PRs), so
it is worth designing before implementing.

## What Changes

- **Shared substituter** — a public **Cachix** cache (`dauliac`, prerequisite:
  created out-of-band) fronted by `cachix/cachix-action@v17`. All CI jobs push
  on push-to-`master`; PRs pull only; forks skip push (no secrets) and just
  build.
- **Intra-run cache reuse** — `nix-community/cache-nix-action@v7` layered under
  Cachix so a single workflow re-run reuses the previous run's store within the
  10 GB GHA-cache budget (gc'd via `gc-max-store-size = 5G`).
- **NUR compatibility** — new root `default.nix` + `ci.nix` at the repo root
  that expose the same package the flake builds but under NUR's evaluator
  (`restrict-eval = true`, no IFD, all inputs pinned). A new
  `.github/workflows/nur-build.yaml` runs `nix-build-uncached ci.nix` on push +
  a daily cron and pushes results to Cachix, matching the pattern NUR's
  aggregator expects.
- **NUR registration** — a one-time PR against `nix-community/NUR` `repos.json`
  adding this repo. Tracked as a documented task so it does not become tribal
  knowledge.
- **OCI image via Nix** — new `nix/container.nix` exposes
  `packages.<system>.container` built with `pkgs.dockerTools.buildLayeredImage`
  (thin layer over `xi`, correct `architecture` field to dodge nixpkgs#106695).
  No `Dockerfile`, no dependency on the Docker daemon at build time.
- **Docker Hub publish** — new `.github/workflows/container-publish.yaml`: on
  tag or `master`, `nix build .#container` →
  `skopeo copy docker-archive:./result docker://docker.io/dauliac/xi:<tag>`.
  Skopeo is preferred over `docker load` + `docker push` because it avoids the
  Docker daemon entirely and streams straight to the registry. Tags derived by
  `docker/metadata-action@v5` (`latest`, `vX.Y.Z`, `sha-<7>`).
- **Multi-arch container (follow-up)** — `linux/amd64` first (single-runner).
  `linux/arm64` via `pkgsCross.aarch64-multiplatform` in a matrix job with
  `docker manifest create/push` stitching. Deferred from MVP but scoped in
  `tasks.md`.
- **`act` support** — `.actrc` selecting `catthehacker/ubuntu:full-latest`,
  `.secrets.example` documenting the four secrets, and Justfile recipes
  (`just act-<workflow>`). New workflows use
  `DeterminateSystems/nix-installer-action@main` — it auto-detects `act` and
  skips systemd; `cachix/install-nix-action` is known-broken under `act`.
  Existing workflows keep `cachix/install-nix-action` (no churn — they are not
  the ones we want to debug locally).
- **Docs** — `docs/guides/ci-publishing.md` covering required repository
  secrets, how to seed the Cachix cache, how to register on NUR, how to pull the
  container image, and how to run any workflow locally with `just act-*`.

## Impact

- **Affected specs**: new capability **`ci-publish`** (first delta introduces
  its requirements).
- **Affected files**:
  - **New workflows**: `.github/workflows/cache-warm.yaml`,
    `.github/workflows/nur-build.yaml`,
    `.github/workflows/container-publish.yaml`.
  - **Modified workflows**: `build.yaml`, `check.yaml`, `test.yaml` gain
    `cache-nix-action` + `cachix-action` steps; `tag.yaml` gains a post-release
    cache warm.
  - **New Nix**: `nix/container.nix` (imported by `nix/packages.nix` or
    `nix/default.nix`); root `default.nix` + `ci.nix` (NUR-compat shims — do not
    touch `flake.nix`).
  - **New repo config**: `.actrc`, `.secrets.example` (both gitignored where
    appropriate).
  - **New docs**: `docs/guides/ci-publishing.md`.
  - **Modified**: `Justfile` (add `act-*` recipes), `README.md` (badge + NUR
    install snippet — small).
- **Breaking?**: **No**. Existing workflows continue to work if secrets are
  absent (`skipPush`, no failure). The flake's package set is unchanged from a
  Nix consumer's perspective; adding `packages.<system>.container` is additive.
- **Prerequisites** (out-of-band, must be done by a human with repo admin +
  third-party accounts):
  - Create Cachix cache `dauliac` (public, writable).
  - Create Docker Hub repository `dauliac/xi`.
  - Add repository secrets: `CACHIX_AUTH_TOKEN`, `CACHIX_SIGNING_KEY`,
    `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN`.
  - After merge: open the NUR registration PR (tracked as its own bead).

## Success Criteria

- A cold PR builds in **≤ 4 min** on average (down from ~12 min) when the Cachix
  cache has a warm entry for the closure.
- `nix-env -iA nur.repos.dauliac.xi -f https://github.com/nix-community/NUR/archive/main.tar.gz`
  installs a signed binary from the Cachix cache without local rebuild.
- `docker run --rm dauliac/xi --help` prints CLI help on `linux/amd64`.
- `just act-container-publish` runs the container-publish workflow end-to-end
  locally and pushes to a scratch Docker Hub tag (using `.secrets`).
- Fork PRs pass CI without any secret access (no failed steps, no aborted jobs).
