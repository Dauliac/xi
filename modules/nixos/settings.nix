{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.xi;
  configFile = config.lib.mkConfigFile {
    inherit pkgs;
    inherit (cfg) settings;
  };
in
{
  options.programs.xi.settings = lib.mkOption {
    type = (config._xi.types.settings pkgs);
    default = { };
    description = ''
      Freeform xi configuration written as Nix attributes.
      Converted to config.toml and baked into the wrapper.

      Corresponds to the [build], [cache], [develop], etc. sections
      of xi's config.toml.  Any key supported by xi can be set here.

      Note: build.nom and build.ci_backend are auto-managed by
      programs.xi.nom.enable and programs.xi.nixFastBuild.enable.
      Only set them here to override the auto-generated values.

      Example:
        programs.xi.settings = {
          build.keep_going = true;
          cache.my-s3.push_url = "s3://bucket?region=eu-west-3";
          cache.my-s3.signing_key = "/path/to/key";
          cache.cachix.push_command = ["cachix" "push" "mycache"];
        };
    '';
  };

  # The same rendered config.toml the wrapper bakes into `XI_CONFIG`
  # (`modules/shared/lib/wrapper.nix`'s `mkFinalPackage`), also placed
  # at a fixed, inspectable path - the settings-config counterpart to
  # `environment.etc."xi/sources.toml"` in sources.nix. `xi` itself
  # never reads this path directly (it is a declarative read-only
  # symlink into the store, same as sources.toml), so this is purely
  # for discoverability and symmetry: `cat /etc/xi/config.toml` answers
  # "what cache/build settings is this host actually running with"
  # without having to inspect the wrapper script. Not conditioned on
  # `programs.xi.wrapper.enable`: this reflects the Nix-level config
  # regardless of whether anything bakes it into a binary.
  config = lib.mkIf (cfg.enable && configFile != null) {
    environment.etc."xi/config.toml".source = configFile;
  };
}
