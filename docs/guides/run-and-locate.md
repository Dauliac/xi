# How to run flake apps and locate nixpkgs commands

`xi run` does two related things: it runs a **flake app** (the normal
`nix run` behaviour), and — with `--locate` — it acts like a
[comma](https://github.com/nix-community/comma)-style locator that finds any
nixpkgs binary by command name and runs it once, without installing it.

## Run a flake app

```sh
xi run                     # runs apps.<system>.default
xi run .#hello             # runs apps.<system>.hello
xi run github:owner/repo#tool
```

Anything after `--` is passed to the app:

```sh
xi run .#myserver -- --port 8080 --config prod.toml
```

## Locate a nixpkgs command

Say you don't have `ffmpeg` installed but you need it right now:

```sh
xi run --locate ffmpeg -- -i input.mp4 -c:v libx264 output.mp4
```

Xi searches nixpkgs for a package that provides an `ffmpeg` binary, builds it,
and runs it. Your profile is unchanged; the derivation stays in the store as a
GC-collectable dependency of the run.

Short form:

```sh
xi run -l ffmpeg -- -i input.mp4 output.mp4
```

## Enable locate mode by default

If most of your `xi run` invocations are locates:

```sh
export XI_RUN_LOCATE=1
xi run htop
```

## Open a shell with the tool available

Instead of running once, open a subshell with the resolved package on `PATH`:

```sh
xi run --locate ffmpeg --shell
```

Everything in that shell — including scripts you call — will find `ffmpeg`.

## Install into your profile

If the tool is useful enough to keep:

```sh
xi run --locate ffmpeg --install
```

Installs the resolved package into your user profile so `ffmpeg` is on `PATH`
in every new shell.

## Control the locate cache

Locate mode caches (a) the command → package resolution and (b) your choice
when multiple packages provide the same command.

| Level | Behaviour                                                                  |
| ----- | -------------------------------------------------------------------------- |
| `0`   | No cache; ask every time                                                   |
| `1`   | Remember your choice when a command has multiple candidates                |
| `2`   | Full: also cache the nixpkgs resolution (default)                          |

```sh
xi run --locate ffmpeg --cache-level 0    # bypass cache
```

Persist a default in `config.toml`:

```toml
[locate]
cache_level = 1
```

Or via environment:

```sh
export XI_LOCATE_CACHE=1
```

## When multiple packages match

Some commands (`convert`, `sha256sum`, `curl`) exist in several nixpkgs
attributes. Xi prompts you interactively; with `cache_level >= 1`, the choice is
remembered for next time.

## See also

- [CLI Reference: `xi run`](../reference/cli.md#xi-run--run-a-flake-app-or-nixpkgs-package)
- [comma](https://github.com/nix-community/comma) — the upstream tool that inspired locate mode
