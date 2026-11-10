# How to Build Against a Local Clone of a Flake Input

Your flake pins a library. You are editing that library too. You want builds to
use your working copy instead of the pinned revision, without retyping a path on
every command.

Pick the scope that matches how long you need the override to last.

## Override an input on this machine, for every project

Add the input to `~/.config/xi/config.toml`:

```toml
[build]
override_input = { nix-lib = "~/ghq/github.com/Dauliac/nix-lib" }
```

Every `xi build`, `xi check`, `xi run`, `xi fmt` and `xi develop` now resolves
that input to your clone, and reports it once per run:

```
> override-input nix-lib -> /home/you/ghq/github.com/Dauliac/nix-lib (from config.toml)
```

Leave the entry there permanently if you like. Xi drops overrides whose input
the current flake does not declare, so it stays silent in unrelated repos.

In this file, use an absolute or `~`-prefixed path. A relative path is refused,
because a machine-global file has no directory to resolve against.

## Override an input for one project

Write it to `.xi.local.toml`, next to `flake.nix`:

```toml
[build]
override_input = { nix-lib = "../nix-lib" }
```

A relative path here resolves against the file's own directory, so it means the
same thing from any subdirectory of the project.

Do not commit this file. It is gitignored on purpose, because an override value
is machine state: an absolute path that is wrong on every other machine and in
CI. If you need an override that is genuinely project policy for everyone, put
it in the committed `.xi.toml` instead.

If you are in a worktree-heavy checkout, prefer the `~/...` form here too. From
a worktree at `.claude/worktrees/<name>`, `../nix-lib` resolves to
`.claude/worktrees/nix-lib`, which does not exist. Resolution fixes "it depends
which directory I am standing in", not nesting.

## Override an input for one shell session

Export `XI_OVERRIDE_INPUT`. It takes `NAME=FLAKEREF` pairs separated by any
whitespace, so the multi-line form works in an `.envrc`:

```bash
export XI_OVERRIDE_INPUT="
  nix-lib=~/ghq/github.com/Dauliac/nix-lib
  nix-vm-test=../../numtide/nix-vm-test
"
```

Relative paths here resolve against the flake root.

## Override an input for a single command

Pass the flag, exactly as you would to `nix`:

```bash
xi build .#xi --override-input nix-lib ../nix-lib
```

A path typed here resolves against your current directory, matching what `nix`
itself would do.

## Combine the levels

Overrides resolve per input name, lowest priority first:

```
~/.config/xi/config.toml  <  .xi.toml  <  .xi.local.toml  <  XI_OVERRIDE_INPUT  <  --override-input
```

Because resolution is per name, you can keep a set of overrides configured and
redirect just one of them:

```bash
# nix-lib and nix-vm-test both configured; this replaces only nix-lib.
xi build .#xi --override-input nix-lib ../other-checkout
```

## Override a transitive input

Quote the key, because `/` is not legal in a bare TOML key:

```toml
override_input = { "nix-lib/flake-parts" = "path:/some/checkout" }
```

## Point an input at something other than a path

Any value that is not a filesystem path is forwarded to `nix` untouched:

```toml
override_input = { nixpkgs = "github:NixOS/nixpkgs/nixos-unstable" }
```

Xi treats a value as a path only when it starts with `path:`, `/`, `./`, `../`
or `~`. A bare word like `nixpkgs` stays an indirect flakeref, so it is never
mistaken for a directory.

## Check what is currently overridden

```bash
xi doctor
```

It lists each active override with the absolute path it resolved to, and warns
if `.xi.local.toml` has been committed by mistake.

## Troubleshooting

**Nothing is reported and the override had no effect.** The input name is not
declared by the flake you are building. Check it against `nodes.root.inputs` in
`flake.lock`.

**"that directory does not exist".** The error names both the value as you wrote
it and the absolute path it resolved to. Compare them: if they disagree with
what you expected, your relative value resolved against a different base than
you assumed. See the precedence table above for which base applies to which
source.

**"a machine-global config requires an absolute or ~-prefixed path".** You put a
relative value in `~/.config/xi/config.toml`. Use `~/...` there, or move the
entry into the project's `.xi.local.toml`.

**`xi update` says it is not applying your overrides.** That is deliberate.
`--override-input` implies `--no-write-lock-file`, so injecting it would stop
`nix flake update` writing `flake.lock`. Run the update, then your overrides
apply again on the next build.

## See also

- [Why flake input overrides work this way](../explanation/flake-input-overrides.md)
  for the Nix behaviour behind these rules, and the trade-offs taken
- [Configuration](../reference/configuration.md) for the full
  `build.override_input` reference
- [Nix Proxy](nix-proxy.md) for using `xi nix` as a drop-in `nix`
