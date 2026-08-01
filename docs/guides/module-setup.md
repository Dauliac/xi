# How to Set Up Xi Modules

Xi provides NixOS, Home Manager, and flake-parts modules that handle wrapping,
configuration generation, shell hooks, and tool injection.

## NixOS module

Add xi as a flake input and import the module:

```nix
# flake.nix
{
  inputs.xi.url = "github:Dauliac/xi";

  outputs = { nixpkgs, xi, ... }: {
    nixosConfigurations.myHost = nixpkgs.lib.nixosSystem {
      modules = [
        xi.nixosModules.default
        ./configuration.nix
      ];
    };
  };
}
```

Then in your configuration:

```nix
# configuration.nix
{
  programs.xi = {
    enable = true;
    flake = "/home/user/nixos-config";  # sets XI_OS_FLAKE

    # Optional: automated garbage collection
    clean.enable = true;
    clean.extraArgs = "--keep-since 4d --keep 3";
  };
}
```

### Shell hooks

Enable shell integration for auto-completion and develop activation:

```nix
{
  programs.xi.shellHook = {
    enable = true;
    nixAlias = true;     # alias nix → xi nix
    completion = true;   # register xi completions
    develop = true;      # auto-activate devshells on cd
  };
}
```

### Replace system nix with xi-wrapped nix

```nix
{
  programs.xi.nix.wrapAlias = true;
}
```

Every `nix build`, `nix develop`, `nix run` on the system goes through xi,
gaining nom output and enhanced UX. All other nix binaries (`nix-build`,
`nix-env`, `nix-daemon`) are preserved.

### Tool packages

```nix
{
  programs.xi = {
    nom.enable = true;             # nix-output-monitor (default: true)
    nixFastBuild.enable = true;    # nix-fast-build (default: true)
    fmt.alejandra.enable = true;   # alejandra formatter
    fmt.treefmt.enable = true;     # treefmt formatter
    test.nixUnit.enable = true;    # nix-unit test framework
    test.nixt.enable = true;       # nixt test framework
    test.namaka.enable = true;     # namaka snapshot testing
  };
}
```

Enabled tools are injected into PATH and their backends are auto-configured in
`config.toml`.

### Custom settings

Pass arbitrary `config.toml` values:

```nix
{
  programs.xi.settings = {
    build.keep_going = true;
    build.connect_timeout = 10;
    cache.my-s3.push_url = "s3://my-bucket?region=eu-west-1";
  };
}
```

## Home Manager module

```nix
# flake.nix
{
  inputs.xi.url = "github:Dauliac/xi";

  outputs = { nixpkgs, home-manager, xi, ... }: {
    homeConfigurations.myUser = home-manager.lib.homeManagerConfiguration {
      modules = [
        xi.homeManagerModules.default
        ./home.nix
      ];
    };
  };
}
```

```nix
# home.nix
{
  programs.xi = {
    enable = true;
    shellHook = {
      enable = true;
      develop = true;
    };
  };
}
```

Home Manager hooks integrate per-shell: `programs.bash.initExtra`,
`programs.zsh.initContent`, and `programs.fish.interactiveShellInit`.

## Flake-parts module

```nix
# flake.nix
{
  inputs.xi.url = "github:Dauliac/xi";
  inputs.flake-parts.url = "github:hercules-ci/flake-parts";

  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    imports = [ inputs.xi.flakeModule ];

    perSystem = { config, pkgs, ... }: {
      xi = {
        enable = true;

        # For development, point to local build
        # binPath = "./target/debug/xi";

        shellHook = {
          enable = true;
          develop = true;
        };

        # Tool packages
        nom.enable = true;
        fmt.alejandra.enable = true;
        test.nixUnit.enable = true;
      };

      # Wrap an existing devShell
      devShells.default = config.xi.wrapDevShell (pkgs.mkShellNoCC {
        packages = [ pkgs.rustc pkgs.cargo ];
      });

      # Or use the auto-generated xi devShell
      # devShells.default = config.xi.devshell;
    };
  };
}
```

### Use the shell hook script

The `shellHookScript` option gives you the composed hook as a string for manual
inclusion in devShells:

```nix
devShells.default = pkgs.mkShellNoCC {
  shellHook = config.xi.shellHookScript;
};
```

## See also

- [Explanation: Module system](../explanation/architecture.md#module-system) —
  how the wrapper is composed
- [Reference: Module Options](../reference/module-options.md) — every option the
  modules expose
