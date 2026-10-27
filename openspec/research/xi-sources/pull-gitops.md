# Pull-mode GitOps daemons for Nix - prior art for `xi`

Scope: tools that run on the target machine and periodically pull a git repo
containing a Nix config, then rebuild. Primary targets of this research: `comin`
(the obvious prior art), plus push-based tools (`morph`, `krops`, `deploy-rs`,
`colmena`) scanned for reusable sub-features.

Local clones (ghq canonical paths):

- `/home/juliendauliac/ghq/github.com/nlewo/comin`
- `/home/juliendauliac/ghq/github.com/serokell/deploy-rs`
- `/home/juliendauliac/ghq/github.com/zhaofengli/colmena`
- `/home/juliendauliac/ghq/github.com/DBCDK/morph`
- `/home/juliendauliac/ghq/github.com/NixOS/nixops`
- `/home/juliendauliac/ghq/github.com/krebs/krops`

Clones attempted and NOT found / irrelevant:

- `nixos-remote-gitops` / `nixops-git`: no such repo on GitHub; the closest hit
  is `nix-community/nixos-anywhere`, which is a one-shot installer, not a
  recurring puller. Skipped.
- `flake-pull.nix` / `nixos-auto-update`: no such named project; the equivalent
  functionality in nixpkgs is the `system.autoUpgrade` NixOS module
  (`nixos/modules/tasks/auto-upgrade.nix` in nixpkgs). It is a systemd timer
  wrapping `nixos-rebuild switch --upgrade`; it does not clone a repo.
- `deploy-rs` pull mode: none. `deploy-rs/README.md:1` describes it as a "simple
  multi-profile Nix-flake deploy tool" that `nix copy`s a built closure to
  targets over SSH. No daemon, no on-target poller.
- `colmena` git-fetching mode: none. `colmena/README.md:97` only mentions git in
  the context of warning users to commit before deploying from the flake.
- `fluxcd + nixosConfigurations`: no official integration; prior art is
  individual blog posts, not a reusable tool. Skipped.

## Convergent patterns

1. **Build, then switch via `switch-to-configuration`.** Every tool that
   actually activates a config shells out to
   `<outPath>/bin/switch-to-configuration <op>` (comin:
   `internal/executor/utils.go:252`; nixpkgs `nixos-rebuild.sh` as cited there).
   The darwin equivalent is `<outPath>/activate-user` + `<outPath>/activate`
   (comin: `internal/executor/utils.go:268-283`).
2. **Operations are `switch` / `boot` / `test`.** Comin picks between these per
   branch (test branch = `test`, main = `switch`/`boot`); nixos-rebuild and
   morph expose the same verbs.
3. **Separate "fetch" from "build" from "deploy" as discrete stages.** Comin's
   `internal/{fetcher,builder,deployer,manager}` directory layout is the
   canonical breakdown; morph's `build`/`push`/`deploy` subcommands reflect the
   same split.

## Divergence points

- **Who holds the clone?** Pull daemons (comin) keep a persistent clone on the
  target. Push tools (morph, colmena, deploy-rs, nixops) build from the
  operator's workstation and `nix copy` the closure - no on-target git. Krops is
  a hybrid: it `rsync`s a sources tree to the target and runs `nixos-rebuild`
  there (`pkgs/populate/default.nix`).
- **Commit-selection policy.** Comin has an opinionated "main is append-only,
  testing must sit on top of main" algorithm (`docs/design.md` lines quoted
  below). Push tools delegate the question to the operator's shell: whatever
  HEAD is checked out gets built.
- **Multi-remote fan-in on one target.** Only comin models N remotes per host as
  a first-class concept (`services.comin.remotes` list, each with its own poller
  period, `nix/module.nix:remotes` and `docs/generated-module-options.md`).
  Everyone else is 1 repo → 1 flake.
- **State directory ownership.** Comin owns `/var/lib/comin` on NixOS and
  curates `/nix/var/nix/profiles/system-profiles/comin*` (next section). Push
  tools have no persistent on-target state beyond the standard
  `/nix/var/nix/profiles/system` link.

## Steal list for `xi`

- **Scheduler on top of gocron-style singleton jobs.** Comin's
  `internal/scheduler/scheduler.go:26-45` runs one job per remote with
  `gocron.LimitModeReschedule` so a slow fetch cannot stack. Rust equivalent:
  `tokio::time::interval` + a `Mutex<bool>` or `tokio::sync::Semaphore(1)`.
- **Named profile under `/nix/var/nix/profiles/system-profiles/<name>`.** Comin
  uses `comin` (`internal/profile/profile.go:17-19`). Lets `nix-collect-garbage`
  and the bootloader see xi's deployments as first-class generations without
  colliding with manual `nixos-rebuild`.
- **Preserve the booted profile even if state is lost.** Comin reads the
  currently-booted store path from `/run/booted-system` and always keeps it in
  the bootloader even when `/var/lib/comin/store.json` is empty
  (`internal/profile/profile.go:30-45` - the "booted storepath is preserved"
  comment). This prevents the "ran gc, machine won't boot" footgun.
- **Testing branch convention.** `testing-<hostname>` deployed with
  `switch-to-configuration test` (no bootloader update) is a cheap way to
  iterate without touching generations (`docs/howtos.md` "How to test a NixOS
  configuration change").
- **Dual signing (GPG + SSH allowed_signers).** Comin accepts both GPG
  (`internal/repository/git.go:227`) and SSH signatures with namespace=git
  (`internal/repository/git.go:242-305`). SSH signing lets users reuse their
  deploy key for commit trust.
- **Only verify the tip.** "Note only the last commit needs to be signed"
  (`docs/howtos.md`). Simple to implement, matches git's own protection model.
- **Multi-remote fan-in for SPOF avoidance.** List of remotes with per-remote
  poll periods (`internal/types/types.go:15-19`,
  `services.comin.remotes.*.poller.period`). A GitHub outage shouldn't stop
  deploys if a GitLab mirror exists.
- **`machineId` guard.** Comin refuses to deploy a generation whose embedded
  machine-id doesn't match the host (`internal/manager/manager.go:208-209`),
  preventing cross-host deploys when a flake is edited sloppily.
- **Switch inhibitors + post-deployment hook.** Listed in `docs/features.md`:
  user scripts after build and after deploy, user confirmation gates. These are
  cheap to add and make the daemon usable on laptops.
- **Local-remote fast path.** Add `file://` or a local path as a remote with 1s
  poll to iterate (`docs/howtos.md` "Iterate faster with local repository").
  Trivial to support, huge dev-loop win.
- **Prometheus exporter.** `services.comin.exporter` with `openFirewall`
  toggle - the operational table-stakes everyone forgets.
- **Krops-style sentinel file.** `${target.path}/.populate` guards against
  rsync-ing on top of an unrelated directory
  (`krops/pkgs/populate/default.nix:5-20`). xi's clone dir should carry an
  equivalent `.xi-managed` sentinel so a misconfigured path never clobbers user
  data.

## Per-tool matrix

### comin (Go) - `/home/juliendauliac/ghq/github.com/nlewo/comin`

1. **Watch**: gocron singleton timer, per-remote `poller.period` seconds,
   default 60s (`readme.md` quick-start; `internal/scheduler/scheduler.go:30`).
   No webhook, no inotify.
2. **Lock resolution**: comin never touches the user's working tree; it keeps
   its own bare-ish clone under `${StateDir}/repository`
   (`internal/config/config.go:90`). Commit selection refuses force-pushes to
   `main` branches and requires `testing` to sit on top of `main`
   (`docs/design.md`, "The comin commit selection algorithm" steps 1-4). The
   user's requirement - "local non-pushed version is OK" - maps to running xi
   against a `file://` local remote; comin models exactly this (`docs/howtos.md`
   "Iterate faster with local repository").
3. **State dir**: `/var/lib/comin` (`internal/config/config_test.go:14`).
   Contents: `store.json`, `gcroots/`, `state.json`, `grpc.sock`, `repository/`
   (`cmd/run.go:77-78`, `internal/config/config.go:63-90`).
4. **Rebuild invocation**: direct exec of
   `<outPath>/bin/switch-to-configuration <op>` on Linux
   (`internal/executor/utils.go:251-263`) and `activate-user` + `activate` on
   darwin (`internal/executor/utils.go:268-291`). Does NOT shell out to
   `nixos-rebuild`; builds the system attribute itself.
5. **Signing**: GPG via `gpgPublicKeyPaths`, SSH via `sshAllowedSignersPath`
   (`docs/howtos.md` "Check Git commit signatures"). Only tip must be signed.
   Implementation: `internal/repository/git.go:227` (GPG), `:242-305` (SSH,
   namespace must be `git`).
6. **Rollback**: creates a system profile at
   `/nix/var/nix/profiles/system-profiles/comin`
   (`internal/profile/profile.go:17-19`). On evaluation / build failure, the
   previous generation stays booted; a user
   `nixos-rebuild
   --rollback`-equivalent is
   `/nix/var/nix/profiles/system-profiles/comin-N-link`. Does NOT auto-rollback
   on activation failure - listed as unchecked in `docs/features.md`. Local
   clone is not reverted on failure (fetching and deploying are separate stages
   via `broker`).
7. **Multi-host**: one flake with `nixosConfigurations.<hostname>` or
   `darwinConfigurations.<hostname>`; hostname-based attr lookup
   (`docs/generated-module-options.md` `services.comin.hostname`). Optional
   `machineId` cross-check (`internal/manager/manager.go:208`).

### krops (Nix + shell) - `/home/juliendauliac/ghq/github.com/krebs/krops`

Push-based from the operator machine. Composes a "source" (git refs, local
files, pass secrets) with `evalSource`, then `rsync`s to the target and runs
`nixos-rebuild switch` there (`pkgs/populate/default.nix`, `README.md:1-50`).
Relevant stealable: the `.populate` sentinel-file guard and the pluggable-source
model (git ref + local file + pass entry = one tree).

### morph (Go) - `/home/juliendauliac/ghq/github.com/DBCDK/morph`

Operator-side CLI; subcommands `build` / `push` / `deploy` / `check-health`
(`README.md:1-55`). "No state" is called out as a notable feature. Uses
`nix-build` + `nix copy` + remote `switch-to-configuration`. Relevant stealable:
health-check gating between push and switch, which xi could add as a post-build
hook that must pass before activating.

### deploy-rs (Rust) - `/home/juliendauliac/ghq/github.com/serokell/deploy-rs`

Operator-side, SSH push, auto-rollback via a magic-number timeout: the target
confirms it is reachable within N seconds or the previous profile is restored.
Not a daemon. Relevant stealable: the magic-number confirm-or-rollback dance for
xi's `switch` path when the daemon runs on a remote laptop.

### colmena (Rust) - `/home/juliendauliac/ghq/github.com/zhaofengli/colmena`

Operator-side push with tag-based host selection. No pull / fetch mode;
`README.md:97` only mentions git as "remember to commit". Relevant stealable:
the `@tag` host-selection syntax if xi grows multi-host.

### nixops (Python) - `/home/juliendauliac/ghq/github.com/NixOS/nixops`

Classic operator-side with a stateful SQLite deployment DB. Not relevant to pull
mode; included for reference.

### `system.autoUpgrade` (nixpkgs)

Systemd timer + `nixos-rebuild switch --upgrade` (or `--flake <url>` on recent
nixpkgs). Options: `dates`, `flake`, `allowReboot`, `rebootWindow`,
`persistent`. Reference: nixpkgs `nixos/modules/tasks/auto-upgrade.nix`.
Relevant stealable: `rebootWindow` (start/end wall-clock gate) so xi doesn't
reboot a laptop at 3pm.

## Open questions (need user decision)

- **Signed-commits-only, and which trust model?** Comin supports both GPG and
  SSH `allowed_signers`. Does xi commit to one, both, or neither for v1?
- **Branch model: hostname-branches vs single-branch + attr lookup.** Comin does
  both (hostname is just the flake attr; `testing-<hostname>` is a separate
  branch). Does xi pick an opinion or expose both?
- **Local-non-pushed semantics.** User said "local non-pushed is OK". Does that
  mean (a) xi treats the local checkout as authoritative when it is ahead of
  origin, (b) xi only pulls when working tree is clean, or (c) xi always rebases
  local-only commits on top of remote? Comin dodges this by only ever reading
  its own clone - the user's checkout is never touched.
- **Auto-rollback on activation failure.** Comin doesn't do it; deploy-rs's
  magic-number confirm is the obvious model. Scope-in or defer?
- **Daemon vs oneshot timer.** Comin runs a long-lived process (gRPC socket,
  Prometheus exporter, broker). A systemd `OnCalendar` timer with
  `xi switch --from-remote` is cheaper. Does xi want the full daemon surface
  area (desktop notifications, inhibitors) or just the timer?
