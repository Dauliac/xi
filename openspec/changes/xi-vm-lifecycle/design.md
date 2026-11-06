# Design: xi-vm-lifecycle topology, check mechanism, and port resolution

## Status

Accepted.

## Context

Several things already exist, or surfaced during design, that shape this:

1. `xi os build-vm [--with-bootloader|-B] [--run|-r]`
   (`crates/xi-nixos/src/{args.rs,nixos.rs}`) builds and optionally launches
   _the current flake's_ NixOS VM. It is scoped (lives under `os`, which maps to
   the System scope per `[[xi-sources-model]]`'s verb-to-scope resolver),
   single-VM, and one-shot: build, maybe exec, done. No registry, no status, no
   check, no connect, no port management.
2. The dotfiles precedent this change is standardizing from treats VMs as named,
   independent things (`gary`, `spongebob`), each with its own flake app for
   launch and a separate one for SSH, wrapped by Taskfile tasks. Its own `dev`
   canary test uses `pkgs.testers.runNixOSTest` with a Python `testScript`, not
   SSH.
3. [numtide/nix-vm-test](https://github.com/numtide/nix-vm-test) extends that
   same `testScript` convention to non-NixOS distros (Debian, Ubuntu, Fedora,
   Rocky, Arch), with outputs `.driver` (noninteractive, networked),
   `.driverInteractive` (QEMU window plus an interactive Python console,
   root/no-password login), and `.sandbox` (noninteractive, fully isolated).

Four questions need an explicit answer before writing the spec:

- Does `xi vm` replace, wrap, or ignore `xi os build-vm`?
- How do checks actually execute against a booted VM?
- How does a host port get chosen for SSH and app forwards without colliding
  across VMs or with unrelated processes on the host?
- Is `xi vm` scoped to NixOS only, or does it also cover non-NixOS test VMs?

## Decision Drivers

- **One build implementation.** A second code path that also builds
  `system.build.vm` would drift from `build-vm` the first time someone fixes a
  bug in only one of them.
- **No surprise breakage.** `xi os build-vm` is shipped and documented
  (`docs/guides/nixos.md`). It keeps working, per the "Breaking? No" bar every
  other change in this repo holds itself to.
- **Match the mental model the user asked for.** "Vagrant / docker-compose, but
  for NixOS VMs" is multi-item and flat, `vagrant up web`, not
  `vagrant
  os up web`. A buried-under-scope command fights that.
- **Reuse, don't reinvent, the IPC the repo already has.** `xi-sources-daemon`
  already solved "a client needs to reach a long-running local process" with a
  length-prefixed JSON-over-`AF_UNIX` socket. A VM registry has the same shape
  of problem.
- **Reuse, don't reinvent, the testing idiom the ecosystem already has.**
  `testers.runNixOSTest`'s `testScript` convention, and `nix-vm-test`'s
  extension of it to other distros, are established, debugged, and already used
  by the dotfiles precedent itself. Building a parallel SSH-driven assertion
  runner would duplicate what this already does correctly.
- **Nix evaluation cannot know what's free on the host at launch time.** Port
  availability is runtime state of the operator's machine. Baking a host port
  into a Nix build bakes in a guess that can be wrong the moment two VMs, or a
  VM and an unrelated process, want the same port.

## Considered Options

### `build-vm` overlap

1. **Leave `build-vm` alone, give `xi vm build` its own implementation.**
   Rejected, two code paths building `system.build.vm`/`vmWithBootLoader` will
   diverge the first time either gets a bug fix or a new flag.
2. **Remove `build-vm`, force migration to `xi vm build` immediately.**
   Rejected, breaking, and nothing about this change requires it.
3. **`xi vm build` becomes the one implementation; `xi os build-vm` becomes a
   thin deprecated alias**, including passing through `--with-bootloader`.
   Accepted. See Decision.

### Check execution mechanism

1. **A bespoke runner: boot `--test` headless, connect over SSH as the
   xi-managed user, run shell-command assertions, tear down manually.** This was
   the original design. Rejected on reflection: it requires SSH provisioning on
   an artifact (`--test`) that never needs network access to be checked, it
   reinvents boot/assert/teardown sequencing that `testers.runNixOSTest` already
   implements correctly, and it is incompatible with "`checks` are
   nixosTest-style assertion snippets" (already the Entities-section wording)
   taken literally, since a real nixosTest derivation cannot be built without
   running it, making a boot-free "`--build-only` the test artifact" mode
   incoherent.
2. **`kind = "nixos"`'s `--test` variant IS a `pkgs.testers.runNixOSTest`
   derivation; `checks` are testScript fragments concatenated into its
   `testScript`; building the derivation is running the check.** Accepted. See
   Decision.
3. **No NixOS module/driver tooling at all; shell out to
   `nixos-rebuild
   build-vm` plus a hand-rolled wait-for-boot loop.**
   Rejected, strictly worse than option 2 on every axis (slower to implement, no
   existing track record, doesn't compose with `nix-vm-test`).

### Port resolution for SSH and app forwards

1. **Bake a fixed host port into `virtualisation.vmVariant.forwardPorts` at
   Nix-eval time** (the dotfiles precedent's own approach: host 2222 to guest
   22, hardcoded in the flake). Rejected: a second VM, or literally anything
   else already listening on 2222, makes QEMU fail to bind at launch. The
   failure mode is "error at `xi vm start` time naming a port already in use",
   not something the Nix build can see coming, since the build has no visibility
   into what else will be running on the host at some future launch time.
2. **Resolve host ports at launch time, in xi's own `start`/`--background`
   supervisor, and inject them into the actual QEMU invocation (not the Nix
   build).** Accepted. See Decision.
3. **QEMU monitor `hostfwd_add` after boot**, dynamically adding forwards to an
   already-running instance via the QMP/monitor socket instead of at invocation
   time. Not rejected outright, noted in Decision as an implementation-level
   alternative to option 2 with the same user-visible contract; the supervisor
   can pick whichever is simpler against the nixpkgs-generated runner script
   once implementation starts.

### Backend scope: NixOS only, or NixOS + `nix-vm-test`

1. **NixOS only.** This was the original scope. Superseded: once `checks` are
   testScript fragments (decision above), the exact same field extends to
   `nix-vm-test`-backed non-NixOS targets with very little additional surface,
   and the user explicitly wants this now rather than as a follow-up.
2. **Add `kind = "nixos" | "distro"`, with full lifecycle parity between the two
   kinds (background, stop, connect for both).** Rejected: `nix-vm-test` is
   deliberately a one-shot tool (`.driver` runs a test and exits,
   `.driverInteractive` opens one interactive session), it does not expose a
   persistent, reconnectable instance the way `xi`'s own control-socket
   supervisor does for `kind = "nixos"`. Forcing parity would mean
   reimplementing a chunk of VM-process supervision that `nix-vm-test`
   deliberately leaves to its caller as a one-shot driver invocation, for a
   backend that was never designed to be backgrounded.
3. **Add `kind = "nixos" | "distro"`, with `kind = "distro"` scoped to
   `build`/`check`/`start` (foreground, `.driverInteractive`) only; no
   `--background`, `stop`, or `connect` for it.** Accepted. See Decision.

## Decision

**`xi vm` is a new top-level command group** (`XiCommand::Vm`, "Development"
banner group), independent of the `os`/`nixos`/`darwin`/`home` scope resolver.
VMs are named, declared independently of "which scope am I switching", matching
the docker-compose/vagrant mental model the user asked for.

**`xi vm build` is the one implementation**, parameterized by `--dev`/`--test`
(default `--dev`) and `--with-bootloader` for `kind = "nixos"`. `xi os build-vm`
is kept as a deprecated alias, resolving the current flake's
`nixosConfiguration` name, passing through `--with-bootloader`, and printing one
deprecation line naming `xi vm build
--dev <name>`.

**Checks are testScript fragments, run through `testers.runNixOSTest`
(`kind = "nixos"`) or `nix-vm-test` (`kind = "distro"`).** `xi.vm.<name>.checks`
is a list of testScript-fragment files, concatenated into one `testScript`
passed to whichever backend's test-runner function. Building that derivation
boots the VM, runs the script, tears down, and fails the build on assertion
failure; there is no separate check-runner process to implement. `--build-only`
therefore cannot mean "build the test derivation without booting" (nixosTest
derivations cannot be built without running); it means "build the extended
configuration's plain `system.build.toplevel` (or `.vm`)", a genuinely separate,
faster, boot-free artifact used as a pre-check. This also makes the
`--dev`/`--test` SSH distinction for `kind = "nixos"` exact: `vm-guest.nix` (the
`xi` user, keypair, passwordless sudo) applies only to `--dev`, since `--test`
is never SSHead into, the driver talks to the machine object directly.

**Port forwarding is resolved at launch time, by the supervisor, never baked
into the Nix build.** `vm-guest.nix` declares no `forwardPorts` at all. When
`xi vm start` constructs the actual QEMU invocation (foreground or
`--background`), it resolves: the SSH forward (guest port 22, always, implicit,
no manifest field) by probing for a free host port from a base and incrementing
on collision; each declared app forward's host port, trying the manifest's
`hostPort` hint first if given (failing loudly on collision unless
`--allow-port-fallback` is passed) or resolving dynamically if no hint was
given. Every resolved port for the current instance is written into the per-VM
registry (alongside PID and control socket path) and is what `xi vm status`
reports and `xi vm connect` uses, there is no other source of truth for "what
port is this on right now".

**Backgrounded VMs register on a per-VM control socket** at
`<state_dir>/vm/<name>/control.sock`, using the same length-prefixed JSON
framing as `xi-sources-daemon/src/control.rs`. `xi vm start --background` spawns
the QEMU process (with its resolved ports already applied) and a small
supervisor that owns the socket, replying to `status`, `stop`, and
`console-attach` requests. `xi vm status` and `xi vm stop` are thin clients over
this socket, exactly like `xi push`'s `pause`/`resume` calls today. This applies
to `kind = "nixos"` only.

**Guest provisioning creates a dedicated `xi` user, injected, never
self-described, `--dev`-only.** `modules/nixos/vm-guest.nix` creates a login
account named `sshUser` (default `"xi"`) with the xi-managed authorized key and,
when `passwordlessSudo` is true (default), passwordless sudo scoped to that one
user via `security.sudo.extraRules` (never a blanket
`wheelNeedsPassword = false`). Creating a new user rather than appending to an
existing one makes non-destructiveness structural: there is no existing option
value to merge with, only a new `users.users.xi` attribute path. The one
remaining risk, the target already declaring a user literally named `sshUser`,
is a build-time assertion naming the collision, not a silent merge. Passwordless
sudo for `xi` is safe to default on because `extendModules` produces a separate
evaluated configuration: the `xi` user and its sudo rule exist only in the
`extendModules`-produced `.config.system.build.vm`, never in the original
`nixosConfigurations.<name>.config.system.build.toplevel` used for real
deployment (e.g. via `xi os switch`). `vm-guest.nix` is also exported as a plain
public `nixosModule`, for manual composition by anyone not going through
flake-parts.

**`kind = "distro"` reuses the exact same manifest, the exact same `checks`
field, and a scoped subset of the lifecycle verbs.** `build`/`check` map onto
`nix-vm-test.lib.<system>.<distro>."<version>" { ...; testScript = ...; }`'s
`.driver` output (`check`) or are simply its build (`build`). `start` runs
`.driverInteractive` in the foreground (QEMU window plus the interactive Python
console nix-vm-test already provides, there is nothing for `xi vm
connect` to
add on top of that). `--background`, `stop`, and `connect` are explicitly
unsupported for this kind and fail with a clear "unsupported for kind = distro"
error rather than attempting and failing confusingly. `xi vm start --all` skips
`kind = "distro"` entries with a note rather than erroring the whole command,
since there is nothing to background for them.

## Consequences

### Positive

- One build code path; `xi vm build` and `xi os build-vm` cannot drift apart on
  correctness, only on surface ergonomics.
- Checks reuse an established, already-debugged mechanism
  (`testers.runNixOSTest`, `nix-vm-test`) instead of a bespoke SSH runner; less
  code, and the exact convention the dotfiles precedent already used.
- The same `checks` field and manifest shape extend to non-NixOS test VMs almost
  for free, because the backend abstraction point (testScript) was already the
  right one for the NixOS-only case.
- Dynamic port resolution eliminates a whole class of "works until you start a
  second VM" collisions, and matches how a human would actually debug a "port
  already in use" QEMU failure today, just automated and reported via
  `xi vm status` instead of a cryptic QEMU stderr line.
- The control-socket pattern is proven in this codebase already
  (`xi-sources-daemon`), so the VM registry isn't a novel IPC design.
- `xi vm` reads as the docker-compose/vagrant surface the user described, with
  no scope ceremony in front of it.

### Negative

- Two names exist for "build a VM" (`xi vm build`, `xi os build-vm`) during the
  deprecation window. Mitigated by the alias being a one-line call-through plus
  a warning.
- Two Nix modules (`modules/flake-parts/vm.nix`, `modules/nixos/vm-guest.nix`)
  and a third-party flake input (`nix-vm-test`) instead of one self-contained
  thing. Mitigated by `vm-guest.nix` also being a plain exported `nixosModule`,
  and by `nix-vm-test` being a small, focused, already-stable (beta-grade,
  backward-compatible API per its own docs) dependency rather than something
  this change has to maintain.
- `kind = "distro"` is lifecycle-asymmetric with `kind = "nixos"` (no
  background/stop/connect). An operator skimming the CLI help could expect
  symmetry that isn't there. Mitigated by each unsupported verb failing with an
  explicit, named reason rather than a confusing lower-level error, and by this
  being a real capability gap in the upstream tool, not an arbitrary restriction
  this change invented.
- Resolving ports at launch time means the host port for a given VM is not
  stable across restarts unless the operator pins it via `hostPort` (for app
  forwards). SSH's port specifically is never pinnable by design (see Decision
  Drivers). Mitigated by `xi vm status`/`connect` always reflecting the current
  truth, so nothing has to be memorized across restarts.
- `xi vm` sits outside the scope resolver, so it does not benefit from
  `[[xi-sources-model]]`'s "scope comes from the verb" guarantee,
  `xi.vm.<name>.*` therefore has to name its own flake/attr explicitly.
  Acceptable: a VM is never "the machine you're switching".
- The per-VM control socket is a second long-running local process type
  (alongside the sources daemon) an operator can forget to clean up. Mitigated
  by `xi vm stop --all` and by the supervisor removing its own socket file on
  clean exit.

## Related

- [[xi-sources-model]] owns the verb-to-scope resolver that `xi vm` deliberately
  sits outside of.
- `xi-sources-daemon/src/control.rs` is the control-socket implementation this
  change reuses the framing from (no spec capability of its own, it is
  implementation, not a documented surface).
- [numtide/nix-vm-test](https://github.com/numtide/nix-vm-test) is the external
  dependency backing `kind = "distro"`.
