# How to Manage Home Manager Configurations

## Switch to a new configuration

```sh
xi home switch /path/to/flake
```

If `XI_FLAKE` or `XI_HOME_FLAKE` is set:

```sh
xi home switch
```

## Select a configuration name

```sh
xi home switch -c myConfig
```

Without `-c`, xi tries to discover the configuration name automatically.

## Build without switching

```sh
xi home build
```

## Enter a REPL

```sh
xi home repl
```

## Update flake inputs before switching

```sh
xi home switch --update
xi home switch --update-input nixpkgs
```

## Specialisations

Home Manager specialisations are read from
`~/.local/share/home-manager/specialisation`. Configure them in your Home
Manager config:

```nix
{config, ...}: {
  specialisation."work".configuration = {
    xdg.dataFile."home-manager/specialisation".text = "work";
  };
}
```

## Show activation logs

```sh
xi home switch --show-activation-logs
```

## Dry run

```sh
xi home switch --dry
```

## Control the diff

```sh
xi home switch --diff always
xi home switch --diff never
```
