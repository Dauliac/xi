# How to Push a Config to a Server and Hand It Off to GitOps

Use this for a lab machine or a server you are not sitting at: push a config to
it once from your workstation, then let it manage itself from there on, pulling
and activating its own updates without further pushes.

Precondition: the target already has Nix reachable over SSH, and your flake
declares it (a `systemConfigs.<name>` or `nixosConfigurations.<name>`
attribute). Today `xi push` only runs the `activate` phase; it does not yet
install Nix onto a bare machine, so a true zero-to-Nix target needs some other
bring-up step first.

## Declare the target

Add its SSH address to `.xi.toml` at your config repo's root:

```toml
[[push.hosts]]
name = "lab1"       # must match the flake attribute name
ssh = "root@lab1.example.com"
```

`name` is how `xi push lab1` finds it; `xi push` with no name pushes to every
target your flake declares.

## Declare GitOps on the target's own config

This is what makes it self-managing after the first push. In the target's own
NixOS or system-manager module:

```nix
programs.xi.sources = {
  enable = true;
  remote = "git@github.com:you/dotfiles.git";
  gitops.enable = true;
};
```

Do not also enable `gitops` for the same remote at user scope on the same host:
two daemons polling the same remote race each other on activation, and xi
refuses to evaluate a config that does.

## Push it once

```sh
xi push lab1
```

This resolves the same commit your local machine would switch to, builds a
closure (by default, on your machine), copies it to `lab1`, and activates it
over SSH. From here, `lab1`'s own `xi-sources-daemon` systemd unit takes over:
it polls `remote` on the configured interval, fast-forwards, and activates on
advance. You do not need to push again unless you want to force something out of
band.

## Push to several targets at once

```sh
xi push                 # every target the flake declares
xi push lab1 lab2       # a subset, by name
xi push --on @gpu        # by tag
```

```sh
xi push --parallel 4
```

Activates up to 4 targets concurrently instead of one at a time.

## Choose where the build happens

```sh
xi push lab1 --remote-build true
```

`true` syncs the sources dir to `lab1` and builds there, correct when your link
to it is slow or the target has more CPU than your machine does. `false` (the
default) builds locally and copies a closure in, correct when the target is
comparatively weak. Pin a default per host with `remote-build` in its
`[[push.hosts]]` entry instead of passing the flag every time.

## If the target needs an interactive sudo password

```sh
xi push lab1 --interactive-sudo
```

Allocates a PTY so a password prompt on the remote actually reaches your
terminal, instead of the connection hanging.

## Stop at the first failure

```sh
xi push --fail-fast
```

Without it, a failing target is recorded and the rest still run.
