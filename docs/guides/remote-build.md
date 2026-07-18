# How to Build and Deploy Remotely

Xi supports SSH-based remote builds and deployments. Derivations are copied to
the remote host, built there, and results transferred back.

## Build on a remote host

```sh
xi os switch --build-host user@builder.example.com
```

The flow:

1. Evaluate locally
2. Copy derivations to the remote builder via `nix copy`
3. Build on the remote host
4. Copy results back to the local machine
5. Activate locally

## Deploy to a remote target

```sh
xi os switch --target-host root@server.example.com
```

The flow:

1. Evaluate locally
2. Build locally
3. Copy closure to the target
4. Activate on the target over SSH

## Build remotely, deploy remotely

```sh
xi os switch --build-host builder@build.local --target-host root@prod.local
```

## Host specification formats

```sh
--target-host hostname
--target-host user@hostname
--target-host ssh-ng://user@hostname
--target-host user@[::1]          # IPv6
```

## SSH configuration

Xi respects your `~/.ssh/config`. For repeated connections, enable
`ControlMaster` to reuse SSH sessions:

```
Host builder.example.com
  ControlMaster auto
  ControlPath ~/.ssh/cm-%r@%h:%p
  ControlPersist 10m
```

Pass extra SSH options:

```sh
export XI_SSHOPTS="-o StrictHostKeyChecking=no"
```

Or per-invocation:

```sh
XI_SSHOPTS="-p 2222" xi os switch --target-host user@host
```

## Elevation on the remote host

By default, xi elevates with `sudo` on the remote. If the remote user is already
root, no elevation is attempted.

For passwordless sudo:

```sh
xi os switch --target-host root@host --elevation-strategy passwordless
```

For no elevation:

```sh
xi os switch --target-host root@host --elevation-strategy none
```

## Use binary cache substitutes during copy

```sh
xi os switch --target-host root@host --use-substitutes
```

The target host fetches paths from its configured substituters instead of
receiving everything over SSH.

## Skip pre-activation validation

```sh
xi os switch --target-host root@host --no-validate
```

Or set `XI_NO_VALIDATE=1`.

## Clean up remote processes on interrupt

Opt-in. When Ctrl+C is pressed, xi can attempt to kill remote Nix processes:

```sh
export XI_REMOTE_CLEANUP=1
xi os switch --target-host root@host
```

## Remote diff

Xi displays package diffs for remote deployments by querying the target's
current system profile over SSH before building.
