# Tasks

## 1. Manifest and config

- [ ] 1.1 Add `modules/shared/types/vm.nix`: the shared submodule type for one
      VM entry with a `kind` (`"nixos"` default | `"distro"`) discriminator.
      `kind = "nixos"` fields: `flakeRef`/`attr`, `cores`, `memorySize`,
      `diskSize`, `display`, `withBootloader`, app port forwards (`guestPort`,
      optional `hostPort`, `protocol`), `sshUser`. `kind = "distro"` fields:
      `distro`, `version`, `sharedDirs`, `cpus`, `memorySize`, `diskSize`.
      Shared: `checks` (list of testScript-fragment paths). Mirrors
      `modules/shared/types/sources.nix`.
- [ ] 1.2 Add `modules/flake-parts/vm.nix` exposing `xi.vm.<name>.*` (bare
      `xi.*`, flake-parts-only, no NixOS/home-manager mirror, see design.md).
- [ ] 1.3 Add `[vm.<name>]` section to the `.xi.toml` schema with a closed key
      list (mirror `crates/xi-core/src/sources/config.rs`'s `SOURCES_KEYS`
      pattern) and the CLI flag > env (`XI_VM_*`) > toml > Nix-default >
      built-in precedence ladder, with provenance tracking for `--explain`.
- [ ] 1.4 Unit tests: unknown key under `[vm.*]` fails to load with the
      offending key named; precedence ladder resolves correctly at each rung;
      `kind` discriminator rejects `nixos`-only or `distro`-only keys declared
      under the wrong kind.

## 2. Guest provisioning module (`kind = "nixos"`, `--dev` only)

- [ ] 2.1 Add `modules/nixos/vm-guest.nix`: creates a dedicated login account
      `sshUser` (default `"xi"`) with the xi-managed authorized key; add an
      assertion that fails the build if the target already declares a user named
      `sshUser`. Declares no port forwards. Export it as a plain public
      `nixosModule` too.
- [ ] 2.2 Add `passwordlessSudo` (default `true`) to `vm-guest.nix`: scoped
      `security.sudo.extraRules` for `sshUser` only, never `wheelNeedsPassword`.
      Every setting uses `lib.mkDefault`, never `lib.mkForce`.
- [ ] 2.3 Wire the flake-parts module to inject `vm-guest.nix` via
      `nixosConfigurations.<name>.extendModules { modules = [ ... ]; }` for the
      `--dev` build only; `--test` builds the extended-or-plain configuration
      with no `vm-guest.nix` injection at all (see group 4).
- [ ] 2.4 NixOS VM test: extend a `nixosConfiguration` that already declares its
      own `services.openssh` settings and its own user with its own authorized
      key; assert the original user/settings are unchanged and a separate `xi`
      user exists with the xi-managed key and passwordless sudo.
- [ ] 2.5 Regression test: extending a target that already declares a user named
      `sshUser` fails the build with an assertion naming the collision, rather
      than merging the two user declarations.
- [ ] 2.6 Regression test: building the target's real
      `.config.system.build.toplevel` (not via `xi vm build`) contains no `xi`
      user and no passwordless sudo rule for it.

## 3. CLI surface

- [ ] 3.1 New crate `xi-vm` with `args.rs` defining `VmArgs`/`VmSubcommand`
      (`list`, `build [--dev|--test] [--with-bootloader]`,
      `check
      [--build-only]`,
      `start [--background] [--allow-port-fallback]`, `stop`,
      `connect [--console]`, `status`).
- [ ] 3.2 Mount `XiCommand::Vm(xi_vm::args::VmArgs)` as a new top-level arm in
      `crates/xi/src/interface.rs`, "Development" banner group; extend the
      `get_feature_requirements()`/scope-resolution match arms this requires.
- [ ] 3.3 `xi vm list`: read manifest entries + instance registry + last check
      verdicts, render the combined table (and `--json` envelope);
      `kind = "distro"` entries always show `unknown` liveness.

## 4. Build

- [ ] 4.1 Implement the `kind = "nixos"` `--dev` build path: evaluate the
      manifest entry's flake attribute, `extendModules` with `vm-guest.nix`,
      build `system.build.vm`/`vmWithBootLoader` (per `--with-bootloader`/
      `xi.vm.<name>.withBootloader`).
- [ ] 4.2 Implement the `kind = "nixos"` `--test` build path: build
      `pkgs.testers.runNixOSTest { nodes.machine = <extended, NO
      vm-guest.nix>; testScript = <concatenated checks>; }`
      (see group 5 for the concatenation logic).
- [ ] 4.3 Implement the `kind = "distro"` build path:
      `nix-vm-test.lib.<system>.<distro>."<version>" { sharedDirs; cpus;
      memorySize; diskSize; testScript; }`;
      add the `nix-vm-test` flake input.
- [ ] 4.4 Rewire `crates/xi-nixos/src/{args.rs,nixos.rs}`'s `build-vm` to
      resolve the current flake's `nixosConfiguration` name and call into
      `xi-vm`'s `kind = "nixos"` `--dev` build path, passing through
      `--with-bootloader`/`-B`; add the one-line deprecation notice naming
      `xi vm build --dev`.
- [ ] 4.5 Regression test: `xi vm build <name> --dev [--with-bootloader]` and
      `xi os build-vm [--with-bootloader]` produce the same store path for an
      unchanged flake.

## 5. Check

- [ ] 5.1 Implement the testScript-fragment concatenation: read
      `xi.vm.<name>.checks` (list of paths), concatenate their contents into one
      `testScript` string, shared by the `kind = "nixos"` `--test` build and the
      `kind = "distro"` build.
- [ ] 5.2 `xi vm check`: build and run the check derivation (group 4.2/4.3); the
      build's own exit status is the verdict. Define the closed `VmCheckReason`
      token enum (`build-failed`, `boot-timeout`, `check-failed:<name>`, ...)
      and the versioned JSON status envelope (`schema_version`, `status`,
      `reasons`, `duration_ms`).
- [ ] 5.3 `--build-only` (`kind = "nixos"` only): build the extended
      configuration's plain `config.system.build.toplevel`/`.vm`, skip the
      `runNixOSTest` derivation entirely; verify no VM is ever booted.
- [ ] 5.4 `xi vm check --all`: run every declared VM's checks (or build-only
      checks) independently, continue past a failing VM, aggregate exit code.

## 6. Port resolution

- [ ] 6.1 Implement free-host-port probing (base port, increment on
      `EADDRINUSE`-equivalent) for the always-dynamic SSH forward
      (`kind = "nixos"`, `--dev` only).
- [ ] 6.2 Implement `hostPort`-hint resolution for declared app forwards: try
      the hint, fail loudly naming the conflict by default, fall back to dynamic
      resolution under `--allow-port-fallback`.
- [ ] 6.3 Inject the resolved ports into the actual QEMU invocation at
      `xi vm start` launch time (verify against the current nixpkgs
      `qemu-vm.nix` runner script's extra-flags mechanism, or use QEMU monitor
      `hostfwd_add` after boot if simpler to integrate), not into the Nix build.
- [ ] 6.4 Record resolved ports in the per-VM registry entry;
      `xi vm
      status`/`connect` read from there, never from static
      configuration.
- [ ] 6.5 Regression test: two VMs started `--background` concurrently resolve
      different SSH ports and are both independently reachable.
- [ ] 6.6 Regression test: a pinned `hostPort` conflict fails `start` by default
      and succeeds on a different, reported port under `--allow-port-fallback`.

## 7. Start/stop and the background registry (`kind = "nixos"`)

- [ ] 7.1 Implement the per-VM control socket supervisor
      (`<state_dir>/vm/<name>/control.sock`), reusing the length-prefixed
      JSON-over-`AF_UNIX` framing from `xi-sources-daemon/src/control.rs`;
      handles `status`, `stop`, `console-attach`.
- [ ] 7.2 `xi vm start`: always build the `--dev` artifact first (no separate
      staleness tracking, rely on Nix's build cache); foreground path (blocks
      for VM lifetime) and `--background` path (resolve ports, spawn,
      supervisor, registry write, return once up).
- [ ] 7.3 `xi vm start --all`: `kind = "nixos"` entries only, implies
      `--background`, defaults headless unless `--graphical` passed;
      `kind
      = "distro"` entries reported as skipped, not an error.
- [ ] 7.4 `xi vm start <name>` for `kind = "distro"`: run `nix-vm-test`'s
      `.driverInteractive` in the foreground; `--background` fails with an
      explicit "unsupported for kind = distro" error.
- [ ] 7.5 `xi vm stop`: client call to the control socket; clear error when no
      instance is registered; clear "unsupported for kind = distro" error for
      that kind.
- [ ] 7.6 `xi vm status`: liveness + resolved ports via control socket + last
      check verdict from the check runner's stored result.

## 8. Connect (`kind = "nixos"`)

- [ ] 8.1 Generate an xi-managed throwaway SSH keypair and feed its public half
      into `vm-guest.nix`'s authorized-key injection at build time (no manual
      key management; see task 2.1).
- [ ] 8.2 `xi vm connect <name>`: SSH as `sshUser` (default `"xi"`) using the
      baked keypair against the instance's dynamically resolved SSH port (read
      from the registry, see task 6.4).
- [ ] 8.3 `xi vm connect <name> --console`: `console-attach` request over the
      control socket, passthrough to QEMU's serial socket.
- [ ] 8.4 Clear errors: no instance running; `kind = "distro"` (unsupported,
      note that `start` already opens an interactive console).

## 9. Docs and migration

- [ ] 9.1 `docs/guides/vm.md`: the `xi vm` workflow (declare, build, check,
      start, connect, stop) for `kind = "nixos"`, and the `kind = "distro"`
      subset (build, check, start-only); cross-linking from
      `docs/guides/nixos.md`'s existing "Build a VM" section.
- [ ] 9.2 `docs/reference/cli.md`: document the new `xi vm` subcommand group
      (including `--dev`/`--test`, `--with-bootloader`, `--build-only`,
      `--allow-port-fallback`) and the `build-vm` deprecation notice.
- [ ] 9.3 Deprecation note in `xi os build-vm`'s `--help` output pointing at
      `xi vm build --dev`.
