{ lib, ... }:
let
  portEntry = lib.types.submodule {
    options = {
      guestPort = lib.mkOption {
        type = lib.types.port;
        description = "Port the guest listens on.";
      };

      hostPort = lib.mkOption {
        type = lib.types.nullOr lib.types.port;
        default = null;
        description = ''
          Preferred host port. `xi vm start` tries this first and fails
          loudly naming the conflict unless `--allow-port-fallback` is
          passed, in which case it falls back to dynamic resolution.
          Left null to always resolve dynamically.
        '';
      };

      protocol = lib.mkOption {
        type = lib.types.enum [
          "tcp"
          "udp"
        ];
        default = "tcp";
        description = "Forwarded protocol.";
      };
    };
  };

  sharedDirEntry = lib.types.submodule {
    options = {
      host = lib.mkOption {
        type = lib.types.str;
        description = "Absolute path on the host to share.";
      };

      guest = lib.mkOption {
        type = lib.types.str;
        description = "Absolute path inside the guest the host dir is mounted at.";
      };
    };
  };
in
{
  options._xi.types.vm = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "One declared VM's submodule type (xi-vm-lifecycle).";
  };

  # One definition, used only by modules/flake-parts/vm.nix: a VM manifest
  # has no installed-system activation step to mirror the way xi-sources
  # does at NixOS/home-manager scope, so this option tree is flake-parts
  # only (see openspec/changes/xi-vm-lifecycle/design.md).
  config._xi.types.vm = lib.types.submodule (
    { name, ... }:
    {
      options = {
        kind = lib.mkOption {
          type = lib.types.enum [
            "nixos"
            "distro"
          ];
          default = "nixos";
          description = ''
            Which backend builds and runs this VM. `nixos` extends a
            `nixosConfiguration` from this flake; `distro` builds a
            non-NixOS image through `numtide/nix-vm-test`.
          '';
        };

        # -- kind = "nixos" -------------------------------------------------

        modules = lib.mkOption {
          type = lib.types.listOf lib.types.path;
          default = [ ];
          description = ''
            The plain NixOS modules backing this VM, evaluated directly by
            `xi vm` (via `<nixpkgs>/nixos/lib/eval-config.nix`, the same
            primitive `lib.nixosSystem` wraps) rather than by resolving an
            already-built `nixosConfigurations.<name>` flake output.

            A flake-parts `perSystem` module has no access to the
            consuming flake's own top-level `self` (only the top-level
            module gets it; see flake-parts' own `perSystem` argument
            error), so `xi.vm.<name>` cannot generically reuse an
            existing `nixosConfigurations` entry by reference - passing
            the same module list this flake's own `nixosConfigurations.${name}`
            (if one happens to exist) is built from is the supported way
            to keep the two in sync.
          '';
        };

        cores = lib.mkOption {
          type = lib.types.ints.positive;
          default = 2;
          description = "Virtual CPU cores given to the VM.";
        };

        memorySize = lib.mkOption {
          type = lib.types.ints.positive;
          default = 2048;
          description = "Guest RAM in MiB.";
        };

        diskSize = lib.mkOption {
          type = lib.types.ints.positive;
          default = 4096;
          description = "Guest disk size in MiB.";
        };

        display = lib.mkOption {
          type = lib.types.enum [
            "graphical"
            "headless"
          ];
          default = "graphical";
          description = ''
            Default display mode for `xi vm start`. Overridable per
            invocation by `--graphical`/`--headless`.
          '';
        };

        withBootloader = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = ''
            Build `vmWithBootLoader` instead of the plain `vm` by default,
            matching `xi os build-vm`'s `-B`/`--with-bootloader`.
          '';
        };

        sshUser = lib.mkOption {
          type = lib.types.str;
          default = "xi";
          description = ''
            Login account `modules/nixos/vm-guest.nix` creates on the
            `--dev` build, and `xi vm connect` uses. Not a reference to a
            user the target configuration is assumed to already have.
          '';
        };

        ports = lib.mkOption {
          type = lib.types.listOf portEntry;
          default = [ ];
          description = ''
            App port forwards. The SSH forward (guest port 22) is implicit
            and always dynamically resolved; it is not declared here.
          '';
        };


        # -- kind = "distro" --------------------------------------------------

        distro = lib.mkOption {
          type = lib.types.nullOr (
            lib.types.enum [
              "debian"
              "ubuntu"
              "fedora"
              "rocky"
              "arch"
            ]
          );
          default = null;
          description = "Distro backend, when `kind = \"distro\"`.";
        };

        version = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "13";
          description = "Distro version string, when `kind = \"distro\"`.";
        };

        sharedDirs = lib.mkOption {
          type = lib.types.listOf sharedDirEntry;
          default = [ ];
          description = "Host/guest directory pairs, when `kind = \"distro\"`.";
        };

        cpus = lib.mkOption {
          type = lib.types.ints.positive;
          default = 2;
          description = "Virtual CPU cores, when `kind = \"distro\"`.";
        };

        # -- shared -----------------------------------------------------------

        checks = lib.mkOption {
          type = lib.types.listOf lib.types.path;
          default = [ ];
          description = ''
            testScript fragment files, concatenated into one `testScript`
            passed to `pkgs.testers.runNixOSTest` (`kind = "nixos"`) or
            `nix-vm-test` (`kind = "distro"`). Building that derivation IS
            running the check.
          '';
        };
      };
    }
  );
}
