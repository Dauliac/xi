{ config, lib, ... }:
{
  options._xi.lib = {
    mkWrappedPackage = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Build a bash wrapper that sets XI_CONFIG and execs the real xi binary.";
    };

    mkToolPackages = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Collect enabled tool packages (nom, nix-fast-build) from cfg.";
    };

    mkFinalPackage = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Produce the final xi package, optionally wrapped with config.";
    };
  };

  config._xi.lib = {
    mkWrappedPackage =
      {
        pkgs,
        package,
        configFile ? null,
        binPath ? null,
        toolPackages ? [ ],
      }:
      let
        storeBin = lib.getExe package;
        pathPrefix = lib.optionalString (toolPackages != [ ]) ''
          export PATH="${lib.makeBinPath toolPackages}:$PATH"
        '';
        configExport = lib.optionalString (configFile != null) ''
          export XI_CONFIG=${configFile}
        '';
        wrapperBody =
          if binPath != null then
            ''
              ${pathPrefix}${configExport}exec ${binPath} "$@"
            ''
          else
            ''
              ${pathPrefix}${configExport}exec ${storeBin} "$@"
            '';
        wrapper = pkgs.writeShellScriptBin "xi" wrapperBody;
      in
      if binPath != null then
        # When binPath is set, only ship the wrapper script — don't pull
        # the store package into PATH via symlinkJoin.
        pkgs.symlinkJoin {
          name = "xi-wrapped";
          paths = [ wrapper ];
          meta.mainProgram = "xi";
        }
      else
        # No binPath: symlink the real package so completions/man pages
        # are available, and the wrapper shadows bin/xi.
        pkgs.symlinkJoin {
          name = "xi-wrapped";
          paths = [
            wrapper
            package
          ];
          meta.mainProgram = "xi";
        };

    ## Collect enabled tool packages from cfg.
    mkToolPackages =
      cfg:
      builtins.filter (p: p != null) [
        (if (cfg.nom.enable or false) then (cfg.nom.package or null) else null)
        (if (cfg.nixFastBuild.enable or false) then (cfg.nixFastBuild.package or null) else null)
        (if (cfg.test.nixUnit.enable or false) then (cfg.test.nixUnit.package or null) else null)
        (if (cfg.test.nixt.enable or false) then (cfg.test.nixt.package or null) else null)
        (if (cfg.test.namaka.enable or false) then (cfg.test.namaka.package or null) else null)
        (if (cfg.fmt.alejandra.enable or false) then (cfg.fmt.alejandra.package or null) else null)
        (if (cfg.fmt.treefmt.enable or false) then (cfg.fmt.treefmt.package or null) else null)
      ];

    mkFinalPackage =
      { pkgs, cfg }:
      let
        configFile = config._xi.lib.mkConfigFile {
          inherit pkgs;
          inherit (cfg) settings;
        };
        toolPackages = config._xi.lib.mkToolPackages cfg;
        hasConfig = configFile != null;
        hasTools = toolPackages != [ ];
        needsWrapper = cfg.wrapper.enable && (hasConfig || cfg.binPath != null || hasTools);
      in
      if needsWrapper then
        config._xi.lib.mkWrappedPackage {
          inherit pkgs configFile toolPackages;
          inherit (cfg) package binPath;
        }
      else
        cfg.package;
  };
}
