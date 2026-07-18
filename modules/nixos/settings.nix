{
  config,
  lib,
  pkgs,
  ...
}:
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
}
