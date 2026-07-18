{ config, lib, ... }:
{
  nix-lib.lib.mkWrappedPackage = {
    type = lib.types.raw;
    fn =
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
        pkgs.symlinkJoin {
          name = "xi-wrapped";
          paths = [ wrapper ];
          meta.mainProgram = "xi";
        }
      else
        pkgs.symlinkJoin {
          name = "xi-wrapped";
          paths = [
            wrapper
            package
          ];
          meta.mainProgram = "xi";
        };
    description = "Build a bash wrapper that sets XI_CONFIG and execs the real xi binary.";
  };

  nix-lib.lib.mkToolPackages = {
    type = lib.types.raw;
    fn =
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
    description = "Collect enabled tool packages (nom, nix-fast-build) from cfg.";
  };

  nix-lib.lib.mkFinalPackage = {
    type = lib.types.raw;
    fn =
      { pkgs, cfg }:
      let
        configFile = config.lib.mkConfigFile {
          inherit pkgs;
          inherit (cfg) settings;
        };
        toolPackages = config.lib.mkToolPackages cfg;
        hasConfig = configFile != null;
        hasTools = toolPackages != [ ];
        needsWrapper = cfg.wrapper.enable && (hasConfig || cfg.binPath != null || hasTools);
      in
      if needsWrapper then
        config.lib.mkWrappedPackage {
          inherit pkgs configFile toolPackages;
          inherit (cfg) package binPath;
        }
      else
        cfg.package;
    description = "Produce the final xi package, optionally wrapped with config.";
  };
}
