# Capability: xi-sources-gitops

## ADDED Requirements

### Requirement: Per-sources-identity singleton daemon

The capability MUST run exactly one scheduler per `SourcesId`
([[xi-sources-model]]). Each scheduler iteration is bounded by a semaphore of 1
so a slow fetch cannot be stacked on top of a prior tick.

#### Scenario: Slow fetch does not stack

- **GIVEN** `poll_interval = 60s` and a fetch takes 120s
- **WHEN** the second tick becomes due
- **THEN** it waits for the first tick to finish, then runs; no two fetches run
  concurrently for the same `SourcesId`

### Requirement: Advance only via fast-forward

The daemon MUST refuse to advance `sources_head` to a sha that is not a
descendant of the previously recorded `sources_head`.

#### Scenario: Linear advance accepted

- **GIVEN** remote head is a descendant of `sources_head`
- **WHEN** the tick runs
- **THEN** `sources_head` advances and the sources dir is checked out to the new
  sha

#### Scenario: Non-descendant rejected

- **GIVEN** remote head is NOT a descendant of `sources_head` (force-pushed)
- **WHEN** the tick runs
- **THEN** `sources_head` does not change
- **AND** `xi sources status` returns `Degraded, reason:
  remote-force-push`
- **AND** the daemon logs the rejected sha

### Requirement: Signed-commit verification

When `require_signed = true` the daemon MUST verify the signature of the tip
commit against `allowed_signers` (SSH, namespace `git`) and/or `gpg_public_keys`
(GPG). At least one of the two files MUST be configured when
`require_signed = true`.

Only the tip must verify; intermediate commits are not inspected.

#### Scenario: Unsigned tip refused

- **GIVEN** `require_signed = true` and the remote tip has no signature
- **WHEN** the tick runs
- **THEN** `sources_head` does not advance
- **AND** `xi sources status` returns `Degraded, reason:
  signature-missing`

#### Scenario: SSH-signed tip accepted

- **GIVEN** `require_signed = true` with a valid `allowed_signers` file
  containing the author's key
- **WHEN** the tick runs with a tip signed `-S` by that key
- **THEN** `sources_head` advances

### Requirement: Multi-remote fan-in

The capability MUST support an array of remotes per `SourcesId`. Each remote
carries its own `poll_interval`. Failure of one remote (DNS error, auth error,
non-descendant tip) MUST NOT prevent other remotes from polling.

The last-advanced sha wins; two remotes advancing to the same sha within the
same tick window is idempotent.

#### Scenario: One remote outage does not stop the other

- **GIVEN** two remotes `origin-github` and `origin-gitlab` configured
- **WHEN** `origin-github` returns a network error
- **THEN** `origin-gitlab` continues polling and may advance `sources_head`
- **AND** the GitHub failure is logged with its retry count

### Requirement: Named Nix profile

Each successful build MUST register a generation on a named profile at:

- System scope: `/nix/var/nix/profiles/system-profiles/xi-sources`
- User scope: `${XDG_STATE_HOME}/nix/profiles/xi-sources`

Generations on this profile are observable to
`nix-env --list-generations
--profile <path>` and participate in
`nix-collect-garbage` normally.

#### Scenario: Generation registered after build

- **GIVEN** a successful build on a system-scope daemon
- **WHEN** the build completes
- **THEN** `/nix/var/nix/profiles/system-profiles/xi-sources` has a new
  generation link pointing to the built store path

### Requirement: machineId guard

Before any activation the daemon MUST compare the host's identifying token
against the generation's embedded token:

- System scope: `/etc/machine-id`
- User scope: hostname (`gethostname()`)

A mismatch MUST refuse activation unless `--ignore-machine-id` is passed on the
CLI. The daemon never passes this flag automatically; it is operator-only.

#### Scenario: Mismatched machine-id refuses

- **GIVEN** host `/etc/machine-id = AAAA...` and the generation embeds `BBBB...`
- **WHEN** the daemon attempts activation
- **THEN** activation is refused
- **AND** both ids are logged

### Requirement: Sentinel file

The sources dir MUST contain a sentinel file `.xi-managed` with a JSON header
containing `{ sources_id, created_by, version }`. Any write to the sources dir
MUST first verify the sentinel's presence and matching `sources_id`.

#### Scenario: Missing sentinel aborts tick

- **GIVEN** the sources dir has lost its `.xi-managed` sentinel (e.g. user
  manually deleted it)
- **WHEN** the daemon's next tick fires
- **THEN** the tick aborts with a `Degraded, reason: missing-sentinel` status
- **AND** no files in the sources dir are modified

### Requirement: Reboot window gate

When an activation would require a reboot (as reported by the underlying rebuild
tool), the daemon MUST honour the configured `reboot_window`. If the current
wall-clock time is outside the window, the activation is deferred and re-queued
for the next window start.

Setting `reboot_window = "always"` or omitting the key disables the gate.

#### Scenario: Reboot-requiring activation deferred

- **GIVEN** `reboot_window = "02:00-04:00"` and current time is 14:00
- **WHEN** the daemon decides to activate a generation requiring a reboot
- **THEN** activation is deferred until 02:00 the following day
- **AND** `xi sources status` reports the deferred activation and its wake-up
  time

### Requirement: Lifecycle hooks

The daemon MUST invoke user-configured shell commands at three points:

- `on_advance` - after a successful fast-forward of `sources_head`
- `on_build` - after a successful build
- `on_switch` - after a successful activation

Hook stdout and stderr MUST be captured to the state dir (`hooks/<name>.log`,
rotated). A non-zero hook exit MUST NOT roll back the preceding step; it is
surfaced as `Degraded` on the next status call and cleared on the next
successful cycle.

#### Scenario: Hook failure is non-fatal

- **GIVEN** `on_switch = "systemctl restart my-service"` and the service restart
  fails
- **WHEN** activation completes but the hook returns non-zero
- **THEN** the generation remains active
- **AND** `xi sources status` returns
  `Degraded, reason:
  hook-on_switch-failed`
