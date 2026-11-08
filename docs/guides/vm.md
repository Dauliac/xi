# VM lifecycle

`xi vm` is a declarative multi-VM dev-space manager: "Vagrant /
docker-compose, but for NixOS machines, inside xi." It is a top-level
command, independent of the `os`/`home`/`darwin`/`system` scope resolver -
a VM is never "the machine you're switching".

## Declare a VM

In your flake, after importing `xi.flakeModule`:

```nix
perSystem = { config, pkgs, ... }: {
  xi.vm.gary = {
    kind = "nixos"; # the default; "distro" targets non-NixOS images
    modules = [ ./hosts/gary/configuration.nix ];
    display = "graphical"; # or "headless"
    ports = [
      { guestPort = 8080; hostPort = 8080; protocol = "tcp"; }
    ];
  };

  # Required: wire the rendered packages/checks into your own perSystem.
  # Not automatic (see modules/flake-parts/vm.nix's `xi.vm` option doc
  # for why), but a one-time two-line addition.
  packages = config.lib.mkVmPackages {
    inherit pkgs;
    rendered = config._xi.vm.rendered;
    manifest = config._xi.vm.manifest;
  };
  checks = config.lib.mkVmChecks { rendered = config._xi.vm.rendered; };
};
```

`modules` is the same NixOS module list you'd pass to
`nixpkgs.lib.nixosSystem`'s `modules` argument - typically the same file(s)
your own `nixosConfigurations.gary` is built from, repeated here (a
flake-parts `perSystem` module has no way to reach the consuming flake's
top-level `nixosConfigurations` by reference; see that same option doc).

## Build, check, start

```sh
xi vm build gary --dev          # the persistent, bootable image
xi vm check gary                # boots gary, runs its declared checks
xi vm check gary --build-only   # boot-free pre-check (no VM is ever booted)
xi vm start gary                # build (if needed) + launch, foreground
xi vm start gary --background    # detach, register for status/stop
xi vm status gary
xi vm connect gary               # SSH in with the xi-managed keypair
xi vm stop gary
```

`xi vm list` enumerates every declared VM with its status.

## Checks

`xi.vm.<name>.checks` is a list of Python testScript fragments, concatenated
and passed to `pkgs.testers.runNixOSTest`. Building that derivation boots
the VM, runs the script, tears down, and fails the build on assertion
failure - there is no separate check-runner process. `--build-only` builds
only the plain `system.build.toplevel`, a faster boot-free pre-check.

## Ports

Port availability is a property of the host at launch time, not something
Nix eval can see: no port is baked into the build. The implicit SSH forward
is always dynamically resolved; a declared app forward's `hostPort` is
tried first, failing loudly on conflict unless `--allow-port-fallback` is
passed. `xi vm status`/`connect` always reflect the currently resolved
ports - nothing to memorize across restarts.

## `kind = "distro"`

For non-NixOS targets (Debian, Ubuntu, Fedora, Rocky, Arch) via
[numtide/nix-vm-test](https://github.com/numtide/nix-vm-test):

```nix
xi.vm.patrick = {
  kind = "distro";
  distro = "debian";
  version = "13";
};
```

`build`/`check` map onto its `.driver` output; `start` runs
`.driverInteractive` in the foreground (a QEMU window plus an interactive
Python console). `--background`, `stop`, and `connect` are unsupported for
this kind and fail with a clear error naming it, rather than a confusing
lower-level failure.

## Migrating from `xi os build-vm`

`xi os build-vm` keeps working, as a deprecated alias, for the current
flake's `nixosConfiguration`. There is no required migration: adopt
`xi.vm.<name>` at your own pace.

## Implementation notes

This is a from-scratch implementation reviewed against a real build in
this repo's own test fixture (`tests/flake-module`) - the manifest, the
boot-free `--build-only` check, and a full `runNixOSTest` boot (under QEMU
TCG, no KVM) all built and passed. Two things fell short of
`openspec/changes/xi-vm-lifecycle/`'s original design, by necessity rather
than oversight:

- **No control socket.** The design called for reusing
  `xi-sources-daemon/src/control.rs`'s framing for a per-VM supervisor.
  This build instead records PID/ports/display mode in one JSON file per
  VM (`crates/xi-vm/src/registry.rs`); `status` is a `kill(pid, 0)` probe
  and `stop` sends `SIGTERM`. `--console` (`xi vm connect --console`) is
  not implemented, since it needs that supervisor's RPC.
- **`kind = "distro"` is unverified and not wired up by default.**
  `modules/flake-parts/vm.nix` accepts an `xiVmTestInput` perSystem arg
  and uses it as the `nix-vm-test` backend when present, but xi's own
  `flake.nix` does not add that input or thread the arg - doing so means
  pinning and testing a new third-party dependency, which this change's
  authoring environment had no `qemu-system-*` binary to actually build
  against. A consumer who wants `kind = "distro"` today must add the
  `nix-vm-test` input themselves and supply `_module.args.xiVmTestInput`
  in their own `perSystem`.
