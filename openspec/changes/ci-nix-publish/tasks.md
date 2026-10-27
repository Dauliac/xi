# Tasks: ci-nix-publish

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads. Each numbered bullet corresponds to one bead; parent-child links
mirror the section grouping. When multiple bullets touch the same file at
overlapping ranges, they are one bullet.

## 0. Prerequisites (human, out-of-band — track as one bead, no code)

- [ ] 0.1 Create Cachix cache `dauliac` (public, writable). Generate
      `CACHIX_AUTH_TOKEN` and `CACHIX_SIGNING_KEY` (Ed25519). Add both as
      repository secrets.
- [ ] 0.2 Create Docker Hub repository `dauliac/xi`. Generate a scoped access
      token with `Read, Write, Delete` on that repo only. Add
      `DOCKERHUB_USERNAME` + `DOCKERHUB_TOKEN` as repository secrets.
- [ ] 0.3 Confirm final names (`dauliac` cachix cache, `dauliac/xi` docker hub)
      before any workflow YAML lands.

## 1. Cache stack — Cachix + GHA cache action (P1)

- [ ] 1.1 Modify `.github/workflows/build.yaml`, `check.yaml`, `test.yaml`: add
      `nix-community/cache-nix-action@v7` (with `gc-max-store-size: 5G`,
      `purge-created: 604800`) before `cachix/install-nix-action@v31`, then
      `cachix/cachix-action@v17` after it. Set
      `skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}`
      and `pushFilter: "(-source$|nixpkgs\\.tar\\.gz$|\\.drv$)"`. Do NOT change
      the installer action in these files.
- [ ] 1.2 Add `.github/workflows/cache-warm.yaml`: on push to `master` and daily
      cron, `nix build .#checks .#packages.x86_64-linux.xi -L --no-link`, push
      to Cachix. Uses `DeterminateSystems/nix-installer-action@main` (this is a
      _new_ workflow — act-compatible installer).

## 2. Container package (P1 — blocks §3)

- [ ] 2.1 Add `nix/container.nix`: `pkgs.dockerTools.buildLayeredImage` wrapping
      `packages.<system>.xi`. Set `architecture` explicitly (matched to the
      current system) to dodge nixpkgs#106695. Include a
      `contents = [ pkgs.cacert pkgs.iana-etc ]` set so `xi` can hit HTTPS in
      the container. Config: `Cmd = [ "/bin/xi" ]`, `Env = [ "PATH=/bin" ]`,
      labels for source/version.
- [ ] 2.2 Wire `nix/container.nix` into `nix/packages.nix` so
      `packages.<system>.container` is a valid output. Verify: `nix flake show`
      lists `packages.x86_64-linux.container`.

## 3. Container publish workflow (P1 — depends on §2)

- [ ] 3.1 Add `.github/workflows/container-publish.yaml`: on push to `master`,
      on tag `v*`, and on workflow_dispatch. Steps: checkout →
      `nix-installer-action` → `cache-nix-action` → `cachix-action` (pull-only,
      no push here) → `docker/login-action@v3` (Docker Hub) →
      `docker/metadata-action@v5` (tags per Decision: Tag scheme) →
      `nix build .#container` →
      `skopeo copy docker-archive:./result docker://docker.io/dauliac/xi:<tag>`
      for each derived tag. Fork PRs: build only (skip login + push, guarded by
      `if:`).
- [ ] 3.2 Verify tag derivation locally with `docker/metadata-action` in dry-run
      mode; document the resulting tag matrix in `docs/guides/ci-publishing.md`.

## 4. Multi-arch container (P2 — deferred follow-up, depends on §3)

- [ ] 4.1 Add `packages.aarch64-linux.container` cross-built via
      `pkgsCross.aarch64-multiplatform`. Set `architecture = "arm64"`
      explicitly.
- [ ] 4.2 Extend `container-publish.yaml` to build both arches on the amd64
      runner, push each as `xi:<tag>-{amd64,arm64}`, then
      `docker manifest create/push` a fat manifest at `xi:<tag>`.

## 5. NUR compatibility (P1)

- [ ] 5.1 Add root `default.nix`:
      `{ pkgs ? import <nixpkgs> {} }: { xi = pkgs.callPackage ./package.nix {}; }`.
      Must not touch `flake.nix`.
- [ ] 5.2 Add `ci.nix`: import `default.nix`, mark
      `recurseForDerivations = true`. Verify with
      `nix-instantiate --strict --eval-only ci.nix --option restrict-eval true --option allow-import-from-derivation false`
      — no eval-time errors.
- [ ] 5.3 Audit `package.nix` for IFD or unpinned `fetchGit`/`fetchTarball`.
      Document any dependency that would fail NUR's evaluator and fix inline (or
      scope out with a rationale).

## 6. NUR build workflow (P1 — depends on §5)

- [ ] 6.1 Add `.github/workflows/nur-build.yaml`: triggers `push`,
      `pull_request`, `workflow_dispatch`, and `schedule: '51 2 * * *'`. Steps:
      checkout → `install-nix-action@v31` → `cachix-action@v17` (with
      `signingKey`, not just `authToken`) →
      `nix-build-uncached ci.nix -build-flags "-L"`. Fork PRs skip push.
- [ ] 6.2 Confirm `signingKey` (not just `authToken`) is what NUR's aggregator
      needs to trust our narinfo; document in `docs/guides/ci-publishing.md`.

## 7. NUR registration (P1 — depends on §5, §6)

- [ ] 7.1 Fork `nix-community/NUR`. Add an entry to `repos.json`:
      `{ "name": "dauliac", "url": "https://github.com/Dauliac/xi", "submodules": false }`.
      Open the PR. Link back from this repo's README once merged.
- [ ] 7.2 After NUR merge, verify from a clean host:
      `nix-env -iA nur.repos.dauliac.xi -f https://github.com/nix-community/NUR/archive/main.tar.gz`
      installs without local rebuild (pulls from Cachix). Records the S7
      evidence.

## 8. `act` local debugging (P2 — mostly independent, unblocks laptop dev)

- [ ] 8.1 Add `.actrc` at repo root: pin `ubuntu-latest` and `ubuntu-24.04` to
      `catthehacker/ubuntu:full-latest` / `full-24.04`. Add
      `--container-daemon-socket -`.
- [ ] 8.2 Add `.secrets.example` documenting `CACHIX_AUTH_TOKEN`,
      `CACHIX_SIGNING_KEY`, `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN`. Add
      `.secrets` to `.gitignore`.
- [ ] 8.3 Add Justfile recipes: `act-cache-warm`, `act-container-publish`,
      `act-nur-build`. Each invokes `act` with
      `--privileged --container-options="--userns=host" --secret-file .secrets`
      and a `-W .github/workflows/<file>` pin.
- [ ] 8.4 Verify each recipe on a Linux dev box. Document known-limitation
      "cache steps cold-start under act" in `docs/guides/ci-publishing.md`.

## 9. Docs (P2 — depends on §1, §2, §3, §5, §6, §8)

- [ ] 9.1 Add `docs/guides/ci-publishing.md`. Sections:
  1. Required repository secrets (from §0).
  2. Cache — how Cachix + GHA-cache stack works, what `pushFilter` excludes and
     why.
  3. NUR — how to install `xi` from NUR, how our aggregator wiring works, how to
     bump the NUR entry.
  4. Container — pull commands (`docker pull dauliac/xi`), supported tags, arch
     matrix, gotchas (arch mislabel).
  5. Local debugging — `just act-<workflow>` recipes, secrets file, known
     limitations under act.
- [ ] 9.2 Add a small README snippet: cachix badge, NUR install one-liner,
      docker pull one-liner. Link to `docs/guides/ci-publishing.md`.

## 10. Verification (P1 — after §1, §2, §3, §5, §6)

- [ ] 10.1 Cache — S1 evidence (double-push wall-time comparison in a scratch
      PR).
- [ ] 10.2 Cache — S2 evidence (`nix path-info` against `dauliac.cachix.org`
      from a clean host).
- [ ] 10.3 Cache — S3 evidence (fork PR passes green with no attempted push).
- [ ] 10.4 Container — S4 (`skopeo inspect` reports correct arch + labels) + S5
      (`docker run --rm dauliac/xi --version` after merge).
- [ ] 10.5 NUR — S6 (`nix-instantiate` under restricted eval) + S7 (NUR install
      from clean host).
- [ ] 10.6 Existing workflows — S9 (build/check/test still pass on PRs; no new
      required check on fork PRs).

## Dependency graph (informal — mirrored in beads via `bd link`)

- 5.x → 6.x → 7.x
- 2.x → 3.x → 4.x
- 1.x ─┐
- 3.x ─┼→ 10.x
- 6.x ─┘
- 1.x, 2.x, 5.x, 8.x are independent lanes — parallelizable from day one.
