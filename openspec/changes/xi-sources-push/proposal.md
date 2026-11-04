# Change: xi-sources-push

## Why

The user wants a symmetric push verb: "we should also be able to push the config
on a remote machine like ansible/ssh". xi already has a `crates/xi-deploy/`
crate with three pluggable backends (`deploy-rs`, `colmena`, `builtin`) per the
codebase map, so this change is NOT a greenfield push implementation. It is:

1. Teaching the sources model to be the source-of-truth that `xi push` consults
   (rather than every call re-asking "which flake?").
2. Closing the ten specific gaps the push-mode research identified in the
   `builtin` backend relative to the ecosystem
   (`openspec/research/xi-sources/push-deploy.md`, §3).
3. Picking an opinion where comparable tools disagree (local eval vs remote
   eval, magic-rollback semantics, secret model).

## What Changes

- Introduce capability `xi-sources-push` that owns:
  - A new top-level verb
    `xi push [targets...] [--on <filter>] [--backend
    <name>] [--phase <kexec|disko|install|activate>]`.
    The command resolves the sources context ([[xi-sources-model]]) before
    dispatching to `xi-deploy`.
  - A **sources-vs-closure** choice at push time:
    - `--mode closure` (default): local eval, remote receives a closure via
      `nix copy`. Matches deploy-rs, colmena, morph, nixinate.
    - `--mode sources`: the operator's sources dir is `rsync`'d to the target
      and the target runs `nixos-rebuild switch --flake .`. Matches krops.
      Required when the target cannot access the upstream substituters that the
      operator's eval depends on.
  - **Magic rollback** (deploy-rs's canary-via-sentinel pattern, research §3):
    the activated generation is probed via a well-known sentinel path from the
    operator within N seconds; failure to confirm triggers an SSH-side
    `switch-to-configuration --rollback`. Config:
    `--magic-rollback
    <seconds>`, default `0` (off) to match today's
    `builtin` backend's hardcoded behaviour until the user opts in.
  - **Secret stanza** in the sources model:
    `xi.sources.secrets.<name> =
    { source = "<local path>"; destination = "/run/..."; owner = "..."; };`
    uploaded out-of-band via SSH BEFORE activation (morph's `uploadAt` pattern,
    research §3). Compatible with `agenix` and `sops-nix`: when the module
    already declares those, xi defers to them and emits a warning if the user
    tries to double-declare.
  - **Parallelism**: `--parallel N` with a default of 1. Steal colmena's split
    eval/apply semaphores (`deployment/limits.rs:13`).
  - **Preflight checks** on each target before any mutation: SSH connectivity,
    free disk (≥ 2 GiB on `/nix`), sudo access, `nixos-rebuild` present. Fail
    fast with per-target error reporting.
  - **Interactive sudo**: when `--interactive-sudo` is passed, allocate a PTY
    (`ssh -t`) so the password prompt reaches the operator. Default stays
    passwordless (`sudo -n`).
  - **`fastConnection` / `substituteOnDestination`** parity with deploy-rs:
    `--copy-via <local|target>` picks whether the operator uploads the closure
    to the target (default, slow link) or the target substitutes from its own
    substituters (fast link to cache).
  - **Target discovery** reads the house taxonomy: `nixosConfigurations`,
    `darwinConfigurations`, `homeConfigurations`, `deploy.nodes`, `colmenaHive`.
    Multi-source: when both `deploy.nodes.<n>` and `nixosConfigurations.<n>`
    exist, prefer the explicit deploy stanza.
  - **Build location as a configurable ladder**, named `remoteBuild` /
    `remote-build` to match `deploy-rs`'s existing terminology rather than
    inventing new vocabulary (the magic-rollback feature above already
    follows that project): `--remote-build` CLI flag > `.xi.toml [push]
    remote-build = true` > `xi.sources.push.remoteBuild` Nix option default.
    `remote-build = false` (the default) builds on the pushing operator's
    machine and copies the closure in, per `--copy-via` above. `remote-build
    = true` is `--mode sources`: the target builds from its own rsync'd
    sources dir.
  - **Remote execution wrapped in a transient systemd unit**, not a bare
    foreground SSH command: `systemd-run --wait --pipe --collect xi <scope>
    switch` (or, for `--mode sources`, the equivalent `nixos-rebuild switch
    --flake` already wired in `mode_sources::switch`) run over the SSH
    session. This survives an SSH hiccup, gets journald logging on the
    target for free, propagates the real exit code, and `--collect` leaves no
    unit file behind. Scoped to the remote-build path only - a local build
    is already a normal interactive invocation and does not need wrapping.
    When the remote command is `xi <scope> switch` itself, the target
    acquires its own sources lock the same way an interactive switch would;
    no separate `ActorName::Push` acquisition is needed for that path.
  - **A declarative inventory**, ansible-style, alongside the existing ad hoc
    `xi push <targets...>` positional form: `.xi.toml [[push.hosts]]` entries
    (`name`, `ssh`, optional per-host `remote-build` override) let `xi push
    web1` resolve a name instead of retyping the SSH target every time. A
    positional that is not a known inventory name is still read as a raw SSH
    target directly, so ad hoc pushes keep working with no config at all.
    The inventory describes *other* machines, unlike every other
    `[sources.*]` key so far which describes the machine it is read on, so
    it is read from the config repo's own `.xi.toml` (the sources dir the
    operator is pushing from), not from any target's local state. Host
    groups are out of scope for the first pass; flat named hosts only.

## Impact

- **Affected specs**: `xi-sources-push` (new capability).
- **Affected code (planned)**: `crates/xi/src/push.rs` new verb entry; additions
  to each of `crates/xi-deploy/src/backend/{builtin,deploy_rs,
  colmena}.rs` to
  honour the magic-rollback and secret stanza; a new `SecretsConfig` section in
  `.xi.toml` ([[xi-sources-config]] extension).
- **Coordination**: resolves target list through [[xi-sources-model]] (knows
  what's "deployed"); honours [[xi-sources-conflict]] (push from a dirty working
  dir refused without `--force-dirty`); [[xi-sources- bootstrap]] uses
  `xi push --phase kexec,disko,install` as the "fresh machine over SSH" path.
- **Breaking?**: No. Existing `xi-deploy` is unchanged; `xi push` is a new
  top-level verb.
- **Migration**: None.

## Success Criteria

- **SC-P-001**: `xi push <host>` from a configured sources dir pushes to a
  single target using the auto-detected backend (deploy-rs if `deploy.*`
  present, colmena if `colmenaHive` present, else `builtin`).
- **SC-P-002**: `xi push --mode sources` rsyncs the sources dir to the target
  and triggers a remote `nixos-rebuild switch --flake .`; no closure is copied
  from the operator.
- **SC-P-003**: `xi push --magic-rollback 60` sets a sentinel on the target that
  the operator probes within 60s; absence triggers remote rollback.
- **SC-P-004**:
  `xi.sources.secrets.ssh-key = { source = "...";
  destination = "/run/keys/ssh"; owner = "root"; };`
  is uploaded via SSH BEFORE activation, lands at the specified path with the
  specified mode, and is NOT present on the operator's `nix copy` closure.
- **SC-P-005**: With `--parallel 4` and five targets, four activations run
  concurrently; the fifth queues. On any target's failure, the queue does not
  abort unless `--fail-fast`.
- **SC-P-006**: Preflight failures (no disk, no sudo, unreachable SSH) are
  reported per target with a non-zero exit before any activation starts.
- **SC-P-007**: `--interactive-sudo` allocates a PTY and the operator can type
  the sudo password; without the flag, sudo uses `-n` and activation fails
  cleanly on a password-required host.
- **SC-P-008**: A target declaring both `deploy.nodes.<n>` and
  `nixosConfigurations.<n>` picks the deploy stanza; a target with only
  `nixosConfigurations.<n>` falls back to the `builtin` backend without the user
  naming it explicitly.
- **SC-P-009**: Pushing from a dirty working dir without `--force-dirty` refuses
  (honours [[xi-sources-conflict]]).
