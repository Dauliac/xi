# Change: xi-vm-lifecycle

## Why

The user already hand-rolls NixOS dev-space VMs in a separate dotfiles repo:
per-host `nixosConfigurations.<name>` with `virtualisation.vmVariant` overrides,
a flake app per host for graphical launch, a second flake app per host for SSH
connect, and a Taskfile wrapping all of it (`vm:<host>`, `vm:ssh:<host>`,
`vm:build:<host>`, `vm:clean`). It works, but it is ad hoc per host, has no
multi-VM "check" story (there's one unrelated `nixosTest` canary, not a per-VM
health check), and lives outside xi entirely.

The ask: fold this into xi as a standardized surface, run a check for one VM or
all VMs, start a VM graphically or headless, connect to a running one. "Vagrant
/ docker-compose, but for NixOS machines, inside xi."

xi already has a narrow piece of this:
`xi os build-vm [--with-bootloader|-B]
[--run|-r]` (`crates/xi-nixos`) builds
`config.system.build.vm` for the current flake's `nixosConfiguration` and
optionally execs the runner script immediately. It is one-shot (build-then-exec,
no persistent registry), single-VM (whatever `nixosConfiguration` the current
scope resolves to), and has no check or connect verb. This change does not
replace it silently; see `design.md` for how the two relate.

Two things sharpened the design while writing it:

- The dotfiles repo's own canary test already uses `pkgs.testers.runNixOSTest`
  with a Python `testScript` (`nixos.wait_for_unit(...)`), not SSH. That is the
  established idiom for "boot a VM and assert things about it" in this
  ecosystem, and this capability should use it rather than inventing a bespoke
  SSH-driven check runner.
- [numtide/nix-vm-test](https://github.com/numtide/nix-vm-test) builds on that
  exact same `testScript` convention to test non-NixOS distros (Debian, Ubuntu,
  Fedora, Rocky, Arch) under QEMU. Because `xi.vm.<name>.checks` is already
  testScript fragments, the same manifest shape and the same `checks` field
  extend to non-NixOS targets almost for free, by adding a second backend
  `kind`.

## What Changes

- Introduce capability `xi-vm-lifecycle`, a top-level `xi vm` command group (not
  scope-mounted under `os`/`nixos`, see `design.md`) that owns:

  - **A declarative multi-VM manifest**: `xi.vm.<name>.*` Nix options in
    `modules/flake-parts/vm.nix` (bare `xi.*`, matching the project-scope naming
    `xi.sources` already uses at this layer, since a VM manifest has no
    installed-system activation step to mirror the way sources does with
    `programs.xi.sources`). A `kind` field (`"nixos"` default, or `"distro"`)
    picks the backend:
    - `kind = "nixos"`: backing flake attribute (default
      `nixosConfigurations.<name>`, overridable via `flakeRef`/`attr`),
      resources (`cores`, `memorySize`, `diskSize`), `display` (`"graphical"`
      default | `"headless"`), `withBootloader` (bool, default `false`), app
      port forwards (`guestPort`, optional `hostPort` hint, `protocol`),
      `sshUser` (default `"xi"`, the account `vm-guest.nix` creates), and
      `checks` (testScript fragments).
    - `kind = "distro"`: `distro` (`debian`/`ubuntu`/`fedora`/`rocky`/ `arch`),
      `version` (e.g. `"13"`, `"23_04"`), `sharedDirs` (host/guest directory
      pairs, mirroring `nix-vm-test`'s own `sharedDirs`), resources (`cpus`,
      `memorySize`, `diskSize`), and `checks` (the same testScript-fragment
      field as `kind = "nixos"`).

  - **A dedicated guest-provisioning NixOS module**,
    `modules/nixos/vm-guest.nix` (also exposed as a plain `nixosModule` for
    manual composition outside flake-parts), used only for `kind = "nixos"`'s
    `--dev` variant. This is NOT something the target host's own flake imports
    or self-describes with, the flake-parts module injects it via
    `nixosConfigurations.<name>.extendModules { modules = [ ... ]; }`,
    parameterized from the manifest entry. It creates a dedicated login account
    (`sshUser`, default `"xi"`, an account it creates, not a reference to one
    the target is assumed to already have) with an xi-managed authorized key
    and, by default, passwordless sudo scoped to that one user. Creating a new
    user rather than appending to an existing one makes non-destructiveness
    structural rather than a merge-semantics promise: the only remaining risk,
    the target already declaring a user by that name, is a build-time assertion,
    not a silent merge. The `xi` user and its passwordless sudo exist only in
    the `extendModules`-produced VM build, never in that same configuration's
    real `.config.system.build.toplevel`. It does NOT bake any host port into
    the build (see the port-forwarding bullet below); it only sets up the
    account and sshd itself (`lib.mkDefault`, never `lib.mkForce`, for every
    setting).

  - **Checks are testScript fragments, not SSH assertions**: `kind = "nixos"`'s
    `--test` variant is
    `pkgs.testers.runNixOSTest { nodes.machine =
    <extended config>; testScript = <concatenated checks>; }`.
    Building that derivation boots the VM, runs the testScript, tears down, and
    fails the build on assertion failure, there is no separate
    boot-then-SSH-then-assert step to implement. `kind = "distro"`'s checks run
    the same way through
    `nix-vm-test.lib.<system>.<distro>."<version>" { testScript = ...; }`. This
    also means the `--dev`/`--test` distinction for `kind = "nixos"` is now also
    an SSH-provisioning distinction: the `xi`-user/keypair/sudo machinery only
    applies to `--dev` (what `start`/`connect` use); `--test` needs no network
    access to the guest at all.

  - **Port forwarding is resolved at runtime, never baked into the Nix build**:
    port availability is a property of the host machine at launch time, not
    something decidable at Nix-eval time. `vm-guest.nix` declares no
    `forwardPorts`. Instead, `xi vm start`'s supervisor resolves a free host
    port when it constructs the actual QEMU invocation: the SSH forward (guest
    port 22) is always dynamically resolved (probe from a base port, increment
    on collision) with no manifest-declared host port at all, since there is no
    legitimate reason to want a fixed SSH port across different VMs. A declared
    app forward's optional `hostPort` hint is tried first; if taken, `start`
    fails loudly by default (naming the conflict) unless `--allow-port-fallback`
    is passed, in which case it falls back to dynamic resolution. Every resolved
    port, for the currently running instance, is recorded in the per-VM registry
    and reported by `xi vm status`/used by `xi vm connect`.

  - **Runtime overrides in `.xi.toml`** under `[vm.<name>]`, following the same
    five-rung precedence ladder as sources: CLI flag > env var (`XI_VM_*`) >
    `.xi.toml [vm.<name>]` > Nix-module default > built-in default. Closed key
    set per `[[xi-sources-config]]`'s pattern, an unknown key under `[vm.*]` is
    a load-time error, not a silent no-op.

  - **`xi vm list`**: enumerate every declared VM with its last-known status
    (stopped / running / unknown) and last check verdict.

  - **`xi vm build [<name>|--all] [--dev|--test] [--with-bootloader]`**: build
    the VM derivation, in one of two variants (`kind = "nixos"` only;
    `kind = "distro"` always builds its `.driver`/`.driverInteractive` output,
    there is no dev/test split for it). `--dev` (default): the persistent,
    bootable image `start`/`connect` use. `--test`: the `runNixOSTest`
    derivation `check` builds and runs. `--with-bootloader` builds
    `vmWithBootLoader` instead of `vm`, matching `xi os build-vm`'s existing
    `-B` flag. One implementation, flags select variant, not separate build
    paths. This becomes the single implementation `xi os
    build-vm` delegates
    to, no duplicated build logic.

  - **`xi vm check [<name>|--all] [--json] [--build-only]`**: for
    `kind = "nixos"`, build and run the `--test` `runNixOSTest` derivation
    (build success/failure IS the verdict). `--build-only` builds only the plain
    `config.system.build.toplevel` (or `.vm`) of the extended configuration, a
    fast pre-check distinct from the full test (which cannot be built without
    running, by nixosTest's own design). For `kind = "distro"`, build and run
    the `.driver` output. One failing VM under `--all` does not abort the
    remaining checks; the command exits non-zero if any VM failed and reports
    each individually.

  - **`xi vm start <name> [--graphical|--headless] [--background]`** (or
    `--all`, `kind = "nixos"` entries only): always builds the `--dev` artifact
    first (cheap no-op via Nix's own caching when unchanged, no separate
    staleness tracking), then launches via QEMU. Display mode defaults to
    `xi.vm.<name>.display`, overridable by the flag. `kind = "distro"` entries
    run `.driverInteractive` in the foreground (QEMU window plus an interactive
    Python console, matching nix-vm-test's own model); they do not support
    `--background` and are skipped (with a note) when `--all` is used, since
    nix-vm-test's one-shot model has no persistent/reconnectable instance to
    background in the first place. `--all` for `kind = "nixos"` entries implies
    `--background` (you cannot foreground-block on more than one VM) and
    defaults to headless (to avoid popping N windows) unless `--graphical` is
    explicitly passed. `--background` detaches and registers the running
    instance (PID, control socket path, display mode, resolved ports) under the
    VM's state dir so later commands can find it.

  - **`xi vm stop <name>|--all`** (`kind = "nixos"` only): terminate a
    backgrounded instance via its control socket; errors clearly if the VM isn't
    running, wasn't started with `--background`, or is `kind =
    "distro"`
    (unsupported for that kind).

  - **`xi vm connect <name> [--console]`** (`kind = "nixos"` only): SSH by
    default, using an xi-managed throwaway keypair and the dynamically resolved
    SSH port from the running instance's registry entry (no manual key or port
    management). `--console` attaches to the QEMU serial console through the
    control socket instead. For `kind = "distro"`, `connect` is unsupported,
    `start` already opened an interactive console.

  - **`xi vm status [<name>|--all]`**: running/stopped plus the last check
    verdict and resolved ports, as a versioned JSON envelope when `--json` is
    passed.

## Capabilities

### New Capabilities

- `xi-vm-lifecycle`: declarative multi-VM dev-space management, manifest
  (`kind = nixos | distro`), build (`--dev`/`--test` variant,
  `--with-bootloader`), check (one or all, testScript-based, `--build-only`),
  start (graphical or headless, foreground or background, implicit build), stop,
  connect, status, runtime port resolution.

## Impact

- **Affected specs**: `xi-vm-lifecycle` (new capability).
- **Affected code (planned)**:
  - New `modules/flake-parts/vm.nix` for the `xi.vm.<name>.*` option tree
    (including the `kind` dimension), and `modules/shared/types/vm.nix` for the
    shared submodule type (mirroring `modules/shared/types/sources.nix`).
  - New `modules/nixos/vm-guest.nix`: the `kind = "nixos"`, `--dev`-only
    guest-provisioning module (dedicated `xi` login account, keypair, scoped
    passwordless sudo), injected via `extendModules`, also exported as a public
    `nixosModule`. No `forwardPorts` declared here.
  - New flake input on `github:numtide/nix-vm-test` for the
    `kind =
    "distro"` backend.
  - New `[vm.<name>]` section in `.xi.toml` schema
    (`crates/xi-core/src/sources/config.rs`-style closed key list, new module).
  - New crate `xi-vm` (CLI subcommands, check runner, runtime port resolution,
    control-socket registry) following the `xi-sources-daemon` split between CLI
    glue and daemon/registry logic.
  - `crates/xi-nixos/src/{args.rs,nixos.rs}`: `build-vm` becomes a thin
    deprecated alias calling into `xi-vm`'s build path (`--dev` variant, passing
    through `--with-bootloader`); emits one deprecation line pointing at
    `xi vm build --dev`.
  - `crates/xi/src/interface.rs`: new `XiCommand::Vm(xi_vm::args::VmArgs)`
    top-level arm, in the "Development" banner group alongside `develop` and
    `search`.
- **Coordination**: reuses the length-prefixed JSON-over-`AF_UNIX` control
  socket framing already implemented in `xi-sources-daemon/src/control.rs` for
  the background-VM registry, rather than inventing a second framing format.
  Does not depend on `[[xi-sources-gitops]]` or any sources capability at
  runtime, VMs are declared and run independently of a sources checkout.
- **Breaking?**: No. `xi os build-vm` keeps working, now as a deprecated alias
  with a one-time warning; no flag or output format of the existing command
  changes.
- **Migration**: None required. Operators adopting the new manifest add
  `xi.vm.<name>.*` entries at their own pace; `xi os build-vm` continues to work
  against the flake's existing `nixosConfiguration` in the meantime.

## Success Criteria

- **SC-VM-001**: `xi vm list` enumerates every `xi.vm.<name>` entry with a
  status of `stopped`, `running`, or `unknown`, and the outcome of the most
  recent `xi vm check` for that name (or `never-checked`).
- **SC-VM-002**: `xi vm build gary --dev` and `xi os build-vm` (run against a
  flake whose `nixosConfiguration` matches `gary`) produce bit-identical store
  paths; `xi vm build gary --dev --with-bootloader` and
  `xi os
  build-vm --with-bootloader` likewise match.
- **SC-VM-003**: `xi vm check gary` builds and runs `gary`'s `runNixOSTest`
  derivation; the build's own success or failure IS the verdict, reported as
  `{"schema_version":1, "status":"ok", ...}` on success, no separate SSH
  connection or custom assertion runner is involved.
- **SC-VM-004**: `xi vm check --all` with three declared VMs where one fails its
  checks still runs and reports all three; overall exit code is non-zero and the
  JSON output names which VM(s) failed and why (closed reason token, not a
  free-text error dump).
- **SC-VM-005**: `xi vm check gary --build-only` builds only `gary`'s extended
  `config.system.build.toplevel` and exits 0 without booting anything;
  `--all --build-only` does the same for every declared `kind = "nixos"` VM,
  exiting non-zero if any fails to build.
- **SC-VM-006**: `xi vm start gary --background` returns immediately once the VM
  is reachable; `xi vm status gary` then reports `running`; `xi vm stop
  gary`
  terminates it and a subsequent `xi vm status gary` reports `stopped`.
- **SC-VM-007**: `xi vm start gary` (no `--background`, default display from
  `xi.vm.gary.display = "graphical"`) opens a QEMU GTK window;
  `xi vm start
  gary --headless` does not, regardless of the manifest default.
- **SC-VM-008**: `xi vm connect gary` SSHes in using the xi-managed keypair and
  the dynamically resolved SSH port for the currently running instance, with no
  manual key or port configuration by the operator;
  `xi vm connect
  gary --console` attaches to the serial console instead.
- **SC-VM-009**: Running `xi os build-vm` emits exactly one deprecation warning
  naming `xi vm build --dev` and otherwise behaves exactly as before, including
  `--with-bootloader`.
- **SC-VM-010**: An unknown key under `[vm.gary]` in `.xi.toml` fails to load
  with an error naming the offending key, matching the closed-key-set behaviour
  of `[sources]`.
- **SC-VM-011**: Building `gary` as a VM creates a dedicated `xi` user with the
  xi-managed key and passwordless sudo, while `gary`'s own declared users and
  their SSH settings are unchanged; if `gary`'s own config already declares a
  user named `xi` (or whatever `sshUser` is overridden to), the build fails with
  an assertion naming the collision instead of merging the two user
  declarations.
- **SC-VM-012**: `gary`'s real `.config.system.build.toplevel` (built outside
  `xi vm`, e.g. via `xi os switch`) contains no `xi` user and no passwordless
  sudo rule for it, even though `gary` is declared as `xi.vm.gary`.
- **SC-VM-013**: Starting `gary --background` and `spongebob --background` at
  the same time resolves two different SSH host ports (no manifest value pins
  either), and both are independently reachable via `xi vm connect`.
- **SC-VM-014**: A declared app port forward with an explicit `hostPort` that is
  already bound by something else fails `xi vm start` with an error naming the
  conflict; the same start with `--allow-port-fallback` succeeds on a different,
  reported, port instead.
- **SC-VM-015**: `xi vm start gary` succeeds from a clean checkout with no prior
  `xi vm build gary` invocation.
- **SC-VM-016**: `xi vm start --all` with two `kind = "nixos"` VMs and one
  `kind = "distro"` VM backgrounds the two nixos VMs headless and reports the
  distro VM as skipped, rather than erroring the whole command.
- **SC-VM-017**: `xi vm build patrick --test` (where
  `xi.vm.patrick.kind =
  "distro"`, `distro = "debian"`, `version = "13"`)
  builds nix-vm-test's `.driver` output; `xi vm check patrick` runs it and
  reports pass/fail from its exit status.
- **SC-VM-018**: `xi vm stop patrick`, `xi vm connect patrick`, and
  `xi vm
  start patrick --background` (all `kind = "distro"`) each fail with a
  clear error naming the operation as unsupported for that kind, rather than
  attempting and failing confusingly.
