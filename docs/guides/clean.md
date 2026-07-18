# How to Clean the Nix Store

Xi reimplements `nix-collect-garbage` with finer control over what is kept and
additional context before cleanup.

## Clean all profiles

```sh
xi clean all
```

This removes old generations from all profiles (system, user, and Home Manager),
cleans orphaned GC roots, and runs `nix store gc`.

## Clean the current user only

```sh
xi clean user
```

## Clean a specific profile

```sh
xi clean profile /nix/var/nix/profiles/system
```

## Keep recent generations

Keep at least 3 generations:

```sh
xi clean all --keep 3
```

Keep anything newer than 7 days:

```sh
xi clean all --keep-since 7d
```

Combine both:

```sh
xi clean all --keep 3 --keep-since 7d
```

Duration format follows [humantime](https://docs.rs/humantime/) syntax: `30s`,
`5m`, `2h`, `7d`, `4w`.

## Dry run

See what would be cleaned without doing it:

```sh
xi clean all --dry
```

## Ask for confirmation

```sh
xi clean all --ask
```

## Preserve direnv GC roots

By default, xi cleans direnv GC roots. To preserve them:

```sh
xi clean all --no-direnv
```

## Skip GC root cleanup entirely

```sh
xi clean all --no-gcroots
```

## Skip garbage collection

Remove old generations but do not run `nix store gc`:

```sh
xi clean all --no-gc
```

## Optimise the store

Run `nix-store --optimise` after garbage collection to deduplicate files:

```sh
xi clean all --optimise
```

## Limit collection size

```sh
xi clean all --max 10000000000  # 10 GB in bytes
```

## Cross filesystem boundaries

By default, the GC root scan stays on the same filesystem as
`/nix/var/nix/gcroots`. To cross boundaries:

```sh
xi clean all --cross-filesystems
```

## Keep one direnv project GC root

Preserve the active direnv GC root for each project regardless of
`--keep-since`:

```sh
xi clean all --keep-one
```

## Automate with the NixOS module

```nix
{
  programs.xi = {
    enable = true;
    clean.enable = true;
    clean.extraArgs = "--keep-since 4d --keep 3";
  };
}
```

This runs `xi clean` as a systemd service on a timer.
