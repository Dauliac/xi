{ config, lib, ... }:
let
  resolveXiBin = config._xi.lib.resolveXiBin;
in
{
  options._xi.lib = {
    mkComposedShellHook = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Compose a posix shell hook from shellHook sub-options.";
    };

    mkComposedFishShellHook = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Compose a fish shell hook from shellHook sub-options.";
    };

    mkCompletionShellHook = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Posix-compatible completion snippet that auto-detects bash/zsh.";
    };

    mkBashCompletionScript = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Bash-specific completion eval script.";
    };

    mkZshCompletionScript = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Zsh-specific completion eval script.";
    };

    mkFishCompletionScript = lib.mkOption {
      type = lib.types.raw;
      internal = true;
      description = "Fish-specific completion eval script.";
    };
  };

  config._xi.lib = {
    mkComposedShellHook =
      {
        pkgs,
        package,
        shellHook,
        binPath,
        nixPackage,
      }:
      let
        binArgs = {
          inherit package binPath;
        };
        xiBin = resolveXiBin binArgs;
        nixBin = "${nixPackage}/bin/nix";
        nixWrapper = config._xi.lib.mkNixWrapperPackage (binArgs // { inherit pkgs nixPackage; });
      in
      lib.concatStrings [
        # Always export XI_NIX_BIN so xi commands find the real nix binary
        # even when called directly (not through the wrapper).
        ''
          export XI_NIX_BIN="${nixBin}"
        ''
        (lib.optionalString shellHook.nixAlias ''
          export PATH="${nixWrapper}/bin:$PATH"
        '')
        (lib.optionalString shellHook.completion (config._xi.lib.mkCompletionShellHook binArgs))
        (lib.optionalString shellHook.develop ''
          if [ -n "''${ZSH_VERSION-}" ]; then
            eval "$(${xiBin} develop activate zsh 2>/dev/null)"
          elif [ -n "''${BASH_VERSION-}" ]; then
            eval "$(${xiBin} develop activate bash 2>/dev/null)"
          fi
        '')
      ];

    mkComposedFishShellHook =
      {
        pkgs,
        package,
        shellHook,
        binPath,
        nixPackage,
      }:
      let
        binArgs = {
          inherit package binPath;
        };
        xiBin = resolveXiBin binArgs;
        nixBin = "${nixPackage}/bin/nix";
        nixWrapper = config._xi.lib.mkNixWrapperPackage (binArgs // { inherit pkgs nixPackage; });
      in
      lib.concatStrings [
        ''
          set -gx XI_NIX_BIN "${nixBin}"
        ''
        (lib.optionalString shellHook.nixAlias ''
          set -p PATH "${nixWrapper}/bin"
        '')
        (lib.optionalString shellHook.completion (config._xi.lib.mkFishCompletionScript binArgs))
        (lib.optionalString shellHook.develop ''
          ${xiBin} develop activate fish 2>/dev/null | source
        '')
      ];

    mkCompletionShellHook =
      { package, binPath }:
      let
        xiBin = resolveXiBin { inherit package binPath; };
      in
      ''
        if [ -n "''${ZSH_VERSION-}" ]; then
          eval "$(${xiBin} completions zsh 2>/dev/null)"
        elif [ -n "''${BASH_VERSION-}" ]; then
          eval "$(${xiBin} completions bash 2>/dev/null)"
        fi
      '';

    mkBashCompletionScript =
      { package, binPath }:
      let
        xiBin = resolveXiBin { inherit package binPath; };
      in
      ''
        eval "$(${xiBin} completions bash 2>/dev/null)"
      '';

    mkZshCompletionScript =
      { package, binPath }:
      let
        xiBin = resolveXiBin { inherit package binPath; };
      in
      ''
        eval "$(${xiBin} completions zsh 2>/dev/null)"
      '';

    mkFishCompletionScript =
      { package, binPath }:
      let
        xiBin = resolveXiBin { inherit package binPath; };
      in
      ''
        ${xiBin} completions fish 2>/dev/null | source
      '';
  };
}
