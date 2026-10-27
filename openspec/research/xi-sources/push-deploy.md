# Push-mode Nix deploy tools: pattern survey for `xi push`

Scope: operator-workstation-to-remote-host push. For each tool: transport, who
evaluates, inventory schema, activation + rollback, pre-flight, secrets,
parallelism, bootstrap. Clones under `~/ghq/`.

---

## TL;DR for xi

### Convergent patterns (everyone does this)

1. **Activation goal is a `nixos-rebuild`-style verb**: every tool exposes
   `switch | boot | test | dry-activate`. deploy-rs, colmena
   (`Goal::{Switch,Boot,DryActivate}`), morph, nixinate, krops all mirror these
   verbs 1:1.
2. **Secrets are copied out-of-band via SSH, never through the Nix store**.
   deploy-rs `profiles.*.profile.path` + separate upload, morph
   `deployment.secrets`, colmena `deployment.keys`, krops
   `pass`/`passage`/`file` source types. Permissions + owner + optional
   post-upload action (restart unit) is the common shape.
3. **Target description is a NixOS module option tree merged into the host
   config**: `deployment.targetHost / targetUser / targetPort / tags` is the
   de-facto schema (morph `data/options.nix`, colmena `hive/options.nix`, krops
   `mkTarget`). Only deploy-rs lives outside the host config (top-level
   `deploy.nodes.<name>` in the flake).

### Divergence points

- **Where eval happens**: deploy-rs evaluates + builds locally and copies the
  closure (`src/push.rs:120 build_profile_locally`). morph same. krops is the
  odd one - rsyncs sources to the target and runs
  `nixos-rebuild switch -I /var/src` _on the target_ (`README.md:60`). colmena
  is tunable per-node via `deployment.buildOnTarget` (default off;
  `src/nix/hive/options.nix:161`). nixinate flips both ways via
  `_module.args.nixinate.buildOn = "remote"|"local"`.
- **Magic rollback**: deploy-rs is the only tool with the
  canary-file-plus-timeout dance. `src/bin/activate.rs:276 danger_zone` sets a
  sentinel in `/tmp`, SSHes back after activation to delete it; if the deletion
  doesn't happen before `confirmTimeout`, the remote side auto-reverts the
  profile. No other push tool implements this. Morph only has post-activation
  health-checks; failure doesn't roll back, it alerts.
- **Inventory**: declarative module-tree (colmena/morph) vs flake top-level attr
  (`deploy.nodes`, deploy-rs) vs ad-hoc nix function call (krops, nixinate
  per-config `_module.args`). nixops4 is moving towards opentofu-style
  **resource graph** (`rust/nixops4-core/src/eval_api.rs`), a different model
  entirely.
- **Bootstrap vs steady-state**: only nixos-anywhere handles fresh machines
  (kexec → disko → install → reboot, as explicit phases in
  `src/nixos-anywhere.sh:27-31`). Everything else assumes a running NixOS
  already answering SSH.

### Steal list for `xi push` (tagged by source)

| Feature                                                                                         | Source                                                               | Notes                                                                                                                                 |
| ----------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| Magic rollback (canary SSH-back within `confirmTimeout`)                                        | deploy-rs                                                            | xi-deploy `builtin.rs:229` hardcodes `magic_rollback: false`. Fill this gap - it's the single most-cited reason users pick deploy-rs. |
| `Goal` enum: `switch                                                                            | boot                                                                 | test                                                                                                                                  |
| Per-node `buildOnTarget` toggle                                                                 | colmena `hive/options.nix:161`                                       | xi should let a slow laptop push the derivation only and have a beefy server self-build.                                              |
| Separate concurrency semaphores for **eval** vs **apply**                                       | colmena `deployment/limits.rs:13`                                    | Default: eval=1, apply=10. Eval is RAM-heavy, apply is network-heavy - one knob fits both badly.                                      |
| Memory-aware eval chunking                                                                      | colmena `limits.rs:60 EvaluationNodeLimit::Heuristic`                | 512 MB/host heuristic prevents OOM on large fleets.                                                                                   |
| `deployment.secrets` with `uploadAt = pre-activation \| post-activation` + post-upload `action` | morph `data/options.nix:71`                                          | Lets a secret restart its consumer unit atomically with the switch. Colmena has the same (`options.nix:88`).                          |
| Pre-deploy healthchecks (HTTP + cmd) + post-deploy healthchecks                                 | morph `data/options.nix:257,249`                                     | HTTP probe + command probe + period/timeout. Both pre- and post-activation.                                                           |
| `fastConnection` + `substituteOnDestination`                                                    | deploy-rs `interface.json:34`, morph `data/options.nix:215`          | Toggle `nix copy --substitute-on-destination` so target pulls from its own binary cache instead of streaming through operator pipe.   |
| `interactiveSudo` toggle                                                                        | deploy-rs `interface.json:52`                                        | Allocates a TTY for sudo password prompts - otherwise push to no-root hosts silently hangs.                                           |
| `tags` for group selection; CLI `--on <tag>` filter                                             | morph, colmena                                                       | xi-deploy already has `--on` - plumb tag resolution through flake schema.                                                             |
| First-boot bootstrap via `kexec → disko → install → reboot` phases                              | nixos-anywhere `src/nixos-anywhere.sh:27`; disko                     | Not for `xi push` v1, but reserve `xi push --install` namespace for a follow-up that composes with disko.                             |
| Facts gathering before install (SSH into target, detect arch/initrd/kernel)                     | nixos-anywhere `src/get-facts.sh`                                    | A pre-flight probe xi could run for _any_ push to catch arch mismatch before copying a 2 GB closure.                                  |
| Hermetic activation (`nix-store --realise nixos-rebuild` on remote before invoking it)          | nixinate `flake.nix:57`                                              | Avoids relying on `nixos-rebuild` already-installed on target with matching nix version.                                              |
| `flock` on remote during activation to prevent concurrent pushes to same host                   | nixinate `flake.nix:58` (`flock -w 60 /dev/shm/nixinate-${machine}`) | Mandatory for a fleet tool where two operators or two `xi push` loops might race.                                                     |
| `--phases` explicit list; `--disko-mode disko \| mount \| format`                               | nixos-anywhere `src/nixos-anywhere.sh:188`                           | Composable phase model > monolithic "deploy" command.                                                                                 |

### What xi-deploy `builtin.rs` lacks today (gap list)

From
`/home/juliendauliac/ghq/github.com/Dauliac/xi/crates/xi-deploy/src/backend/builtin.rs`:

1. `magic_rollback: false` hardcoded - no sentinel, no auto-revert on partition.
2. No `Goal` abstraction - the four modes are not surfaced as first-class.
3. No secret-handling path (no equivalent of morph `secrets` / colmena `keys`).
4. No pre- or post-deploy healthcheck hook.
5. No concurrency control: one host at a time.
6. No `buildOnTarget` toggle.
7. No `tags` resolution - `--on` is string match only.
8. No `fastConnection` / `substituteOnDestination` flag.
9. No interactive-sudo TTY path.
10. No bootstrap mode (disko + nixos-anywhere composition).

---

## Per-tool extraction

### deploy-rs - `~/ghq/github.com/serokell/deploy-rs`

- **Transport**: SSH + optional sudo. Rust (tokio). ssh args as `sshOpts: [str]`
  in flake schema (`interface.json:15`).
- **Eval**: local; `build_profile_locally` (`src/push.rs:120`) runs `nix build`
  locally then `nix copy` to target. There is no remote-eval path.
- **Inventory**: top-level flake output
  `deploy.nodes.<name>.{hostname,profiles.*,sshUser,sshOpts,fastConnection,autoRollback,magicRollback,confirmTimeout,activationTimeout,tempPath,interactiveSudo}`
  (`interface.json`).
- **Activation**: `${closure}/activate-rs activate` run under sudo on the target
  (`src/deploy.rs:32 build_activate_command`). Rollback modes:
  - `autoRollback`: non-zero exit → roll profile back.
  - `magicRollback`: operator SSHes back within `confirmTimeout` and deletes a
    sentinel file; if timeout fires on target, target rolls itself back. See
    `src/bin/activate.rs:276 danger_zone` + `src/deploy.rs:66`.
- **Pre-flight**: minimal; relies on `nix copy` failing fast. No disk/sudo
  probe.
- **Secrets**: `profiles.*.profile.path`-style; secrets go via the profile
  closure. No out-of-band secret stanza - community expectation is sops-nix /
  agenix _baked into the closure_. (This is a known deploy-rs weakness.)
- **Parallelism**: nodes deployed sequentially in default CLI loop; concurrency
  is per-profile within a node.
- **Bootstrap**: no.

### colmena - `~/ghq/github.com/zhaofengli/colmena`

- **Transport**: SSH, Rust. `src/nix/host/ssh.rs`.
- **Eval**: local by default; per-node `buildOnTarget`
  (`src/nix/hive/options.nix:161`) ships the derivation and builds remotely.
  Chunked eval for RAM control (`src/nix/deployment/limits.rs:60`).
- **Inventory**: Hive, which is either `colmena = { ... }` flake output or a
  `hive.nix` file. Schema at `src/nix/hive/options.nix`:
  `deployment.{targetHost,targetPort,targetUser,allowLocalDeployment,buildOnTarget,tags,keys,replaceUnknownProfiles}`.
- **Activation**: `Goal` enum (`src/nix/deployment/goal.rs:14`):
  `Switch | Boot | Test | DryActivate | Push | Build`. No magic rollback.
- **Pre-flight**: SSH reachability only.
- **Secrets**:
  `deployment.keys.<name> = { text|keyCommand|keyFile, destDir, user, group, permissions, uploadAt = pre-activation|post-activation }`.
  Upload script template at `src/nix/host/key_uploader.template.sh`, fed via SSH
  stdin (`src/nix/host/key_uploader.rs:42 feed_uploader`). Atomic rename on
  target.
- **Parallelism**:
  `ParallelismLimit { evaluation: Semaphore, apply: Semaphore }`
  (`src/nix/deployment/limits.rs:13`). Defaults: eval=1, apply=10.
- **Bootstrap**: no.

### morph - `~/ghq/github.com/DBCDK/morph`

- **Transport**: SSH, Go. `ssh/ssh.go`.
- **Eval**: local always. `data/eval-machines.nix`.
- **Inventory**: `network.nix` listing hosts; per-host
  `deployment.{targetHost,targetPort,targetUser,buildOnly,substituteOnDestination,secrets,healthChecks,preDeployChecks,tags}`
  (`data/options.nix`).
- **Activation**: `morph deploy <net> <switch-action>` where switch-action ∈
  `{dry-activate,test,switch,boot}` (`morph.go:24,370`). Optional `--reboot`
  between switch and healthcheck. No magic rollback.
- **Pre-flight**: `preDeployChecks` (HTTP + cmd) run before activation
  (`morph.go:412`).
- **Secrets**: rich schema (`data/options.nix:29 keyOptionsType`) - `source`,
  `destination`, `owner.{user,group}`, `permissions`, `action` (command to run
  after upload, e.g. `systemctl reload nginx`),
  `uploadAt = pre-activation|post-activation`, `mkDirs`.
- **Parallelism**: `--limit n` (`morph.go:109`) is max selected hosts, not
  concurrency. Serial deploys in practice.
- **Bootstrap**: no.

### krops - `~/ghq/cgit.krebsco.de/krops`

- **Transport**: SSH + rsync. Pure Nix + shell (`writeDeploy` returns a shell
  script).
- **Eval**: **remote** by default. Rsync source tree to `/var/src` on target,
  then run `nixos-rebuild switch -I /var/src` there. Optional `buildTarget` as a
  third machine. `fast = true` skips the separate `nix build` step.
- **Inventory**: ad-hoc - one `writeDeploy` call per target. `target` is
  `[[USER]@]HOST[:PORT][/SOME/PATH]` or an attrset with
  `{host,user,port,path,sudo,extraOptions}` (`README.md:70`).
- **Activation**: `nixos-rebuild <operation>` on target; operation configurable,
  defaults `switch`.
- **Pre-flight**: none built in.
- **Secrets**: first-class source types `pass` (passwordstore), `passage`
  (age-based), `pipe` (local command stdout) - decrypted/computed locally then
  rsynced into the chrooted source tree (`README.md:288,302,325`). Not
  Nix-store-visible.
- **Parallelism**: no - one `writeDeploy` script per target, operator
  parallelizes with shell.
- **Bootstrap**: no.

### nixos-anywhere - `~/ghq/github.com/nix-community/nixos-anywhere`

- **Transport**: SSH. Shell (`src/nixos-anywhere.sh`, 1072 lines).
- **Eval**: local; closures copied to target.
- **Inventory**: a flake with `nixosConfigurations.<name>` and (optionally)
  `diskoConfigurations.<name>`. CLI accepts `--flake .#name` or
  `--store-paths <disko-script> <nixos-system>`.
- **Activation**: `nixos-install` on target after disk setup. Steady-state
  updates are out of scope - hand off to deploy-rs / colmena / nixos-rebuild.
- **Pre-flight**: `src/get-facts.sh` SSHes in, detects arch, kernel, whether
  already in installer, etc.
- **Secrets**: `--extra-files <path>` (copied into install root),
  `--disk-encryption-keys <remote_path> <local_path>`
  (`src/nixos-anywhere.sh:159,165`). Keys never land in the Nix store.
- **Phases** (`src/nixos-anywhere.sh:27`): `kexec | disko | install | reboot`,
  selectable via `--phases a,b,c`. `--disko-mode disko|mount|format`.
- **Hardware config generator**:
  `--generate-hardware-config nixos-facter|nixos-generate-config <path>` writes
  a `hardware-configuration.nix` back to the operator's workstation.
- **Parallelism**: single-host (bootstrap tool).
- **Bootstrap**: YES - this is the leader. Pair with disko
  (`~/ghq/github.com/nix-community/disko`) for declarative partitioning.

### bento - `~/ghq/github.com/rapenne-s/bento`

- **Transport**: SSH + SFTP chroot. Shell.
- **Model**: **pull, not push** (README `line 23`). Operator runs `bento deploy`
  which copies configs to a per-host SFTP chroot; client polls over SSH, pulls,
  runs `nixos-rebuild`. Reverse-engineered as "push via pull".
- **Relevance to xi**: validates that an `xi push` daemon-on-target variant is
  viable when operator and target can't open a persistent reverse connection
  (firewalls, mobile hosts). Not a direct steal, but worth considering as a
  `xi push --mode pull-via-chroot` later.
- **Secrets**: planned, not shipped at time of README.

### nixinate - `~/ghq/github.com/MatthewCroughan/nixinate`

- **Transport**: SSH, pure Nix (generates shell scripts as flake `apps`).
- **Eval**: configurable via `_module.args.nixinate.buildOn = "remote"|"local"`
  (`flake.nix:40`).
- **Inventory**:
  `_module.args.nixinate = { host, sshUser, buildOn, substituteOnTarget, hermetic, nixOptions }`
  injected into each `nixosConfiguration` (`flake.nix:36`).
- **Activation**: `nixos-rebuild switch --flake path#name` either on operator
  (`--target-host user@host --use-remote-sudo`) or on target (via `nix copy` +
  `ssh`).
- **Hermetic mode** (`flake.nix:55`): `nix copy --derivation` the exact
  `nixos-rebuild` + `flock` binaries to the target, `nix-store --realise` them,
  then invoke. Avoids target having a different nix / nixos-rebuild version.
- **Remote lock**: `flock -w 60 /dev/shm/nixinate-${machine}` on every
  activation. Prevents concurrent pushes.
- **Parallelism**: no - one flake app per host.
- **Bootstrap**: no.

### fleet (`nix-community/fleet`)

- Repo not accessible (`ghq get` returned "repository exists" 128). Based on
  secondary sources it is a Rust tool with sops-nix-first secret handling and
  declarative inventory; could not confirm via source. Skipping verbatim
  extraction.

### nixops / nixops4 - `~/ghq/github.com/NixOS/nixops`, `~/ghq/github.com/nixops4/nixops4`

- **nixops (1/2)**: Python. Stateful - tracks a deployment state file. SSH push.
  Still a reference for the "resource" (not just machine) abstraction: a
  deployment can contain EC2 instances, DNS records, S3 buckets as sibling
  resources.
- **nixops4**: Rust rewrite in progress (`rust/nixops4-core`,
  `rust/nixops4-resource`). README goals: stateless + stateful deployments;
  resource schema is a stable extension point; reuse opentofu for state. Direct
  relevance to xi: the **resource runner** (`rust/nixops4-resource-runner`)
  model would let `xi push` eventually include non-host resources (DNS, firewall
  rules) in the same transaction. Deferred for v1.

### Baseline: `nix-copy-closure`

Everyone's lowest common denominator - SSHes a closure to a remote `nix-daemon`.
All tools above are "+ activation + inventory + secrets" on top of this
primitive. `xi push` MUST at minimum match this baseline.

---

## Open questions

1. **Should `xi push` imply a target-side `xi pull` first?** If the target also
   runs a gitops `xi` daemon with its own cadence, a bare push will race the
   next pull. Options: (a) pause the pull daemon via SSH before pushing, (b)
   make the push write a `.xi-push-lock` the pull daemon respects, (c) refuse to
   push when a daemon is detected and tell the operator to use the daemon's own
   queue.
2. **Magic rollback transport**: deploy-rs reuses the operator's SSH session for
   the sentinel heartbeat. If the operator's workstation sleeps mid-deploy, the
   target rolls back. Is that the right UX for `xi push` from a laptop, or do we
   want the heartbeat to come from an xi-cloud relay the user can nominate?
3. **Build-on-target vs build-locally default**: colmena defaults to local,
   nixinate exposes both. What's `xi push`'s default? Argument for remote:
   operator laptops are weak and often cross-arch. Argument for local: operator
   machine likely has a warmer nix cache + binary cache credentials.
4. **Secret model**: adopt morph/colmena's out-of-band `secrets` stanza
   (owner/perms/action/uploadAt), or presume sops-nix/agenix are in the closure?
   Deploy-rs took the latter and users complain. Recommend: ship both, with
   sops-nix as the documented default and `secrets = { ... }` as the fallback.
5. **Should bootstrap (disko + nixos-anywhere) be a `xi push --install`
   subcommand, or a separate `xi install`?** `xi push` for steady-state has very
   different safety semantics (idempotent, can roll back) than first-boot
   install (destructive, no prior state). Lean toward separate verb, but shared
   target-resolution and secret-upload code.
