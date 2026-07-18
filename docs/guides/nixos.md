# How to Manage NixOS Systems

## Switch to a new configuration

```sh
xi os switch /path/to/flake
```

If `XI_FLAKE` or `XI_OS_FLAKE` is set:

```sh
xi os switch
```

Xi evaluates, builds with nom, shows a package diff, and activates.

## Select a specific hostname

```sh
xi os switch -H myHost
```

Without `-H`, xi uses the current system hostname.

## Boot without activating

Set the new configuration as the boot default without activating it in the
running system:

```sh
xi os boot
```

## Test without making it the boot default

Activate the configuration for this session only:

```sh
xi os test
```

## Build without switching

Build the configuration and leave the result in `./result`:

```sh
xi os build
```

## Rollback

Revert to the previous generation:

```sh
xi os rollback
```

Rollback to a specific generation:

```sh
xi os rollback --to 42
```

## Inspect generations

```sh
xi os info
```

Select specific columns:

```sh
xi os info --fields version,closure-size
```

## Build a VM

```sh
xi os build-vm
```

With a bootloader and immediate launch:

```sh
xi os build-vm --with-bootloader --run
```

## Build a disk image

```sh
xi os build-image --image-variant <VARIANT>
```

Variants come from `config.system.build.images`.

## Update flake inputs before switching

Update all inputs:

```sh
xi os switch --update
```

Update a single input:

```sh
xi os switch --update-input nixpkgs
```

## Control the diff

```sh
xi os switch --diff always   # always show
xi os switch --diff never    # never show
```

## Skip confirmation

```sh
xi os switch  # asks by default if --ask is configured
```

## Dry run

```sh
xi os switch --dry
```

## Use a classical (non-flake) configuration

```sh
xi os switch -f '<nixpkgs/nixos>'
xi os switch -f '<nixpkgs/nixos>' -- -I nixos-config=/path/to/configuration.nix
```

## Specialisations

Xi auto-detects the running specialisation. To configure detection, write the
specialisation name to `/etc/specialisation`:

```nix
{config, ...}: {
  specialisation."gaming".configuration = {
    environment.etc."specialisation".text = "gaming";
  };
}
```

## Install the bootloader

Force bootloader installation during switch or boot:

```sh
xi os switch --install-bootloader
```

## Enter a REPL

Load your NixOS configuration in an interactive Nix REPL:

```sh
xi os repl
```

## Show activation logs

Activation output is hidden by default. Show it for debugging:

```sh
xi os switch --show-activation-logs
```

Or set `XI_SHOW_ACTIVATION_LOGS=1` globally.
