# xi-vm-lifecycle Specification

## Purpose

`xi-vm-lifecycle` is the capability an operator reaches for to manage named
dev/test VMs as a flat set of independent things, build one or all, check one or
all, start one graphically or headless, stop it, connect to it, the way
`docker-compose` or `vagrant` manage a set of named machines. A declared VM can
be backed by a NixOS configuration (`kind = "nixos"`) or by a plain Linux distro
image via `nix-vm-test` (`kind = "distro"`); both kinds share the same manifest
shape and the same `checks` field.

It deliberately owns no scope resolution: a declared VM is not "this machine's
config" the way `[[xi-sources-model]]`'s switch verbs are, it is an independent
named target. It also owns no sources/gitops behaviour, a VM manifest entry is
evaluated and built the same way regardless of whether the flake it lives in has
`xi.sources.*` enabled at all.

It reuses, rather than reinvents, several things that already exist: the build
logic behind `xi os build-vm` (one implementation, two entry points); the
length-prefixed JSON-over-`AF_UNIX` control-socket framing `xi-sources-daemon`
already uses for its pause/resume RPC (one framing format, two kinds of
registry); `pkgs.testers.runNixOSTest`'s `testScript` convention, the same one
the dotfiles precedent's own canary test already uses, for running checks; and
`nix-vm-test` for extending that exact same convention to non-NixOS distros.

It splits its own Nix surface for a reason: the manifest (`xi.vm.<name>`,
flake-parts) is what an operator writes, naming and describing a VM; the
guest-provisioning module (`vm-guest.nix`, NixOS, `kind = "nixos"`'s `--dev`
variant only) is what configures SSH/sudo inside the target, injected rather
than self-described, so a VM manifest entry never requires the target host's own
flake to know anything about `xi`.

### Entities

- **VM manifest entry** - one `xi.vm.<name>` declaration: `kind` (`"nixos"` |
  `"distro"`), backend-specific fields, and a `checks` list shared across both
  kinds.
- **Build variant** - `--dev` (persistent, used by `start`/`connect`,
  `kind = "nixos"` only) or `--test` (the `runNixOSTest`/`nix-vm-test`
  derivation `check` builds and runs); the same build call parameterized
  differently for `kind = "nixos"`, not two implementations.
- **Check** - one testScript fragment run against a VM boot, shared verbatim
  between `kind = "nixos"` (via `testers.runNixOSTest`) and `kind = "distro"`
  (via `nix-vm-test`). Building the check derivation IS running it; there is no
  separate check-runner process.
- **Check verdict** - the closed-enum outcome of a check build: `ok`,
  `degraded`, or `failed`, each `failed`/`degraded` verdict carrying one or more
  closed reason tokens.
- **VM instance** - a running, `--background`-started QEMU process for a named
  `kind = "nixos"` VM, discoverable via its control socket until stopped.
- **Guest provisioning** - the `vm-guest.nix` NixOS module content: a dedicated
  login account (`sshUser`, default `"xi"`) with the xi-managed key and scoped
  passwordless sudo, injected into a `--dev` build via `extendModules`, additive
  to whatever that target's own config already sets. Declares no port forwards.
- **Resolved port** - a host port chosen at `xi vm start` launch time (not at
  Nix-build time) for the SSH forward (always dynamic) or a declared app forward
  (its `hostPort` hint if free, otherwise dynamic or a loud failure), recorded
  in the instance's registry entry.
- **Control socket** - the per-instance `AF_UNIX` socket
  (`<state_dir>/vm/<name>/control.sock`) a background instance's supervisor
  listens on for `status`, `stop`, and `console-attach` requests.
  `kind = "nixos"` only.

## ADDED Requirements

### Requirement: Declarative VM manifest with a backend `kind` and closed, layered config

The system MUST provide a `xi.vm.<name>.*` Nix option tree in
`modules/flake-parts/vm.nix`. Every entry MUST declare `kind` (`"nixos"`
default, or `"distro"`) and a `checks` list (testScript fragments, shared
verbatim across both kinds).

When `kind = "nixos"`: backing flake attribute (default
`nixosConfigurations.<name>`, overridable via `flakeRef`/`attr`), `cores`,
`memorySize`, `diskSize`, `display` (`"graphical"` | `"headless"`, default
`"graphical"`), `withBootloader` (bool, default `false`), app port forwards
(`guestPort`, optional `hostPort` hint, `protocol`), and `sshUser` (the account
`vm-guest.nix` creates, default `"xi"`).

When `kind = "distro"`: `distro` (`"debian"` | `"ubuntu"` | `"fedora"` |
`"rocky"` | `"arch"`), `version` (string), `sharedDirs` (attrset of
`{ source; target; }`), `cpus`, `memorySize`, `diskSize`.

This is a flake-parts-only option tree: a VM manifest entry has no
installed-system activation step to mirror (unlike `xi.sources`'s
`programs.xi.sources` rung), so there is no corresponding NixOS or home-manager
option for the manifest itself (see the guest-provisioning requirement below for
the one NixOS module this capability does ship, which applies to
`kind = "nixos"` only).

The system MUST allow runtime overrides via `.xi.toml [vm.<name>]`, resolved
with precedence CLI flag > env var (`XI_VM_*`) > `.xi.toml [vm.<name>]` >
Nix-module default > built-in default, matching `[[xi-sources-config]]`'s
layering. The key set under `[vm.<name>]` MUST be closed: an unrecognized key is
a load-time error naming the offending key, not a silent no-op.

#### Scenario: Unknown config key rejected

- **GIVEN** a `.xi.toml` containing `[vm.gary]` with a key `displey` (typo)
- **WHEN** any `xi vm` command loads configuration
- **THEN** loading fails with an error naming `displey` as an unrecognized key
  under `[vm.gary]`

#### Scenario: CLI flag overrides manifest default

- **GIVEN** `xi.vm.gary.display = "headless"` in the Nix module
- **WHEN** `xi vm start gary --graphical` runs
- **THEN** the VM starts with a graphical display, not headless

### Requirement: Checks are testScript fragments; building the check derivation is running it

`xi.vm.<name>.checks` MUST be a list of testScript-fragment files, concatenated
into a single `testScript`. For `kind = "nixos"`, the `--test` build variant
MUST be a `pkgs.testers.runNixOSTest` derivation over the manifest's (possibly
`extendModules`-extended) configuration with that `testScript`. For
`kind = "distro"`, the build MUST be
`nix-vm-test.lib.<system>.<distro>."<version>" { ...; testScript = ...; }`. In
both cases, building the derivation MUST boot the VM, run the script, and tear
down; there MUST be no separate check-runner process that connects to the guest
over SSH or any other channel.

#### Scenario: `kind = "nixos"` check success is a build success

- **GIVEN** `xi.vm.gary.checks = [ ./checks/default.nix ]` (the built-in
  `multi-user.target` check)
- **WHEN** `xi vm check gary --json` runs
- **THEN** the `runNixOSTest` derivation builds successfully (boot, assert,
  teardown all happen inside that build), and the output is
  `{"schema_version":1,"status":"ok","reasons":[],...}` with exit code 0

#### Scenario: `kind = "distro"` check reuses the same checks field

- **GIVEN**
  `xi.vm.patrick = { kind = "distro"; distro = "debian"; version =
  "13"; checks = [ ./checks/default.nix ]; }`
- **WHEN** `xi vm check patrick` runs
- **THEN**
  `nix-vm-test.lib.<system>.debian."13" { testScript = <checks
  concatenated>; }`'s
  `.driver` output is built and run, and pass/fail comes from that build's own
  exit status

### Requirement: `xi vm build` is the single VM-build implementation, with dev/test variants and bootloader passthrough

`xi vm build [<name>|--all] [--dev|--test] [--with-bootloader]` (default
`--dev`) MUST build the named VM's (or every declared VM's) artifact. For
`kind = "nixos"`: `--dev` builds `config.system.build.vm` (or `vmWithBootLoader`
when `--with-bootloader` or `xi.vm.<name>.withBootloader` is set), the
persistent image `start`/`connect` use; `--test` builds the `runNixOSTest`
derivation per the checks requirement above. For `kind = "distro"`: there is no
dev/test split; build produces the `nix-vm-test` derivation needed by whichever
of `check`/`start` invoked it.

`xi os build-vm` MUST delegate to this same implementation (`kind =
"nixos"`,
`--dev` variant), resolving the current flake's `nixosConfiguration` name as its
target, passing through `--with-bootloader`/`-B` unchanged, and printing exactly
one deprecation line naming `xi vm build --dev <name>` before proceeding; no
other behavior of `xi os build-vm` changes.

#### Scenario: Alias produces an identical artifact

- **GIVEN** a flake whose current `nixosConfiguration` is named `gary`, also
  declared as `xi.vm.gary` (`kind = "nixos"`)
- **WHEN** `xi vm build gary --dev` and, separately, `xi os build-vm` both run
  against an unchanged flake
- **THEN** both produce the same `/nix/store` path for the VM derivation

#### Scenario: `--with-bootloader` passes through the alias

- **GIVEN** the same flake as above
- **WHEN** `xi vm build gary --dev --with-bootloader` and
  `xi os build-vm
  --with-bootloader` both run
- **THEN** both build `vmWithBootLoader` and produce the same store path

#### Scenario: `--test` produces a different artifact than `--dev`

- **GIVEN** `xi.vm.gary.display = "graphical"` with app port forwards declared
- **WHEN** `xi vm build gary --dev` and `xi vm build gary --test` both run
- **THEN** the `--dev` artifact is a bootable `config.system.build.vm`; the
  `--test` artifact is the `runNixOSTest` derivation from the checks
  requirement, a different kind of output entirely, not merely a
  headless/no-forwards variant of the same image

### Requirement: `xi vm check` builds and runs the check derivation, with a `--build-only` fast gate

`xi vm check [<name>|--all] [--json] [--build-only]` MUST build and run the VM's
check derivation (per the checks requirement above); the build's own success or
failure IS the verdict. `--build-only` MUST instead build only the extended
configuration's plain `config.system.build.toplevel` (or `.vm`), a fast,
boot-free pre-check distinct from the full test (a `runNixOSTest`/`nix-vm-test`
derivation cannot be built without running it, so `--build-only` cannot apply to
that derivation). `--build-only` applies to `kind = "nixos"` only.

Running `--all` MUST run every declared VM's checks (or build-only checks for
`kind = "nixos"` entries) and report all outcomes even if one VM fails; the
command's exit code MUST be non-zero if any VM reports `failed`.

Each verdict MUST be reported as a versioned JSON envelope
(`{"schema_version": 1, "status": "ok"|"degraded"|"failed", "reasons": [...],
"duration_ms": ...}`)
when `--json` is passed, using a closed set of reason tokens (e.g.
`build-failed`, `boot-timeout`, `check-failed:<name>`) rather than free-text
error dumps.

#### Scenario: `--all` keeps checking after one VM fails

- **GIVEN** three declared VMs where `spongebob`'s checks fail and the other two
  pass
- **WHEN** `xi vm check --all` runs
- **THEN** all three VMs are checked, the output reports each individually,
  `spongebob` carries a `check-failed:<name>` reason token, and the process exit
  code is non-zero

#### Scenario: `--build-only` validates the plain closure without booting

- **GIVEN** three declared `kind = "nixos"` VMs
- **WHEN** `xi vm check --all --build-only` runs
- **THEN** each VM's extended `config.system.build.toplevel` is built, no VM is
  ever booted, and the verdict for each reflects only that build's success or
  failure

#### Scenario: `--build-only` reports a build failure

- **GIVEN** a VM whose manifest evaluates to a broken configuration
- **WHEN** `xi vm check <name> --build-only` runs
- **THEN** the command exits non-zero with a `build-failed` reason token and no
  boot is attempted

### Requirement: `xi vm start` builds implicitly, launches graphically or headless, foreground or background

`xi vm start <name> [--graphical|--headless] [--background]` (`kind =
"nixos"`
only for `--background`) MUST first build the `--dev` artifact (reusing Nix's
own build cache, no separate staleness tracking), then launch it via QEMU.
Display mode MUST default to the manifest's `display` setting and MUST be
overridable by `--graphical`/`--headless` regardless of the manifest default.
Without `--background`, the command MUST block in the foreground for the VM's
lifetime. With `--background`, the command MUST resolve ports (per the
port-resolution requirement below), detach, and register the running instance
(PID, control socket path, display mode, resolved ports) under
`<state_dir>/vm/<name>/`, returning once the instance is up.

For `kind = "distro"`, `xi vm start <name>` MUST run the `nix-vm-test`
`.driverInteractive` output in the foreground (QEMU window plus the interactive
Python console it already provides); `--background` MUST fail with an error
naming it unsupported for `kind = "distro"`.

`xi vm start --all` MUST apply to `kind = "nixos"` entries only, implying
`--background` for each (foreground cannot block on more than one VM) and
defaulting to headless unless `--graphical` is explicitly passed;
`kind =
"distro"` entries MUST be reported as skipped rather than causing the
whole command to fail.

#### Scenario: Start succeeds with no prior build

- **GIVEN** a clean checkout with no prior `xi vm build gary` invocation
- **WHEN** `xi vm start gary` runs
- **THEN** it builds `gary`'s `--dev` artifact first, then launches it

#### Scenario: Background start returns promptly and is discoverable

- **GIVEN** `gary` is not running
- **WHEN** `xi vm start gary --background` runs
- **THEN** the command returns once the instance is up
- **AND** `xi vm status gary` subsequently reports `running`

#### Scenario: Flag overrides manifest display default

- **GIVEN** `xi.vm.gary.display = "graphical"`
- **WHEN** `xi vm start gary --headless --background` runs
- **THEN** the instance starts with no graphical display, regardless of the
  manifest default

#### Scenario: `--all` backgrounds nixos entries, skips distro entries

- **GIVEN** `gary`, `spongebob` (`kind = "nixos"`) and `patrick`
  (`kind =
  "distro"`) all declared
- **WHEN** `xi vm start --all` runs
- **THEN** `gary` and `spongebob` are built, started headless, and backgrounded;
  `patrick` is reported as skipped; the command does not fail because of the
  skip

#### Scenario: `kind = "distro"` start opens an interactive session, `--background` is rejected

- **GIVEN** `patrick` (`kind = "distro"`)
- **WHEN** `xi vm start patrick` runs
- **THEN** `nix-vm-test`'s `.driverInteractive` runs in the foreground
- **WHEN** `xi vm start patrick --background` is attempted instead
- **THEN** it fails with an error naming `--background` unsupported for
  `kind = "distro"`

### Requirement: Port forwarding is resolved at launch time, never baked into the Nix build

The SSH forward (guest port 22, `kind = "nixos"` `--dev` only) MUST be resolved
by `xi vm start`'s supervisor at launch time by probing for a free host port,
never declared in the manifest or baked into the Nix build. A declared app port
forward's optional `hostPort` hint MUST be tried first; if it is already in use,
`start` MUST fail with an error naming the conflict, unless
`--allow-port-fallback` is passed, in which case a dynamically resolved port
MUST be used instead. The currently resolved ports for a running instance MUST
be recorded in its registry entry and reported by `xi vm status`;
`xi vm connect` MUST read the SSH port from there, not from any static
configuration.

#### Scenario: Two concurrently running VMs don't collide on SSH

- **GIVEN** `gary` and `spongebob` are both `kind = "nixos"`, neither declares
  anything about an SSH port
- **WHEN** `xi vm start gary --background` and
  `xi vm start spongebob
  --background` both run
- **THEN** each resolves a different host port for its SSH forward, and both are
  independently reachable via `xi vm connect`

#### Scenario: Pinned app port conflict fails loudly by default

- **GIVEN** `xi.vm.gary`'s manifest declares an app forward with
  `hostPort = 8096`, and port 8096 is already bound by an unrelated process
- **WHEN** `xi vm start gary` runs
- **THEN** it fails with an error naming the 8096 conflict

#### Scenario: `--allow-port-fallback` resolves around a pinned conflict

- **GIVEN** the same conflict as above
- **WHEN** `xi vm start gary --allow-port-fallback` runs
- **THEN** it succeeds, using a different host port for that forward, and
  `xi vm status gary` reports the actual port used

### Requirement: `xi vm stop` terminates a backgrounded `kind = "nixos"` instance

`xi vm stop <name>|--all` MUST terminate a `--background`-started instance by
calling `stop` on its control socket. Stopping a VM with no registered instance
MUST fail with a clear error rather than silently succeeding. Stopping a
`kind = "distro"` VM MUST fail with an error naming it unsupported for that
kind.

#### Scenario: Stop a running background instance

- **GIVEN** `gary` was started with `xi vm start gary --background`
- **WHEN** `xi vm stop gary` runs
- **THEN** the instance terminates and `xi vm status gary` subsequently reports
  `stopped`

#### Scenario: Stop with nothing running errors clearly

- **GIVEN** `gary` has no registered running instance
- **WHEN** `xi vm stop gary` runs
- **THEN** the command fails with an error stating `gary` is not running, rather
  than exiting 0

#### Scenario: Stop is unsupported for `kind = "distro"`

- **GIVEN** `patrick` (`kind = "distro"`)
- **WHEN** `xi vm stop patrick` runs
- **THEN** the command fails with an error naming `stop` unsupported for
  `kind = "distro"`

### Requirement: `xi vm connect` reaches a running `kind = "nixos"` instance by SSH or console

`xi vm connect <name> [--console]` (`kind = "nixos"` only) MUST, by default, SSH
into the named running instance using an xi-managed keypair and the dynamically
resolved SSH port from its registry entry (no manual key or port setup
required). With `--console`, it MUST instead attach to the instance's QEMU
serial console via its control socket's `console-attach` request. Connecting to
a VM with no running instance MUST fail with a clear error. Connecting to a
`kind = "distro"` VM MUST fail with an error naming it unsupported for that kind
(its interactive console is already opened by `start`).

#### Scenario: Default connect is SSH with no manual key or port setup

- **GIVEN** `gary` is running via `xi vm start gary --background`
- **WHEN** `xi vm connect gary` runs
- **THEN** an SSH session opens using the xi-managed keypair and the instance's
  resolved SSH port, with no prior manual configuration

#### Scenario: Console connect

- **GIVEN** `gary` is running, started `--background`
- **WHEN** `xi vm connect gary --console` runs
- **THEN** the serial console attaches via the control socket's `console-attach`
  request

#### Scenario: Connect with nothing running errors clearly

- **GIVEN** `gary` has no registered running instance
- **WHEN** `xi vm connect gary` runs
- **THEN** the command fails with an error stating `gary` is not running

#### Scenario: Connect is unsupported for `kind = "distro"`

- **GIVEN** `patrick` (`kind = "distro"`)
- **WHEN** `xi vm connect patrick` runs
- **THEN** the command fails with an error naming `connect` unsupported for
  `kind = "distro"`, noting `start` already opens an interactive console

### Requirement: Guest provisioning creates a dedicated `xi` user, injected via `extendModules`, `--dev`-only

The system MUST provide `modules/nixos/vm-guest.nix`, a NixOS module that
creates a dedicated login account (`sshUser`, default `"xi"`) with the
xi-managed authorized key. `sshUser` names an account this module CREATES; it
MUST NOT be read as a reference to an account the target is expected to already
have. This module MUST NOT be imported by the target's own flake; the
flake-parts manifest module MUST inject it via
`nixosConfigurations.<name>.extendModules { modules = [ ... ]; }` for the
`--dev` build only (never `--test`, which needs no guest network access).

If the target's own configuration already declares a user named `sshUser`,
loading MUST fail with an assertion naming the collision and suggesting a
different `xi.vm.<name>.sshUser` override, rather than silently merging the two
user declarations.

When `xi.vmGuest.passwordlessSudo` is true (default), the module MUST grant
passwordless sudo scoped to `sshUser` only (`security.sudo.extraRules`,
additive), never a blanket `security.sudo.wheelNeedsPassword = false`. This MUST
exist only in the `extendModules`-produced VM build, never in the target's own
`.config.system.build.toplevel`, extending a configuration produces a separate
evaluation, so a VM build's `xi` user and its passwordless sudo are never
present in that same configuration's real, deployable system closure.

`modules/nixos/vm-guest.nix` MUST NOT declare any port forward (see the
port-resolution requirement above for how ports are actually assigned). Every
other setting this module contributes MUST be additive (`lib.mkDefault`), never
`lib.mkForce`: building a VM variant of an existing, real
`nixosConfigurations.<name>` MUST NOT remove or replace that target's own SSH
configuration for its own users.

`modules/nixos/vm-guest.nix` MUST also be exported as a plain public
`nixosModule`, usable by manual module composition outside flake-parts.

#### Scenario: Target's own users and SSH policy are untouched

- **GIVEN** `nixosConfigurations.gary` already declares its own
  `services.openssh.settings.PasswordAuthentication = false` and its own user
  `gary` with its own authorized key
- **WHEN** `xi vm build gary --dev` extends `gary` with the guest- provisioning
  module
- **THEN** the built VM's `sshd` config still has
  `PasswordAuthentication = false`, user `gary` and its authorized key are
  unchanged, AND a separate `xi` user exists with the xi-managed key and
  passwordless sudo

#### Scenario: `sshUser` collision is rejected, not merged

- **GIVEN** `nixosConfigurations.patrick` already declares its own user named
  `xi` (the default `sshUser`)
- **WHEN** `xi vm build patrick --dev` runs
- **THEN** the build fails with an assertion naming the `xi` user collision and
  suggesting an `xi.vm.patrick.sshUser` override, rather than merging the two
  `users.users.xi` declarations

#### Scenario: Passwordless sudo never reaches the real deployment

- **GIVEN** `nixosConfigurations.gary` is both declared as `xi.vm.gary` and used
  for real deployment via `xi os switch`
- **WHEN** `gary`'s real `.config.system.build.toplevel` is built (not via
  `xi vm build`)
- **THEN** no `xi` user and no passwordless sudo rule for it exist in that
  closure

#### Scenario: `--test` build needs no guest provisioning

- **GIVEN** `xi.vm.gary` (`kind = "nixos"`)
- **WHEN** `xi vm build gary --test` runs
- **THEN** the resulting `runNixOSTest` derivation's configuration is NOT
  extended with `vm-guest.nix`; no `xi` user, key, or sudo rule exists in it

### Requirement: `xi vm list` enumerates declared VMs with status

`xi vm list` MUST enumerate every `xi.vm.<name>` manifest entry together with
its current status (`stopped`, `running`, or `unknown` if no instance record
exists, always `unknown` for `kind = "distro"`) and the verdict of its most
recent `xi vm check` (or `never-checked`).

#### Scenario: Mixed-status listing

- **GIVEN** three declared VMs: `gary` (running), `spongebob` (stopped, last
  check `ok`), `patrick` (`kind = "distro"`, never checked)
- **WHEN** `xi vm list` runs
- **THEN** the output shows `gary: running`, `spongebob: stopped, ok`,
  `patrick: unknown, never-checked`

### Requirement: `xi vm status` reports instance, port, and check state

`xi vm status [<name>|--all]` MUST report each target's running/stopped state,
its resolved ports (for a running `kind = "nixos"` instance), and its most
recent `xi vm check` verdict, as a versioned JSON envelope when `--json` is
passed.

#### Scenario: Status reflects liveness, resolved ports, and last check

- **GIVEN** `gary` is running with a dynamically resolved SSH port, and its last
  `xi vm check` verdict was `ok`
- **WHEN** `xi vm status gary --json` runs
- **THEN** the output includes `"running": true`, the resolved SSH port, and
  `"last_check": {"status": "ok", ...}`
