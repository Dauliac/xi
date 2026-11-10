# Why Flake Input Overrides Work This Way

Pointing a flake input at a local checkout looks like it should be a one-line
convenience. It is not, and the reason is that Nix offers exactly one mechanism
that does the job, and that mechanism leaves three traps open. Understanding
both explains why xi's rules are shaped the way they are.

This document discusses the reasoning. For the task itself, see
[How to build against a local clone of a flake input](../guides/multi-flake-development.md).

## There is only one mechanism, not a choice of four

`--override-input` is not one option among several. It is the only thing that
beats `flake.lock`. The alternatives all look plausible and all lose:

| Mechanism                    | Overrides an input already in `flake.lock`?                                                                                                                 |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `nix registry add`           | No. The user registry is not consulted for flake inputs at all. `nix flake lock` still reports `error: cannot find flake 'flake:b' in the flake registries` |
| global `flake-registry` file | No. It resolves an unlocked input, but once locked, the lock wins and changes are ignored                                                                   |
| `--override-flake`           | No. It loses to the lock, and when no lock exists it writes one                                                                                             |
| `--override-input`           | Yes. The only mechanism that wins, and it is documented to imply `--no-write-lock-file`                                                                     |

This is why the feature exists as a wrapper concern at all. There is no
declarative Nix equivalent to configure instead, so if the override is going to
be persistent, something above Nix has to persist it.

It is also the safe mechanism. `flake.lock` was byte-identical after every
override run, including without passing `--no-write-lock-file` explicitly. The
override changes what gets built, not what is recorded.

## Three traps only a wrapper can close

These were measured on Nix 2.34.8, against this repository's own `nix-lib`
input. Each one is the direct reason for a rule in the implementation.

### A bare path silently becomes a different fetcher

`--override-input nix-lib /abs/path` locks as
`{"type":"git","rev":"cb3f79a","ref":"refs/heads/main"}`: a git fetch pinned to
the last commit. `path:/abs/path` locks as `{"type":"path"}` and reads the live
working tree.

The git form does see uncommitted edits to tracked files, which is why the
problem hides for a while. Then it hard-errors on anything untracked:

```
error: Path 'extra.nix' in the repository "..." is not tracked by Git.
```

That is precisely the file you just created in the sibling repo, which is the
whole reason you were pointing at a local clone. So xi always emits the `path:`
scheme and never a bare path. The cost of that choice is that you cannot ask for
the git-fetch semantics through a plain path value; if you want them, write a
`git+file://` flakeref explicitly.

### Relative paths resolve against the shell, not the flake

In raw Nix, a relative override resolves against the process working directory.
The same override therefore means different things depending on which
subdirectory you happened to run it from. In a worktree-heavy checkout that is
worse than merely surprising: from `.claude/worktrees/<name>`, `../nix-lib`
points at `.claude/worktrees/nix-lib`.

Xi resolves relative values itself, against the context the value was _written_
in rather than the context it was _read_ in:

| Source                        | Base for a relative value                                  |
| ----------------------------- | ---------------------------------------------------------- |
| `.xi.toml`, `.xi.local.toml`  | that file's own directory                                  |
| `~/.config/xi/config.toml`    | rejected: a machine-global file has no meaningful base     |
| `XI_OVERRIDE_INPUT`           | the flake root, so it means the same from any subdirectory |
| `--override-input` on the CLI | the process working directory, matching Nix exactly        |

The last row is a deliberate inconsistency. A path typed at a prompt should mean
what the shell would mean by it. Silently reinterpreting it would be a worse
surprise than the inconsistency, and it keeps `xi nix` a faithful `nix` for
anything typed directly.

The global-config row is the other judgement call. Using `$HOME` or the working
directory as an implicit base would have been a guess, and a guess that silently
redirects a build input is exactly the class of bug this feature could
introduce. Refusing is louder and cheaper to diagnose.

### A literal space breaks flakeref parsing

`path:` is parsed as a URL, so a path containing a raw space stops being
recognised as one. Nix then prepends the working directory to the literal text
and reports something unguessable from the symptom:

```
error: getting status of "<cwd>/path:/home/.../sp test/b"
```

Percent-encoding works. So paths are encoded before the scheme is applied, which
is why this is treated as a correctness case in the tests rather than an edge
case.

## Why the value is a map, written inline

The config key is a TOML inline table:

```toml
[build]
override_input = { nix-lib = "~/ghq/github.com/Dauliac/nix-lib" }
```

Inline rather than a `[build.override_input]` section, because TOML forbids
adding scalar keys to `[build]` after a sub-table of `[build]` has been opened.
With the section form, any key written below it silently lands in the wrong
table. For a file humans hand-edit, the inline form removes that trap entirely.

A map rather than the array-of-pairs shape the CLI's two-value flag suggests,
because TOML forbids duplicate keys. Passing the same input twice on the command
line is last-wins and silent; in the config file the same mistake is
structurally impossible.

## Why overrides are merged by name, not concatenated

Nix already resolves two flags naming the same input by taking the last one,
silently. Concatenating the cascade and leaning on that would produce the
correct build. It would not produce an honest report: xi would be printing an
override that Nix then discarded.

Merging by name first means the report matches reality, and it means
`--override-input nix-lib <other>` replaces only that entry while the rest of
your configured set stays applied. Resolution is sorted by name, which also
keeps the generated argv deterministic for snapshot tests.

## Why overrides are filtered against `flake.lock`

An override naming an input the flake does not have is not an error in Nix, only
a warning:

```
warning: the flag '--override-input nix-lib ...' does not match any input
```

Without filtering, a single entry in `~/.config/xi/config.toml` would print that
on every build in every unrelated repository, which would make machine-global
placement useless in practice. So xi reads `nodes.root.inputs` from `flake.lock`
(a plain JSON read: no evaluation, no network) and drops what does not apply.

A transitive key such as `a/b` is checked against the parent name `a` only.
Whether `b` exists cannot be established without resolving `a` first, and doing
that would mean evaluating the flake.

## Why lock-writing commands are excluded

`--override-input` implies `--no-write-lock-file`. Injecting it into
`nix flake update` or `nix flake lock` would make those commands silently stop
doing the one thing they exist to do. That is the worst failure mode this
feature could have, so those subcommands are excluded and the suppression is
announced rather than silent: a user who has configured overrides must be able
to tell whether they applied.

This gate cannot be expressed on the command kind alone. Xi's internal `Flake`
command kind covers `update`, `lock`, `check`, `show` and `metadata` alike, so
the subcommand itself has to be inspected.

## Why the report is mandatory

A config file that silently redirects a build input is a genuine hazard. You
could be debugging a failure that comes from a stale local checkout you set up
weeks ago and forgot. Every applied override is therefore reported once per
invocation with the absolute path actually used, and `xi doctor` surfaces the
active set on demand. The one-line report and the "never commit
`.xi.local.toml`" guidance are requirements of this feature, not polish on top
of it.

## What was deliberately left out

**Deriving the local path from the input URL.** Mapping `github:Dauliac/nix-lib`
to a clone root, so that no path ever has to be typed, was considered and
rejected. It breaks the property that the configured value is exactly what Nix
receives, and it needs an owner allowlist to be safe: this repository's
`flake.nix` deliberately keeps some inputs off its own nixpkgs, and a "link
anything cloned" rule would silently defeat those pins and trigger large
rebuilds. With a single linked input in practice, it was speculative machinery.
If typing paths becomes the real pain, it belongs in a separate explicit key
rather than as an overloaded meaning for this one.

**An `xi link` subcommand.** The only thing it bought was discoverability, which
the per-invocation report and `xi doctor` already provide. It would also have
been the one setting in xi that does not follow xi's own documented
flag/env/config mapping.

**Persisting via `nix flake lock --override-input`.** Writes a `path:` entry
into a tracked file, pollutes every diff, and risks pushing a machine-local path
to everyone else.

**A relative path input committed in `flake.nix`.** Measured to be impossible
when the target is outside the tree: relative inputs resolve against the store
path, giving
`access to absolute path '/nix/store/...' is forbidden in pure
evaluation mode`,
and `--impure` only changes the error. It works only when the target is inside
the same repository.

## See also

- [How to build against a local clone of a flake input](../guides/multi-flake-development.md)
- [The Xi Specification](xi-spec.md) for the configuration cascade this follows
- [Configuration](../reference/configuration.md) for the key's full reference
