{
  self,
  lib,
  ...
}:
let
  repoRoot = ../.;

  sourcesTypeModule = repoRoot + "/modules/shared/types/sources.nix";
  nixosHead = repoRoot + "/modules/nixos/sources.nix";
  homeManagerHead = repoRoot + "/modules/home-manager/sources.nix";
  flakePartsHead = repoRoot + "/modules/flake-parts/sources.nix";

  # The renderer, reached directly rather than through nix-lib's
  # `nix-lib.lib.*` plumbing, so these fixtures can evaluate a single
  # head in isolation instead of dragging in the whole module tree.
  mkSourcesFile =
    (import (repoRoot + "/modules/shared/lib/sources.nix") { inherit lib; }).nix-lib.lib.mkSourcesFile.fn;

  unspecifiedAttrs = lib.mkOption {
    type = lib.types.attrsOf lib.types.unspecified;
    default = { };
  };

  fileAttrs = lib.mkOption {
    type = lib.types.attrsOf (lib.types.attrsOf lib.types.unspecified);
    default = { };
  };

  assertionsOption = lib.mkOption {
    type = lib.types.listOf lib.types.unspecified;
    default = [ ];
  };

  # Minimal stand-ins for the host options each head writes into. A head
  # is a unit under test here: the real NixOS module system is exercised
  # by the VM test below, and stubbing lets the assertion fixtures read
  # `config.assertions` instead of having to make an evaluation throw.
  nixosStub = {
    options = {
      assertions = assertionsOption;
      lib = unspecifiedAttrs;
      programs.xi.enable = lib.mkEnableOption "xi";
      environment.etc = fileAttrs;
      systemd.tmpfiles.rules = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
      };
      home-manager.users = unspecifiedAttrs;
    };
    config.lib = { inherit mkSourcesFile; };
  };

  # home-manager's `lib.hm.dag` is not part of nixpkgs lib, so the
  # fixture supplies the two bits the head touches.
  hmLib = lib // {
    hm.dag.entryAfter = after: data: { inherit after data; };
  };

  homeManagerStub = {
    options = {
      assertions = assertionsOption;
      lib = unspecifiedAttrs;
      programs.xi.enable = lib.mkEnableOption "xi";
      xdg.dataHome = lib.mkOption { type = lib.types.str; };
      xdg.stateHome = lib.mkOption { type = lib.types.str; };
      xdg.configFile = fileAttrs;
      home.activation = unspecifiedAttrs;
    };
    config = {
      lib = { inherit mkSourcesFile; };
      xdg.dataHome = "/home/anon/.local/share";
      xdg.stateHome = "/home/anon/.local/state";
    };
  };

  flakePartsStub = {
    options = {
      xi.enable = lib.mkEnableOption "xi";
      xi.settings = unspecifiedAttrs;
    };
  };

  evalHead =
    {
      head,
      stub,
      modules ? [ ],
      specialArgs ? { },
    }:
    lib.evalModules {
      # `lib` reaches a module through `specialArgs`, not through
      # `_module.args`: the module system resolves an argument from its
      # own `args` set first, so only a specialArg can shadow `lib`.
      inherit specialArgs;
      modules = [
        sourcesTypeModule
        head
        stub
      ]
      ++ modules;
    };

  # ---------------------------------------------------------------------
  # Option-tree parity
  # ---------------------------------------------------------------------

  descriptionOf =
    opt:
    let
      d = opt.description or null;
    in
    if d == null then
      ""
    else if lib.isAttrs d then
      d.text
    else
      d;

  # Walk an option set, recording every leaf and every sub-option a
  # submodule contributes, so a head that redeclares or drops anything
  # shows up as a line-level diff.
  collect =
    prefix: tree:
    lib.concatMap (
      name:
      let
        value = tree.${name};
        path = prefix ++ [ name ];
      in
      if lib.isOption value then
        let
          # `attrsOf` hides a name level that the option paths should
          # show, so it is spelled explicitly rather than collapsed.
          subPrefix =
            if lib.elem value.type.name [ "attrsOf" "lazyAttrsOf" ] then path ++ [ "<name>" ] else path;
        in
        [
          {
            option = lib.concatStringsSep "." path;
            type = value.type.description;
            default = if value ? default then builtins.toJSON value.default else "<required>";
            description = descriptionOf value;
          }
        ]
        ++ collect subPrefix (value.type.getSubOptions subPrefix)
      else
        collect path value
    ) (lib.sort (a: b: a < b) (lib.attrNames tree));

  renderTree =
    leaves:
    lib.concatMapStrings (leaf: ''
      ${leaf.option}
        type: ${leaf.type}
        default: ${leaf.default}
        description: ${leaf.description}
    '') leaves;

  treeOf =
    {
      head,
      stub,
      path,
      specialArgs ? { },
    }:
    let
      evaluated = evalHead { inherit head stub specialArgs; };
      option = lib.getAttrFromPath path evaluated.options;
    in
    renderTree (collect [ ] (option.type.getSubOptions [ ]));

  heads = {
    nixos = {
      head = nixosHead;
      stub = nixosStub;
      path = [
        "programs"
        "xi"
        "sources"
      ];
    };
    home-manager = {
      head = homeManagerHead;
      stub = homeManagerStub;
      specialArgs.lib = hmLib;
      path = [
        "programs"
        "xi"
        "sources"
      ];
    };
    flake-parts = {
      head = flakePartsHead;
      stub = flakePartsStub;
      path = [
        "xi"
        "sources"
      ];
    };
  };

  # ---------------------------------------------------------------------
  # Behaviour fixtures
  # ---------------------------------------------------------------------

  nixosWith =
    settings:
    (evalHead {
      head = nixosHead;
      stub = nixosStub;
      modules = [ settings ];
    }).config;

  homeManagerWith =
    settings:
    (evalHead {
      head = homeManagerHead;
      stub = homeManagerStub;
      specialArgs.lib = hmLib;
      modules = [ settings ];
    }).config;

  flakePartsWith =
    settings:
    (evalHead {
      head = flakePartsHead;
      stub = flakePartsStub;
      modules = [ settings ];
    }).config;

  failures = config: map (a: a.message) (lib.filter (a: !a.assertion) config.assertions);

  failsWith =
    config: needle:
    let
      messages = failures config;
    in
    messages != [ ] && lib.any (m: lib.hasInfix needle m) messages;

  enabledSystem = {
    programs.xi.enable = true;
    programs.xi.sources.enable = true;
    programs.xi.sources.remote = "git@github.com:you/nix-config.git";
  };

  enabledUser = {
    programs.xi.enable = true;
    programs.xi.sources.enable = true;
    programs.xi.sources.remote = "git@github.com:you/dotfiles.git";
  };

  systemHappy = nixosWith (
    lib.recursiveUpdate enabledSystem {
      programs.xi.sources.pollInterval = "5m";
      programs.xi.sources.bootstrap.installer = "lix";
    }
  );

  userHappy = homeManagerWith (
    lib.recursiveUpdate enabledUser { programs.xi.sources.workingDir = "/home/anon/src/dotfiles"; }
  );

  projectHappy = flakePartsWith {
    xi.enable = true;
    xi.sources.enable = true;
    xi.sources.remote = "git@github.com:you/nix-config.git";
    xi.sources.syncMode = "never";
    xi.sources.bootstrap.installer = "detsys";
  };

  userActivation = userHappy.home.activation.xiSourcesDirs.data;
  projectSources = projectHappy.xi.settings.sources;

  cases = [
    {
      name = "system scope refuses workingDir";
      ok = failsWith (nixosWith (
        lib.recursiveUpdate enabledSystem { programs.xi.sources.workingDir = "/root/dotfiles"; }
      )) "user scope only";
    }

    {
      name = "requireSigned without a trust root is refused at system scope";
      ok = failsWith (nixosWith (
        lib.recursiveUpdate enabledSystem { programs.xi.sources.requireSigned = true; }
      )) "needs a trust root";
    }

    {
      name = "requireSigned without a trust root is refused at user scope";
      ok = failsWith (homeManagerWith (
        lib.recursiveUpdate enabledUser { programs.xi.sources.requireSigned = true; }
      )) "needs a trust root";
    }

    {
      name = "gpgPublicKeys alone satisfies requireSigned";
      ok =
        failures (nixosWith (
          lib.recursiveUpdate enabledSystem {
            programs.xi.sources.requireSigned = true;
            programs.xi.sources.gpgPublicKeys = [ (repoRoot + "/flake.nix") ];
          }
        )) == [ ];
    }

    {
      name = "dual-scope gitops on the same remote is refused";
      ok = failsWith (nixosWith (
        lib.recursiveUpdate enabledSystem {
          programs.xi.sources.gitops.enable = true;
          home-manager.users.anon.programs.xi.sources = {
            enable = true;
            remote = "git@github.com:you/nix-config.git";
            gitops.enable = true;
          };
        }
      )) "Two daemons polling the same remote";
    }

    {
      name = "dual-scope gitops on different remotes is accepted";
      ok =
        failures (nixosWith (
          lib.recursiveUpdate enabledSystem {
            programs.xi.sources.gitops.enable = true;
            home-manager.users.anon.programs.xi.sources = {
              enable = true;
              remote = "git@github.com:you/dotfiles.git";
              gitops.enable = true;
            };
          }
        )) == [ ];
    }

    {
      name = "system head asserts nothing on the happy path";
      ok = failures systemHappy == [ ];
    }

    {
      name = "system head creates the sources and state dirs 0700 root:root";
      ok = lib.all (rule: lib.elem rule systemHappy.systemd.tmpfiles.rules) [
        "d /var/lib/xi/sources 0700 root root -"
        "d /var/lib/xi/state 0700 root root -"
      ];
    }

    {
      name = "user head creates the XDG dirs with mode 700";
      ok =
        lib.hasInfix "mkdir -p '/home/anon/.local/share/xi/sources' '/home/anon/.local/state/xi'"
          userActivation
        && lib.hasInfix "chmod 700 '/home/anon/.local/share/xi/sources' '/home/anon/.local/state/xi'"
          userActivation;
    }

    {
      name = "user head runs after the home-manager write boundary";
      ok = userHappy.home.activation.xiSourcesDirs.after == [ "writeBoundary" ];
    }

    {
      name = "user head renders the XDG sources.toml";
      ok = userHappy.xdg.configFile ? "xi/sources.toml";
    }

    {
      name = "flake-parts head folds kebab-case keys into the wrapper settings";
      ok =
        projectSources == {
          remote = "git@github.com:you/nix-config.git";
          sync-mode = "never";
          installer = "detsys";
        };
    }
  ];

  broken = lib.filter (case: !case.ok) cases;
in
{
  perSystem =
    { pkgs, ... }:
    {
      checks = {
        # SC-N-001: the three heads expose the same option tree.
        xi-sources-option-parity =
          pkgs.runCommand "xi-sources-option-parity"
            (lib.mapAttrs' (name: head: lib.nameValuePair "tree_${lib.replaceStrings [ "-" ] [ "_" ] name}" (
              treeOf head
            )) heads)
            ''
              printf '%s' "$tree_nixos" > nixos.txt
              printf '%s' "$tree_home_manager" > home-manager.txt
              printf '%s' "$tree_flake_parts" > flake-parts.txt

              status=0
              for head in home-manager flake-parts; do
                if ! diff -u nixos.txt "$head.txt"; then
                  echo "xi.sources option tree drifted between nixos and $head" >&2
                  status=1
                fi
              done

              if [ "$status" -ne 0 ]; then
                echo "the tree lives in modules/shared/types/sources.nix; a head must not redeclare it" >&2
                exit 1
              fi

              cp nixos.txt "$out"
            '';

        # SC-N-002 / SC-N-003 / SC-N-005: scope assertions and the
        # activation surface of each head, evaluated against stubs.
        xi-sources-module-fixtures =
          pkgs.runCommand "xi-sources-module-fixtures"
            {
              sourcesToml = systemHappy.environment.etc."xi/sources.toml".source;
              brokenCases = lib.concatMapStringsSep "\n" (case: case.name) broken;
            }
            ''
              if [ -n "$brokenCases" ]; then
                echo "xi.sources module fixtures failed:" >&2
                printf '  - %s\n' "$brokenCases" >&2
                exit 1
              fi

              # SC-N-004: the rendered file is a snapshot of the options,
              # spelled in the kebab-case keys xi's parser accepts.
              grep -q '^poll-interval = "5m"$' "$sourcesToml"
              grep -q '^installer = "lix"$' "$sourcesToml"
              grep -q '^\[sources\]$' "$sourcesToml"

              # Options the tree declares but xi's closed key set does not
              # accept yet must stay out of the file, or loading it is an
              # error rather than a no-op.
              ! grep -q 'gitops\|secrets\|credentials\|prestage' "$sourcesToml"

              cp "$sourcesToml" "$out"
            '';
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        # SC-N-002: the real thing, in a VM, through the real NixOS
        # module system rather than the stubs above.
        xi-sources-nixos = pkgs.testers.runNixOSTest {
          name = "xi-sources-nixos";

          nodes.machine = {
            imports = [ self.nixosModules.default ];

            programs.xi = {
              enable = true;
              sources = {
                enable = true;
                remote = "https://example.invalid/nix-config.git";
                pollInterval = "5m";
              };
            };
          };

          testScript = ''
            machine.wait_for_unit("multi-user.target")

            # The dirs are activation output, so they are there before
            # anything xi-related could have created them on demand.
            machine.succeed("test -d /var/lib/xi/sources")
            machine.succeed("test -d /var/lib/xi/state")
            machine.succeed("test 700 = $(stat -c %a /var/lib/xi/sources)")
            machine.succeed("test 700 = $(stat -c %a /var/lib/xi/state)")
            machine.succeed("test root:root = $(stat -c %U:%G /var/lib/xi/sources)")
            machine.succeed("test root:root = $(stat -c %U:%G /var/lib/xi/state)")

            machine.succeed("test -f /etc/xi/sources.toml")
            machine.succeed(
                "grep -q '^remote = \"https://example.invalid/nix-config.git\"$' /etc/xi/sources.toml"
            )
            machine.succeed("grep -q '^poll-interval = \"5m\"$' /etc/xi/sources.toml")
          '';
        };
      };
    };
}
