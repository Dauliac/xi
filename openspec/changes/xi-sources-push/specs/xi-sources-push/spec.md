# Capability: xi-sources-push

## ADDED Requirements

### Requirement: Verb `xi push`

The capability MUST expose a top-level verb `xi push` with the following
contract:

- Positional: `<targets...>` - hostnames or deploy node names; empty = all
  discovered targets.
- `--on <filter>` - tag or regex filter against discovered targets.
- `--backend {auto|deploy-rs|colmena|builtin}` - default `auto`.
- `--phase <csv>` - subset of `{kexec,disko,install,activate}`; default
  `activate`.
- `--mode {closure|sources}` - default `closure`.
- `--magic-rollback <seconds>` - default `0` (disabled).
- `--parallel <N>` - default `1`.
- `--interactive-sudo` - default false.
- `--copy-via {local|target}` - default `local`.
- `--fail-fast` - default false; one failed target does not abort others.
- `--force-dirty` - default false.

Before dispatch, the verb MUST resolve the sources context
([[xi-sources-model]]). A `Missing` sources context exits non-zero with a
message naming `xi sources init`.

#### Scenario: Push with no sources configured

- **GIVEN** no sources configured for the resolved scope
- **WHEN** `xi push some-host` runs
- **THEN** exit non-zero with a message referencing `xi sources init`

### Requirement: Backend auto-detection

With `--backend auto` the verb MUST probe the resolved sources flake and pick
the backend as follows:

1. If `deploy.nodes.<attr>` exists for ANY requested target → `deploy-rs`
2. Else if `colmenaHive` output exists → `colmena`
3. Else → `builtin`

With an explicit `--backend` the detection is skipped.

#### Scenario: Mixed targets prefer deploy-rs

- **GIVEN** host A has `deploy.nodes.A`, host B has only `nixosConfigurations.B`
- **WHEN** `xi push A B` runs with `--backend auto`
- **THEN** both targets go through the `deploy-rs` backend; backend resolves B
  via its own `nixosConfigurations` fallback

### Requirement: `--mode sources`

With `--mode sources` the verb MUST `rsync` the sources dir to the target at
`/etc/xi/sources` (or `~/.local/share/xi/sources` for user scope) excluding
`.git/` and operator-specific state, then SSH into the target and run the
appropriate rebuild tool with `--flake /etc/xi/sources#<host>`.

The operator does NOT run a local build or `nix copy` under this mode.

#### Scenario: Target without upstream cache access

- **GIVEN** the target cannot reach `cache.nixos.org` or the operator's
  configured substituters
- **WHEN** `xi push <host> --mode sources` runs
- **THEN** no `nix copy` from the operator is attempted; the target resolves all
  dependencies via its own substituter config

### Requirement: Magic rollback

With `--magic-rollback <N>` where N > 0, the backend MUST, after each successful
activation:

1. Write a sentinel file to the target at a well-known path.
2. Begin a timer of N seconds on the operator.
3. Poll the sentinel over SSH; absence means the target rebooted or lost the ssh
   config.
4. If the sentinel is still readable at timer expiry → success, continue.
5. If the operator cannot verify the sentinel within N seconds → trigger remote
   `switch-to-configuration --rollback` via a pre-planted escape-hatch script.

#### Scenario: Target becomes unreachable within window

- **GIVEN** `--magic-rollback 60` and the target's SSH access is broken by the
  new config
- **WHEN** the activation completes but SSH stops working
- **THEN** within 60 seconds the pre-planted rollback script restores the
  previous generation

### Requirement: Secret stanza

`.xi.toml` MAY declare `[sources.secrets.<name>]` sections with:

- `source` - local path on operator
- `destination` - absolute path on target
- `owner` - user name on target; default `root`
- `group` - group name on target; default owner's primary group
- `mode` - octal string, default `0600`
- `upload_at` - `pre_activate` or `post_activate`; default `pre_activate`

Upload uses SSH out-of-band; secrets MUST NOT enter any `nix copy` closure.

When agenix or sops-nix declares a secret of the same name on the same target,
xi MUST emit a warning and prefer the module's declaration.

#### Scenario: Secret uploaded pre-activate

- **GIVEN**
  `[sources.secrets.key] source="./key", destination="/run/
  keys/key", mode="0400"`
- **WHEN** `xi push <host>` runs
- **THEN** the file lands at `/run/keys/key` with mode `0400` BEFORE activation
  starts
- **AND** the key is not visible in the target's `/nix/store`

### Requirement: Parallelism with independent eval/apply

The capability MUST support concurrent targets up to `--parallel N`. Eval and
apply MUST have independent semaphores so apply on target A can proceed while
target B still evaluates.

#### Scenario: Four parallel targets

- **GIVEN** `--parallel 4` and five targets
- **WHEN** `xi push` runs
- **THEN** at most four targets are in the activation phase concurrently; the
  fifth queues until one slot frees

### Requirement: Preflight

Before any mutating action the backend MUST check per target:

- SSH connectivity (`ssh -o BatchMode=yes -o ConnectTimeout=10 <host>
  true`)
- Free disk ≥ 2 GiB on `/nix`
- Sudo access (`sudo -n true` unless `--interactive-sudo`)
- `nixos-rebuild` or `darwin-rebuild` or `home-manager` present

A failure in any check MUST be surfaced per target with a non-zero exit if ALL
targets failed; a mix of failures and successes exits with a non-zero status
when `--fail-fast` is set, otherwise completes the successful targets and
reports failures at the end.

#### Scenario: One of three targets preflights

- **GIVEN** targets A/B/C; A has no sudo
- **WHEN** `xi push A B C` runs without `--fail-fast`
- **THEN** B and C activate; A is reported as a preflight failure; overall exit
  code is non-zero but B and C are committed

### Requirement: Dirty working dir refusal

The verb MUST honour [[xi-sources-conflict]]: a dirty working dir refuses to
push without `--force-dirty`.

#### Scenario: Dirty working dir refuses

- **GIVEN** the working dir has unstaged changes
- **WHEN** `xi push <host>` runs without `--force-dirty`
- **THEN** the push is refused before preflight; message names the dirty files
