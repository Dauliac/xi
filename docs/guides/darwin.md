# How to Manage nix-darwin Configurations

## Switch to a new configuration

```sh
xi darwin switch /path/to/flake
```

If `XI_FLAKE` or `XI_DARWIN_FLAKE` is set:

```sh
xi darwin switch
```

## Select a hostname

```sh
xi darwin switch -H myMac
```

## Build without switching

```sh
xi darwin build
```

## Enter a REPL

```sh
xi darwin repl
```

## Update inputs before switching

```sh
xi darwin switch --update
xi darwin switch --update-input nixpkgs
```

## Show activation logs

Activation output is hidden by default:

```sh
xi darwin switch --show-activation-logs
```

## Dry run

```sh
xi darwin switch --dry
```
