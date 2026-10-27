# ci-publish (delta)

## ADDED Requirements

### Requirement: Shared binary cache

CI MUST publish every store path produced by a merged commit on `master` to a
public Cachix cache (`dauliac`), and MUST pull from that cache on every event
where a build is required.

#### Scenario: Merged commit populates cache

- **WHEN** a commit is merged to `master` and the build workflow succeeds
- **THEN** every store path produced by that build is available at
  `https://dauliac.cachix.org` within the workflow's runtime

#### Scenario: Subsequent build hits cache

- **WHEN** a subsequent PR builds a closure whose paths are already in the cache
- **THEN** those paths are fetched from Cachix instead of rebuilt, and the
  workflow log records the substitution

#### Scenario: Fork PR builds without secrets

- **WHEN** a PR is opened from a fork
- **THEN** the workflow completes with no failed steps and no attempt to push to
  Cachix

### Requirement: Intra-run store reuse

CI MUST cache the Nix store between consecutive runs of the same workflow via
the GitHub Actions cache, bounded so it does not evict its own fresh entries.

#### Scenario: Second push to a branch is faster

- **WHEN** a branch is pushed twice within a week and the first push's cache
  entry is still present
- **THEN** the second push's `nix build` step completes in materially less
  wall-time than the first, using restored store paths from the GHA cache

#### Scenario: Cache stays under budget

- **WHEN** the cached store approaches 5 GB
- **THEN** the cache action garbage-collects older paths before saving, so the
  resulting cache entry fits under the configured budget

### Requirement: NUR compatibility

The repository MUST evaluate cleanly under NUR's evaluator constraints
(`restrict-eval = true`, `allow-import-from-derivation = false`) and MUST expose
`xi` as a NUR attribute.

#### Scenario: NUR evaluator ingests the repo

- **WHEN** NUR's aggregator evaluates `ci.nix` on any of its supported nixpkgs
  channels
- **THEN** evaluation completes without errors and yields a derivation for `xi`

#### Scenario: End user installs from NUR

- **WHEN** a user runs `nix-env -iA nur.repos.dauliac.xi -f <NUR archive>` on a
  clean host
- **THEN** the install succeeds by pulling from the shared Cachix cache, without
  a local rebuild

### Requirement: NUR cache pipeline

CI MUST run a NUR-shaped build (`nix-build-uncached ci.nix`) on every push to
`master`, on schedule at least daily, and MUST push signed results to the shared
Cachix cache so downstream NUR consumers pull from it.

#### Scenario: NUR-build runs nightly

- **WHEN** the nightly schedule fires
- **THEN** `nix-build-uncached ci.nix` runs and any newly-built paths are pushed
  to Cachix signed with the project's signing key

#### Scenario: Fork PR triggers NUR-build without push

- **WHEN** a PR from a fork touches files that trigger `nur-build.yaml`
- **THEN** the workflow builds `ci.nix` and skips the push step, without failing

### Requirement: OCI image publication

CI MUST build an OCI image via the flake's `packages.<system>.container` output
and publish it to Docker Hub for every push to `master` and every version tag,
with tags derived from git ref.

#### Scenario: Merge to master publishes `latest`

- **WHEN** a commit is merged to `master`
- **THEN** the container publish workflow builds `.#container` and pushes to
  `docker.io/dauliac/xi:latest` and `:sha-<7>`

#### Scenario: Version tag publishes semver tags

- **WHEN** a tag matching `v[0-9]+.[0-9]+.[0-9]+` is pushed
- **THEN** the workflow pushes `docker.io/dauliac/xi:<version>`,
  `:<major.minor>`, and `:<major>` (but not `:latest` on a beta-suffixed tag)

#### Scenario: Fork PR builds container without pushing

- **WHEN** a PR from a fork touches container-publish trigger paths
- **THEN** the workflow builds the image and skips the Docker Hub login + push
  steps

#### Scenario: Published image runs on target arch

- **WHEN** a user runs
  `docker pull dauliac/xi:latest && docker run --rm dauliac/xi --version` on
  `linux/amd64`
- **THEN** the container prints the expected version without an
  architecture-mismatch warning

### Requirement: OCI image architecture correctness

The container package MUST set the OCI manifest `architecture` field to match
the target system, not the build host, so cross-built images pull correctly on
their real target arch.

#### Scenario: Amd64 image reports amd64

- **WHEN** `skopeo inspect docker-archive:./result` runs against an amd64 build
- **THEN** the output includes `"Architecture": "amd64"`

#### Scenario: Arm64 cross build reports arm64

- **WHEN** `packages.aarch64-linux.container` is built on an amd64 runner via
  `pkgsCross.aarch64-multiplatform` and inspected
- **THEN** the output includes `"Architecture": "arm64"` — not the host's
  `amd64`

### Requirement: Local workflow debugging via `act`

Every workflow introduced by this change MUST run end-to-end under `nektos/act`
with the project's Justfile recipes and a local `.secrets` file, without
requiring changes to workflow YAML.

#### Scenario: `just act-container-publish` runs locally

- **WHEN** a developer runs `just act-container-publish` on a Linux host with
  Docker and a populated `.secrets`
- **THEN** the workflow executes to completion locally, produces the image
  tarball, and (if secrets are present) pushes to Docker Hub

#### Scenario: `just act-nur-build` runs locally

- **WHEN** a developer runs `just act-nur-build`
- **THEN** the workflow executes `nix-build-uncached ci.nix` locally under act
  and reports success

### Requirement: Fork PR safety

Any workflow that references a secret (Cachix push, Docker Hub push, NUR cache
push) MUST guard the secret-using step so PRs from forks do not fail on missing
secrets.

#### Scenario: Fork PR sees only green checks

- **WHEN** a PR from a fork triggers any workflow that has secret-using steps
- **THEN** the secret-using steps are skipped with an explicit `if:` guard, not
  silently failed, and the workflow's overall status is success

### Requirement: Existing workflows preserved

Adding this change MUST NOT break existing workflows (`build.yaml`,
`check.yaml`, `test.yaml`, `tag.yaml`, `changelog.yaml`, `nixos-search.yaml`,
`cleanup.yaml`) nor introduce a new required status check that blocks fork PRs.

#### Scenario: Existing build workflow still green

- **WHEN** a PR touches the same source paths that trigger `build.yaml`
- **THEN** `build.yaml` continues to pass with the added cache + cachix steps
  and no new required check appears in the merge gate for fork PRs
