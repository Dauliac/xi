# Module Options Reference

Xi provides modules for NixOS, Home Manager, and flake-parts. All three share
the same option structure with minor differences noted below.

## Option namespace

| Context      | Namespace           |
| ------------ | ------------------- |
| NixOS        | `programs.xi.*`     |
| Home Manager | `programs.xi.*`     |
| flake-parts  | `xi.*` (per-system) |

## Core options

| Option         | Type           | Default       | Description                               |
| -------------- | -------------- | ------------- | ----------------------------------------- |
| `enable`       | bool           | `false`       | Enable xi                                 |
| `package`      | package        | xi from flake | Xi package to use                         |
| `binPath`      | string or null | `null`        | Override xi binary path (for development) |
| `finalPackage` | package        | (read-only)   | Computed package after wrapping           |

## Nix backend

| Option          | Type    | Default                           | Description                                             |
| --------------- | ------- | --------------------------------- | ------------------------------------------------------- |
| `nix.package`   | package | `pkgs.nixUnwrapped` or `pkgs.nix` | Backend Nix package (supports Lix)                      |
| `nix.wrapAlias` | bool    | `false`                           | Replace system nix with xi-wrapped version (NixOS only) |

`nix.wrapAlias` is only available in the NixOS module. It sets `nix.package` to
a version where `bin/nix` is an xi proxy while preserving all other Nix binaries
and libraries.

## Wrapper

| Option           | Type | Default | Description                         |
| ---------------- | ---- | ------- | ----------------------------------- |
| `wrapper.enable` | bool | `true`  | Wrap xi with baked-in configuration |

When enabled and configuration exists, the wrapper:

1. Generates `config.toml` as a store path
2. Exports `XI_CONFIG` pointing to it
3. Injects tool packages into PATH
4. Preserves completions and man pages via `symlinkJoin`

## Settings

| Option     | Type          | Default | Description                     |
| ---------- | ------------- | ------- | ------------------------------- |
| `settings` | freeform TOML | `{}`    | Arbitrary `config.toml` content |

Accepts any key-value structure matching the
[Configuration Reference](./configuration.md). Tool enables auto-populate
relevant settings.

Example:

```nix
{
  programs.xi.settings = {
    build.keep_going = true;
    build.connect_timeout = 10;
    cache.my-s3.push_url = "s3://bucket";
  };
}
```

## Shell hooks

| Option                 | Type | Default | Description                      |
| ---------------------- | ---- | ------- | -------------------------------- |
| `shellHook.enable`     | bool | `false` | Install shell integration        |
| `shellHook.nixAlias`   | bool | `true`  | Alias `nix` to `xi nix`          |
| `shellHook.completion` | bool | `true`  | Register xi completions via eval |
| `shellHook.develop`    | bool | `false` | Auto-activate devshells on cd    |

### Where hooks are installed

**NixOS:**

- `environment.interactiveShellInit` (bash/zsh)
- `programs.fish.interactiveShellInit` (fish)

**Home Manager:**

- `programs.bash.initExtra`
- `programs.zsh.initContent`
- `programs.fish.interactiveShellInit`

**flake-parts:**

- `shellHookScript` option (compose into devShells manually)
- `wrapDevShell` function (augment existing devShell)

## Tool packages

All tool options follow the same pattern:

```nix
{
  <tool>.enable = <bool>;     # add to PATH + auto-configure backend
  <tool>.package = <pkg>;     # override package (null = use default)
}
```

### Build tools

| Option         | Default enable                          | Default package           | Description         |
| -------------- | --------------------------------------- | ------------------------- | ------------------- |
| `nom`          | `true` (NixOS/HM), varies (flake-parts) | `pkgs.nix-output-monitor` | Pretty build output |
| `nixFastBuild` | `true` (NixOS/HM), varies (flake-parts) | `pkgs.nix-fast-build`     | Parallel CI builds  |

### Formatter tools

| Option          | Default enable | Default package  | Description               |
| --------------- | -------------- | ---------------- | ------------------------- |
| `fmt.alejandra` | `false`        | `pkgs.alejandra` | Opinionated Nix formatter |
| `fmt.treefmt`   | `false`        | `pkgs.treefmt`   | Multi-language formatter  |

Enabling a formatter auto-sets `settings.fmt.backend`.

### Test tools

| Option         | Default enable | Default package | Description        |
| -------------- | -------------- | --------------- | ------------------ |
| `test.nixUnit` | `false`        | `pkgs.nix-unit` | nix-unit framework |
| `test.nixt`    | `false`        | `pkgs.nixt`     | nixt framework     |
| `test.namaka`  | `false`        | `pkgs.namaka`   | Snapshot testing   |

Enabling a test tool auto-adds to `settings.test.backends`.

## Flake-parts specific

| Option                | Type     | Description                            |
| --------------------- | -------- | -------------------------------------- |
| `devshell.enable`     | bool     | Generate an `xi` devShell              |
| `devshellPackages`    | list     | (read-only) Packages for devShell      |
| `wrapDevShell`        | function | Augment an existing devShell with xi   |
| `shellHookScript`     | string   | (read-only) Composed shell hook        |
| `completionShellHook` | string   | (read-only) Standalone completion hook |

### wrapDevShell usage

```nix
devShells.default = config.xi.wrapDevShell (pkgs.mkShellNoCC {
  packages = [ pkgs.rustc ];
});
```

## NixOS specific

| Option            | Type   | Description                    |
| ----------------- | ------ | ------------------------------ |
| `clean.enable`    | bool   | Enable xi clean systemd timer  |
| `clean.extraArgs` | string | Extra arguments for `xi clean` |
| `flake`           | string | Set `XI_OS_FLAKE` system-wide  |

## Overlay

Xi provides two overlays:

| Overlay               | Description                                 |
| --------------------- | ------------------------------------------- |
| `overlays.default`    | Adds `pkgs.xi`                              |
| `overlays.nixWrapper` | Replaces `pkgs.nix` with xi-wrapped version |

The `nixWrapper` overlay preserves `pkgs.nixUnwrapped` and only replaces
`bin/nix`. All other binaries (`nix-build`, `nix-env`, `nix-daemon`) and
libraries are preserved.
