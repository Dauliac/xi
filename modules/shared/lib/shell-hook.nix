{ config, lib, ... }:
{
  nix-lib.lib.mkComposedShellHook = {
    type = lib.types.raw;
    fn =
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
        xiBin = config.lib.resolveXiBin binArgs;
        nixBin = "${nixPackage}/bin/nix";
        nixWrapper = config.lib.mkNixWrapperPackage (binArgs // { inherit pkgs nixPackage; });
      in
      lib.concatStrings [
        ''
          export XI_NIX_BIN="${nixBin}"
        ''
        (lib.optionalString shellHook.nixAlias ''
          export PATH="${nixWrapper}/bin:$PATH"
        '')
        (lib.optionalString shellHook.commaAlias ''
          alias ,='${xiBin} run'
        '')
        (lib.optionalString shellHook.completion (config.lib.mkCompletionShellHook binArgs))
        (lib.optionalString shellHook.develop ''
          if [ -n "''${ZSH_VERSION-}" ]; then
            eval "$(${xiBin} develop activate zsh 2>/dev/null)"
          elif [ -n "''${BASH_VERSION-}" ]; then
            eval "$(${xiBin} develop activate bash 2>/dev/null)"
          fi
        '')
      ];
    description = "Compose a posix shell hook from shellHook sub-options.";
  };

  nix-lib.lib.mkComposedFishShellHook = {
    type = lib.types.raw;
    fn =
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
        xiBin = config.lib.resolveXiBin binArgs;
        nixBin = "${nixPackage}/bin/nix";
        nixWrapper = config.lib.mkNixWrapperPackage (binArgs // { inherit pkgs nixPackage; });
      in
      lib.concatStrings [
        ''
          set -gx XI_NIX_BIN "${nixBin}"
        ''
        (lib.optionalString shellHook.nixAlias ''
          set -p PATH "${nixWrapper}/bin"
        '')
        (lib.optionalString shellHook.commaAlias ''
          alias , '${xiBin} run'
        '')
        (lib.optionalString shellHook.completion (config.lib.mkFishCompletionScript binArgs))
        (lib.optionalString shellHook.develop ''
          ${xiBin} develop activate fish 2>/dev/null | source
        '')
      ];
    description = "Compose a fish shell hook from shellHook sub-options.";
  };

  nix-lib.lib.mkCompletionShellHook = {
    type = lib.types.raw;
    fn =
      { package, binPath }:
      let
        xiBin = config.lib.resolveXiBin { inherit package binPath; };
      in
      ''
        if [ -n "''${ZSH_VERSION-}" ]; then
          eval "$(${xiBin} completions zsh 2>/dev/null)"
        elif [ -n "''${BASH_VERSION-}" ]; then
          eval "$(${xiBin} completions bash 2>/dev/null)"
        fi
      '';
    description = "Posix-compatible completion snippet that auto-detects bash/zsh.";
  };

  nix-lib.lib.mkBashCompletionScript = {
    type = lib.types.raw;
    fn =
      { package, binPath }:
      let
        xiBin = config.lib.resolveXiBin { inherit package binPath; };
      in
      ''
        eval "$(${xiBin} completions bash 2>/dev/null)"
      '';
    description = "Bash-specific completion eval script.";
  };

  nix-lib.lib.mkZshCompletionScript = {
    type = lib.types.raw;
    fn =
      { package, binPath }:
      let
        xiBin = config.lib.resolveXiBin { inherit package binPath; };
      in
      ''
        eval "$(${xiBin} completions zsh 2>/dev/null)"
      '';
    description = "Zsh-specific completion eval script.";
  };

  nix-lib.lib.mkFishCompletionScript = {
    type = lib.types.raw;
    fn =
      { package, binPath }:
      let
        xiBin = config.lib.resolveXiBin { inherit package binPath; };
      in
      ''
        ${xiBin} completions fish 2>/dev/null | source
      '';
    description = "Fish-specific completion eval script.";
  };
}
