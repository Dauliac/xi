{ lib, ... }:
{
  options._xi.types.sources = lib.mkOption {
    type = lib.types.raw;
    internal = true;
    description = "Config-repo sources submodule type (xi sources).";
  };

  # One definition shared by the nixos, home-manager and flake-parts
  # modules. The three surfaces differ only in WHERE the generated
  # sources.toml lands, never in which keys exist, so the option tree
  # lives here and cannot drift between them.
  #
  # Option names are camelCase per nixpkgs convention; `mkSourcesFile`
  # maps them back to the kebab-case keys xi's parser accepts.
  config._xi.types.sources = lib.types.submodule {
    options = {
      enable = lib.mkEnableOption ''
        the xi sources model: a managed clone of your config repo, so
        `xi <scope> switch` works from any directory on the machine
      '';

      remote = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "git@github.com:you/dotfiles.git";
        description = ''
          Remote git URL of the config repo. Accepts an `https://` or
          `ssh://` URL, git's scp-style `host:path`, or an absolute
          local path.
        '';
      };

      branch = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "main";
        description = ''
          Branch to track. Leave null to use xi's built-in default
          (`main`), so the CLI flag and `XI_SOURCES_BRANCH` can still
          win without this module pinning a value.
        '';
      };

      workingDir = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        example = "/home/you/src/dotfiles";
        description = ''
          Absolute path to your editable clone. Recorded so a `switch`
          started from an unrelated directory can still find it.
        '';
      };

      pollInterval = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "5m";
        description = ''
          How often the gitops daemon re-checks the remote, as a
          duration string (`60s`, `5m`). Defaults to `60s`.
        '';
      };

      requireSigned = lib.mkOption {
        type = lib.types.nullOr lib.types.bool;
        default = null;
        description = ''
          Refuse to advance past unsigned commits. Requires
          `allowedSigners`.
        '';
      };

      allowedSigners = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = ''
          Absolute path to an SSH `allowed_signers` file. Required when
          `requireSigned` is true: verifying against an empty trust root
          is worse than not verifying, because it looks safe.
        '';
      };

      scope = lib.mkOption {
        type = lib.types.nullOr (
          lib.types.enum [
            "system"
            "user"
          ]
        );
        default = null;
        description = ''
          Disambiguates when both scopes are configured and the command
          runs as root. Usually inferred and left null.
        '';
      };

      syncMode = lib.mkOption {
        type = lib.types.nullOr (
          lib.types.enum [
            "prompt"
            "auto-copy"
            "never"
          ]
        );
        default = null;
        description = ''
          What to do when the working dir and the sources dir disagree.
          Defaults to `prompt`.
        '';
      };

      installer = lib.mkOption {
        type = lib.types.nullOr (
          lib.types.enum [
            "detsys"
            "lix"
            "none"
          ]
        );
        default = null;
        description = ''
          Which Nix installer a bootstrap run should use. Left null so
          `xi sources bootstrap` picks from the resolved flake's
          substituters.
        '';
      };
    };
  };
}
