{
  ...
}:
{
  perSystem =
    { pkgs, ... }:
    {
      _xi.workspace = {
        root = ../Cargo.toml;
        binaryCrate = "xi";
        excludeCrates = [ "xtask" ];

        nativeBuildInputs = with pkgs; [
          installShellFiles
          makeBinaryWrapper
          pkg-config
        ];

        buildInputs = [ pkgs.openssl ];

        testSkips = [
          "test_get_build_image_variants_expression"
          "test_get_build_image_variants_file"
          "test_get_build_image_variants_flake"
        ];

        darwinTestSkips = [
          "test_build_sudo_cmd_basic"
          "test_build_sudo_cmd_with_preserve_vars"
          "test_build_sudo_cmd_with_preserve_vars_disabled"
          "test_build_sudo_cmd_with_set_vars"
          "test_build_sudo_cmd_force_no_stdin"
          "test_build_sudo_cmd_with_remove_vars"
          "test_build_sudo_cmd_with_askpass"
          "test_build_sudo_cmd_env_added_once"
          "test_elevation_strategy_passwordless_resolves"
          "test_build_sudo_cmd_with_nix_config_spaces"
        ];
      };
    };
}
