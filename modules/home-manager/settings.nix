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
          cache.cachix.push_command = ["cachix" "push" "mycache"];
        };
    '';
  };

  # The settings-config counterpart to the NixOS/system-manager head's
  # `environment.etc."xi/config.toml"` (same symmetry, same reasoning).
  # Lands at `$XDG_CONFIG_HOME/xi/config.toml` (usually
  # `~/.config/xi/config.toml`), which happens to be exactly
  # `default_config_path()`'s own non-`XI_CONFIG` fallback
  # (`crates/xi/src/config.rs`) - so unlike the system-scope `/etc`
  # file, this one is not purely cosmetic: an unwrapped `xi` run as
  # this user picks it up on its own, no `programs.xi.wrapper.enable`
  # required.
  config = lib.mkIf (cfg.enable && configFile != null) {
    xdg.configFile."xi/config.toml".source = configFile;
  };
}
