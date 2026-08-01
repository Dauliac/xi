{ lib, ... }:
{
  options._xi.types.shellHook = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Shell hook configuration submodule type.";
  };

  config._xi.types.shellHook = lib.types.submodule {
    options = {
      enable = lib.mkEnableOption "xi shell hook";

      nixAlias = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Alias the nix command to xi nix for enhanced UX.";
      };

      commaAlias = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Install a `,` shell alias mapped to `xi run`, giving comma-style
          package execution (e.g. `, cowsay hello`).

          `xi run` walks a three-tier chain (local flake apps → flake's
          locked nixpkgs → system nixpkgs), so `,` inside a project uses
          reproducible resolution and only falls back to the rolling
          registry when nothing else can satisfy the request.

          Off by default because a `,` alias conflicts with the standalone
          `comma` package if you also have it installed.
        '';
      };

      completion = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Register xi shell completions via eval at shell init.";
      };

      develop = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Install the xi develop shell hook.
          This adds a prompt hook (precmd/PROMPT_COMMAND/fish_prompt) that
          automatically detects flake directories and spawns devshell subshells.
          Equivalent to adding `eval "$(xi develop activate <shell>)"` to your
          shell init.
        '';
      };
    };
  };
}
