# Change: xi-sources-gitops

## Why

Once `xi` has a sources model ([[xi-sources-model]]), a config surface
([[xi-sources-config]]), and a conflict policy ([[xi-sources-conflict]]), the
next verb is a background puller: a daemon that watches the configured remote on
a timer, advances the sources dir when the remote moves, and (optionally)
triggers a rebuild. The user framed it as "a gitops daemon that watch the remote
and pull it very N time".

The pull-GitOps research (`openspec/research/xi-sources/pull-gitops.md`) found
that **comin** is the only non-trivial prior art in this ecosystem. Its
implementation choices (named `/nix/var/nix/profiles/system-profiles/
<name>`
profile, singleton scheduler per remote, GPG+SSH dual signing, only the tip must
be signed, `machineId` guard, multi-remote fan-in) have been validated in
production. This change adopts the ones that fit xi's model and documents the
ones it defers.

## What Changes

- Introduce capability `xi-sources-gitops` that owns:
  - A per-sources-identity daemon (one tokio task per `SourcesId`) that polls
    the configured remote(s) on `poll_interval` (default 60s), with a singleton
    guarantee (a slow fetch cannot stack) matching comin's
    `gocron.LimitModeReschedule` behaviour (research doc, "Steal list").
  - A commit-selection algorithm that honours [[xi-sources-conflict]]: on remote
    advance, fast-forward the sources dir only if it is a descendant of the
    current `sources_head`; refuse force pushes.
  - Signed-commit verification: `ssh` `allowed_signers` AND GPG public keys,
    both optional, enabled via `require_signed = true`. Only the tip must be
    signed, matching comin's `docs/howtos.md` guidance.
  - **Multi-remote fan-in**: `.xi.toml [[sources.remote]]` accepts an array of
    remotes with per-remote poll intervals, so a GitHub outage does not stop
    deploys when a GitLab mirror exists (research doc, "Multi-remote fan-in").
  - Post-fetch hooks: `on_advance` runs after a successful fast-forward,
    `on_build` after a successful build, `on_switch` after a successful
    activation (comin's post-deployment hook pattern).
  - A **named Nix profile** at
    `/nix/var/nix/profiles/system-profiles/xi-sources` (system scope) or
    `~/.local/state/nix/profiles/xi-sources` (user scope) so generations are
    first-class and `nix-collect-garbage` can see them.
  - A **`machineId` guard**: xi refuses to activate a generation whose embedded
    `/etc/machine-id` or hostname does not match the host, unless
    `--ignore-machine-id` is passed. Steals comin's
    `internal/manager/manager.go:208-209` check.
  - A **sentinel file** `.xi-managed` in the sources dir to guard against
    misconfigured paths clobbering user data (krops's `.populate` pattern,
    research doc).
  - A **`reboot_window`** config to prevent 3pm laptop reboots when gitops
    triggers an activation requiring a reboot (nixpkgs `system.autoUpgrade`
    pattern).

## Impact

- **Affected specs**: `xi-sources-gitops` (new capability).
- **Affected code (planned)**: new crate `crates/xi-sources-daemon/`, a separate
  daemon from `xi-develop` (they share nothing but config loading conventions -
  see codebase map `openspec/research/xi-sources/xi-codebase-map.md`); new Nix
  module `modules/nixos/sources-daemon.nix` (and home-manager peer); IPC via a
  gRPC-less Unix socket at `$XDG_STATE_HOME/xi/sources/daemon.sock` (user scope)
  / `/run/xi/sources.sock` (system scope).
- **Coordination**: enforces [[xi-sources-conflict]] on every tick; writes only
  to the sources dir ([[xi-sources-model]] rule); reads config from
  [[xi-sources-config]]; cross-references [[xi-sources-cache-prestage]] when the
  daemon fetches a generation that changes substituter config (daemon must
  re-prestage before building).
- **Breaking?**: No. The daemon is opt-in via
  `xi.sources.gitops.enable =
  true` (Nix module) or `xi sources gitops enable`
  (CLI).
- **Migration**: None. Existing xi users continue without a daemon.

## Success Criteria

- **SC-G-001**: Daemon configured with `poll_interval = 60s` tolerates a 120s
  fetch without a second fetch starting mid-stream (singleton guarantee).
- **SC-G-002**: Daemon fetches a remote that has advanced by N commits, all
  descendants of `sources_head`; `xi sources status` returns `Ready` and the
  sources dir advances to the new tip.
- **SC-G-003**: Daemon fetches a remote that is NOT a descendant of
  `sources_head` (force push); the sources dir does NOT advance,
  `xi sources status` returns `Degraded, reason: remote-force-push`, and the
  daemon logs the rejected sha.
- **SC-G-004**: With `require_signed = true` and `allowed_signers` set, the
  daemon refuses to advance past a commit whose SSH signature does not verify
  against the file; only the tip is checked.
- **SC-G-005**: Multi-remote config with two remotes polls each on its own
  interval; failure of one remote does not stop the other.
- **SC-G-006**: After a successful activation, the named profile
  `.../system-profiles/xi-sources` contains a generation link; the booted system
  profile is unchanged until the user runs `xi {nixos,darwin,home} switch` or
  passes `on_advance = "switch"`.
- **SC-G-007**: `machineId` mismatch refuses activation; the daemon logs the
  host id and the generation id for operator review.
- **SC-G-008**: A `.xi-managed` sentinel file is required in the sources dir
  before any write; absence aborts the tick with a clear error.
- **SC-G-009**: `reboot_window = "02:00-04:00"` prevents activations that
  require a reboot outside the window; the daemon re-queues the activation for
  the next window.
