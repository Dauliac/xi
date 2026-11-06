# How to Bootstrap a Machine Directly from the OS

Use this when you are sitting at the machine you want to configure: a laptop or
a workstation. Xi adopts your config repo and switches to it in one command, no
separate install step beyond Nix itself.

Precondition: Nix is already installed and `xi` is reachable (via
`nix shell github:you/xi`, a profile install, or your distro's package). Xi does
not install Nix on a bare machine yet; see
[How to Push a Config
to a Server and Hand It Off to GitOps](bootstrap-push-gitops.md) for the one
bootstrap path that is further along, or watch the roadmap if you need a true
zero-to-Nix flow.

## Pick a scope

- **User scope** (`xi home switch`): your dotfiles, no root needed.
- **System scope** (`xi system switch`): host-wide, via NixOS, nix-darwin, or
  system-manager on non-NixOS Linux. Needs elevation for the final activation
  step only.

Both take the same shape.

## Adopt and switch in one step

```sh
xi home switch --branch main git@github.com:you/dotfiles.git
```

```sh
xi system switch --branch main git@github.com:you/dotfiles.git
```

Xi auto-detects a `git@host:path`, `ssh://`, or `https://` URL as a config repo
to adopt rather than a flake installable. A bare local path is ambiguous with a
flake directory, so name it explicitly:

```sh
xi system switch --branch main --adopt /srv/mirror/dotfiles
```

This clones the remote into xi's own sources directory, records it, and builds
and activates from the commit it points at, not your terminal's current
directory and not any uncommitted changes sitting in some other checkout. If
your remote's default branch is not `main`, `--branch` is required on this first
call: there is no `.xi.toml` yet to read it from.

## If system scope refuses to run

`xi system switch` refuses outright if you run it as root: it elevates
internally only for the steps that need it (writing `/etc`, activating the new
generation), using `--elevation-strategy` (`pkexec` by default). Run it as
yourself.

On a host where `/var/lib/xi` has never existed, the very first system-scope
switch can fail with a bare permission error before it gets that far, creating
that directory is not yet wired through xi's own elevation path. Provision it
once:

```sh
sudo install -d -m 0700 -o "$(id -un)" -g "$(id -gn)" \
  /var/lib/xi /var/lib/xi/sources /var/lib/xi/state
```

Then run the switch as yourself, not under `sudo`.

## Verify

```sh
xi system sources status --explain
```

`status: "ready"` with a `sources_head` and `active_generation` means it worked.
`--explain` shows which rung (CLI flag, env var, `.xi.toml`, or Nix module)
answered each config key.

## Switching again later

Once adopted, drop the remote:

```sh
xi system switch
```

This re-resolves from what was recorded on the first call. Passing the remote
again is only needed to re-point at a different one, with `--force`.
