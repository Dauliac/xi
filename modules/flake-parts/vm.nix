{
  config,
  lib,
  pkgs,
  system,
  xiVmTestInput ? null,
  ...
}:
let
  cfg = config.xi.vm;

  # Mirrors `crates/xi-core/src/sources/config.rs::xdg_state_home`: the
  # same impure fallback chain, read at eval time so the guest-account
  # module and the Rust CLI agree on one path without either side
  # hardcoding the other's default. Only used to locate a *public* key,
  # so reading it into the store (via `vm-guest.nix`'s `lib.fileContents`)
  # carries no secret-leak risk.
  stateDir =
    let
      xdg = builtins.getEnv "XDG_STATE_HOME";
      home = builtins.getEnv "HOME";
      base = if xdg != "" then xdg else if home != "" then "${home}/.local/state" else "/tmp";
    in
    "${base}/xi";

  # The same primitive `lib.nixosSystem` wraps, called directly so this
  # module never needs the consuming flake's own top-level `self`
  # (unavailable here: flake-parts only threads `self`/`inputs` into the
  # top-level module, not into a `perSystem` import - see the `modules`
  # option's doc in `modules/shared/types/vm.nix`).
  evalNixos =
    modules:
    import "${pkgs.path}/nixos/lib/eval-config.nix" {
      inherit system;
      inherit modules;
    };

  mkNixosOutputs =
    name: entry:
    let
      base = evalNixos entry.modules;

      devExtended = base.extendModules {
        modules = [
          ../nixos/vm-guest.nix
          {
            services.xiVmGuest = {
              enable = true;
              sshUser = entry.sshUser;
              inherit stateDir;
              # Computed against `base` (pre-extension) on purpose: by
              # the time `vm-guest.nix` itself can read `config.users.users`,
              # its own `users.users.${sshUser}` definition is already
              # merged in, so it could never distinguish "already there"
              # from "I just added it". Only this module, which holds
              # both the plain and the extended config, can tell.
              sshUserAlreadyDeclared = builtins.hasAttr entry.sshUser base.config.users.users;
            };
          }
        ];
      };

      checkScript = lib.concatMapStringsSep "\n" builtins.readFile entry.checks;

      testDerivation =
        if entry.modules == [ ] then
          null
        else
          pkgs.testers.runNixOSTest {
            name = "xi-vm-${name}-test";
            nodes.machine = {
              imports = entry.modules;
            };
            testScript = if checkScript == "" then "machine.wait_for_unit('multi-user.target')" else checkScript;
          };
    in
    {
      packages = {
        "xi-vm-${name}-vm" = base.config.system.build.vm;
        "xi-vm-${name}-vm-with-bootloader" = base.config.system.build.vmWithBootLoader;
        "xi-vm-${name}-dev-vm" = devExtended.config.system.build.vm;
        "xi-vm-${name}-dev-vm-with-bootloader" = devExtended.config.system.build.vmWithBootLoader;
        "xi-vm-${name}-build-only" = base.config.system.build.toplevel;
      }
      // lib.optionalAttrs (testDerivation != null) {
        "xi-vm-${name}-test" = testDerivation;
      };
      checks = lib.optionalAttrs (testDerivation != null) {
        "xi-vm-${name}-test" = testDerivation;
      };
    };

  mkDistroOutputs =
    name: entry:
    let
      backend =
        if xiVmTestInput == null then
          null
        else
          xiVmTestInput.lib.${system}.${entry.distro}."${entry.version}" or null;
      checkScript = lib.concatMapStringsSep "\n" builtins.readFile entry.checks;
      sharedDirs = lib.listToAttrs (
        map (d: lib.nameValuePair (baseNameOf d.guest) { source = d.host; target = d.guest; }) entry.sharedDirs
      );
    in
    if backend == null then
      {
        packages = { };
        checks = { };
      }
    else
      {
        packages = {
          "xi-vm-${name}-distro-build" =
            (backend {
              inherit sharedDirs;
              cpus = entry.cpus;
              memorySize = entry.memorySize;
              testScript = "";
            }).driver;
          "xi-vm-${name}-distro-interactive" =
            (backend {
              inherit sharedDirs;
              cpus = entry.cpus;
              memorySize = entry.memorySize;
              testScript = "";
            }).driverInteractive;
        };
        checks = {
          "xi-vm-${name}-test" =
            (backend {
              inherit sharedDirs;
              cpus = entry.cpus;
              memorySize = entry.memorySize;
              testScript = checkScript;
            }).driver;
        };
      };

  perVm = lib.mapAttrs (
    name: entry: if entry.kind == "nixos" then mkNixosOutputs name entry else mkDistroOutputs name entry
  ) cfg;

  manifest = lib.mapAttrs (
    _name: entry: {
      inherit (entry)
        kind
        display
        withBootloader
        sshUser
        ports
        distro
        version
        ;
    }
  ) cfg;
in
{
  options.xi.vm = lib.mkOption {
    type = lib.types.attrsOf config._xi.types.vm;
    default = { };
    description = ''
      Declarative multi-VM dev-space lifecycle: "Vagrant / docker-compose,
      but for NixOS machines, inside xi". A flake-parts-only option tree
      (no NixOS/home-manager mirror: a VM manifest has no installed-system
      activation step to mirror). Each attribute name is the VM name `xi
      vm` subcommands take as `<name>`.

      Reading this option back into `packages`/`checks` is deliberately
      NOT automatic here (doing so recurses - see
      `modules/shared/lib/vm.nix`'s header comment). Wire it from your own
      `perSystem`:

        packages = config.lib.mkVmPackages {
          inherit pkgs;
          rendered = config._xi.vm.rendered;
          manifest = config._xi.vm.manifest;
        };
        checks = config.lib.mkVmChecks { rendered = config._xi.vm.rendered; };

      For each declared `kind = "nixos"` entry this renders, under
      `rendered.<name>.packages`:
      - `xi-vm-<name>-vm` / `-vm-with-bootloader`: the plain configuration's
        VM build (no guest account).
      - `xi-vm-<name>-dev-vm` / `-dev-vm-with-bootloader`: the same,
        extended with `modules/nixos/vm-guest.nix` (`xi vm build --dev`,
        `start`, `connect` use this).
      - `xi-vm-<name>-build-only`: the plain configuration's
        `system.build.toplevel`, a boot-free pre-check.
      - `xi-vm-<name>-test` (also under `.checks`): a
        `pkgs.testers.runNixOSTest` derivation running the concatenated
        `checks` testScript fragments against `entry.modules`.

      For each declared `kind = "distro"` entry, when the top-level flake
      providing this module also feeds it a `nix-vm-test` input (xi's own
      flake does; see `nix/modules.nix`'s `xiVmTestInput` wiring):
      - `xi-vm-<name>-distro-build`: `nix-vm-test`'s `.driver`, no checks.
      - `xi-vm-<name>-test` (also under `.checks`): the same, with the
        concatenated `checks` testScript.
    '';
  };

  options._xi.vm = {
    rendered = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Per-VM { packages; checks; } attrsets, keyed by VM name.";
    };
    manifest = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Per-VM display metadata `xi vm` reads at runtime, keyed by VM name.";
    };
  };

  config._xi.vm = {
    rendered = perVm;
    inherit manifest;
  };
}
