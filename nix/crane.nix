{
  inputs,
  self,
  lib,
  ...
}:
let
  inherit (lib) mkOption types;
in
{
  perSystem =
    { config, pkgs, ... }:
    let
      cfg = config._xi;
      craneLib = inputs.crane.mkLib pkgs;

      rev = self.shortRev or self.dirtyShortRev or "dirty";
      cargoToml = lib.importTOML cfg.workspace.root;
      version = "${cargoToml.workspace.package.version}-${rev}";

      # ── Auto-discovery from Cargo.toml ─────────────────────────────
      cratesDir = builtins.dirOf cfg.workspace.root + "/crates";
      crateDirEntries = builtins.readDir cratesDir;
      allMemberNames = builtins.attrNames (
        lib.filterAttrs (_: type: type == "directory") crateDirEntries
      );

      discoveredDeps = builtins.listToAttrs (
        map (
          name:
          let
            manifest = cratesDir + "/${name}/Cargo.toml";
            parsed =
              if builtins.pathExists manifest then builtins.fromTOML (builtins.readFile manifest) else { };
            allDeps =
              (parsed.dependencies or { })
              // (parsed.dev-dependencies or { })
              // (parsed.build-dependencies or { });
          in
          {
            inherit name;
            value = builtins.filter (dep: builtins.elem dep allMemberNames) (builtins.attrNames allDeps);
          }
        ) allMemberNames
      );

      libMemberNames = builtins.filter (
        name: !(builtins.elem name cfg.workspace.excludeCrates) && name != cfg.workspace.binaryCrate
      ) allMemberNames;

      # ── Topological sort (Kahn's) ──────────────────────────────────
      libMemberDeps = lib.filterAttrs (name: _: builtins.elem name libMemberNames) discoveredDeps;

      topoSort =
        let
          go =
            remaining: resolved:
            if remaining == { } then
              [ ]
            else
              let
                ready = lib.filterAttrs (_: deps: builtins.all (d: builtins.elem d resolved) deps) remaining;
                readyNames = builtins.attrNames ready;
                rest = builtins.removeAttrs remaining readyNames;
              in
              readyNames ++ (go rest (resolved ++ readyNames));
        in
        go libMemberDeps [ ];

      topoIndex = builtins.listToAttrs (lib.imap0 (i: name: lib.nameValuePair name i) topoSort);

      # ── Transitive deps ────────────────────────────────────────────
      transitiveDeps =
        memberName:
        let
          go =
            acc: toProcess:
            if toProcess == [ ] then
              acc
            else
              let
                current = builtins.head toProcess;
                rest = builtins.tail toProcess;
                newDeps = builtins.filter (d: !(builtins.elem d acc)) (discoveredDeps.${current} or [ ]);
              in
              go (acc ++ [ current ]) (rest ++ newDeps);
        in
        go [ ] (discoveredDeps.${memberName} or [ ]);

      # ── Per-member source filtering ────────────────────────────────
      mkMemberSrc =
        memberName:
        let
          fullSourceMembers = [ memberName ] ++ (transitiveDeps memberName);
          memberFilesets = map (name: cratesDir + "/${name}") fullSourceMembers;

          stubMembers = builtins.filter (
            name:
            !(builtins.elem name fullSourceMembers)
            && name != cfg.workspace.binaryCrate
            && !(builtins.elem name cfg.workspace.excludeCrates)
          ) allMemberNames;

          mkStubs =
            names:
            lib.concatMap (
              name:
              let
                toml = cratesDir + "/${name}/Cargo.toml";
                libRs = cratesDir + "/${name}/src/lib.rs";
                mainRs = cratesDir + "/${name}/src/main.rs";
              in
              [ toml ]
              ++ lib.optional (builtins.pathExists libRs) libRs
              ++ lib.optional (builtins.pathExists mainRs) mainRs
            ) names;

          rootDir = builtins.dirOf cfg.workspace.root;
        in
        lib.fileset.toSource {
          root = rootDir;
          fileset = lib.fileset.unions (
            [
              (rootDir + "/.cargo")
              (rootDir + "/.config")
              cfg.workspace.root
              (rootDir + "/Cargo.lock")
            ]
            ++ memberFilesets
            ++ (mkStubs stubMembers)
            ++ (mkStubs [ cfg.workspace.binaryCrate ] ++ (mkStubs cfg.workspace.excludeCrates))
          );
        };

      # ── Common build args ──────────────────────────────────────────
      fullSrc = lib.fileset.toSource {
        root = builtins.dirOf cfg.workspace.root;
        fileset =
          lib.fileset.intersection
            (lib.fileset.fromSource (lib.sources.cleanSource (builtins.dirOf cfg.workspace.root)))
            (
              lib.fileset.unions [
                (builtins.dirOf cfg.workspace.root + "/.cargo")
                (builtins.dirOf cfg.workspace.root + "/.config")
                cratesDir
                cfg.workspace.root
                (builtins.dirOf cfg.workspace.root + "/Cargo.lock")
              ]
            );
      };

      # ── Nix CLI schema (for nix-command build.rs) ──────────────────
      nixCliSchema =
        pkgs.runCommand "nix-cli-schema-${cfg.workspace.nixPackage.version or "unknown"}"
          {
            nativeBuildInputs = [ cfg.workspace.nixPackage ];
          }
          ''
            nix __dump-cli > $out
          '';

      commonArgs = {
        src = fullSrc;
        inherit version;
        pname = cfg.workspace.binaryCrate;
        strictDeps = true;
        nativeBuildInputs = cfg.workspace.nativeBuildInputs;
        buildInputs =
          cfg.workspace.buildInputs ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
        env.XI_REV = rev;
        env.NIX_CLI_SCHEMA_PATH = "${nixCliSchema}";
      };

      # ── Artifact providers (DAG computation) ───────────────────────
      artifactProvider =
        memberName:
        let
          directDeps = builtins.filter (dep: builtins.elem dep topoSort) (
            discoveredDeps.${memberName} or [ ]
          );
          sorted = lib.sort (a: b: (topoIndex.${a} or 0) > (topoIndex.${b} or 0)) directDeps;
        in
        if sorted == [ ] then null else builtins.head sorted;

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      cargoArtifactsCheck = craneLib.buildDepsOnly (
        commonArgs
        // {
          pname = "${cfg.workspace.binaryCrate}-deps-check";
          cargoCheckExtraArgs = "--all-targets";
        }
      );

      builtMembers = builtins.foldl' (
        acc: memberName:
        let
          provider = artifactProvider memberName;
          prevArtifacts = if provider != null then acc.${provider} else cargoArtifacts;
        in
        acc
        // {
          ${memberName} = craneLib.cargoBuild (
            commonArgs
            // {
              cargoArtifacts = prevArtifacts;
              src = mkMemberSrc memberName;
              pname = "${memberName}-build";
              cargoExtraArgs = "-p ${memberName}";
              doCheck = false;
              doInstallCargoArtifacts = true;
            }
          );
        }
      ) { } topoSort;

      lastMember = lib.last topoSort;

      # ── Test skip filter ───────────────────────────────────────────
      allTestSkips =
        cfg.workspace.testSkips
        ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin cfg.workspace.darwinTestSkips;

      nextestFilterExpr =
        if allTestSkips == [ ] then
          ""
        else
          let
            exprs = map (t: "not test(=${t})") allTestSkips;
          in
          "-E '${builtins.concatStringsSep " and " exprs}'";

      # ── Per-member test derivations ────────────────────────────────
      memberTests = builtins.listToAttrs (
        map (
          memberName:
          lib.nameValuePair "test-${memberName}" (
            craneLib.cargoNextest (
              commonArgs
              // {
                cargoArtifacts = cargoArtifactsCheck;
                src = mkMemberSrc memberName;
                pname = "${cfg.workspace.binaryCrate}-test-${memberName}";
                cargoNextestExtraArgs = "-p ${memberName} ${nextestFilterExpr}";
                nativeCheckInputs = lib.optionals (!pkgs.stdenv.hostPlatform.isDarwin) [ pkgs.sudo ];
              }
            )
          )
        ) topoSort
      );
    in
    {
      # ── Options (declarative interface) ────────────────────────────
      options._xi = {
        workspace = {
          root = mkOption {
            type = types.path;
            description = "Path to the workspace Cargo.toml";
          };
          binaryCrate = mkOption {
            type = types.str;
            description = "Name of the main binary crate";
          };
          excludeCrates = mkOption {
            type = types.listOf types.str;
            default = [ ];
            description = "Crate names to exclude from the build graph";
          };
          buildInputs = mkOption {
            type = types.listOf types.raw;
            default = [ ];
            description = "Build inputs for all crates";
          };
          nativeBuildInputs = mkOption {
            type = types.listOf types.raw;
            default = [ ];
            description = "Native build inputs for all crates";
          };
          testSkips = mkOption {
            type = types.listOf types.str;
            default = [ ];
            description = "Test names to skip in sandbox";
          };
          darwinTestSkips = mkOption {
            type = types.listOf types.str;
            default = [ ];
            description = "Additional test names to skip on Darwin";
          };
          nixPackage = mkOption {
            type = types.package;
            default = pkgs.nix;
            description = "Nix package used to generate the CLI flag schema at build time.";
          };
        };

        # Computed outputs (read-only)
        craneLib = mkOption {
          type = types.raw;
          readOnly = true;
        };
        commonArgs = mkOption {
          type = types.raw;
          readOnly = true;
        };
        cargoArtifacts = mkOption {
          type = types.raw;
          readOnly = true;
        };
        cargoArtifactsCheck = mkOption {
          type = types.raw;
          readOnly = true;
        };
        builtMembers = mkOption {
          type = types.raw;
          readOnly = true;
        };
        xiBinaryArtifacts = mkOption {
          type = types.raw;
          readOnly = true;
        };
        memberTests = mkOption {
          type = types.raw;
          readOnly = true;
        };
        libMembers = mkOption {
          type = types.listOf types.str;
          readOnly = true;
        };
        nextestFilterExpr = mkOption {
          type = types.str;
          readOnly = true;
        };
        mkMemberSrc = mkOption {
          type = types.raw;
          readOnly = true;
        };
      };

      # ── Config (computed from options) ─────────────────────────────
      config._xi = {
        inherit
          craneLib
          commonArgs
          cargoArtifacts
          cargoArtifactsCheck
          builtMembers
          memberTests
          nextestFilterExpr
          mkMemberSrc
          ;
        xiBinaryArtifacts = builtMembers.${lastMember};
        libMembers = topoSort;
      };
    };
}
