# Design: xi-override-input-config

## Naming is derived, not invented

`docs/explanation/xi-spec.md:61` already fixes the naming rule, so there is
nothing to decide:

| CLI flag           | Env var             | Config key             |
| ------------------ | ------------------- | ---------------------- |
| `--override-input` | `XI_OVERRIDE_INPUT` | `build.override_input` |

Keys are `snake_case` per `xi-spec.md:41`, hence `override_input` and not
`override-input`. An earlier draft of this change proposed a `[link]` table with
an `xi link` subcommand. It was dropped: it would have been the only setting in
xi that does not follow xi's own documented mapping, and a verb is not needed to
write two lines of TOML.

## Measured Nix behaviour this design is built on

All of the following were run on nix 2.34.8, with isolated test flakes under a
scratch directory and an isolated `XDG_CONFIG_HOME`, plus confirmation against
this repo's real `nix-lib` input. They are the reason each rule below exists.

| observation                                                                                                               | consequence for this design                                                                                       |
| ------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `--override-input` beats `flake.lock`; the user registry, the global registry file and `--override-flake` all lose to it  | the feature must inject this flag, there is no declarative Nix equivalent                                         |
| `--override-input` implies `--no-write-lock-file`, and `flake.lock` stayed byte-identical in every run                    | injection is safe for the lock, but fatal for lock-writing subcommands                                            |
| bare `/abs/path` locks as `{"type":"git","rev":...,"ref":"refs/heads/main"}`; `path:/abs/path` locks as `{"type":"path"}` | always emit the `path:` scheme                                                                                    |
| the git form hard-errors on untracked and gitignored files (`Path 'extra.nix' ... is not tracked by Git`)                 | the `path:` scheme is the only one usable for work-in-progress checkouts                                          |
| relative overrides resolve against the process CWD                                                                        | xi must resolve them itself, per source                                                                           |
| a literal space makes `path:` stop parsing as a URL, producing `getting status of "<cwd>/path:/..."`; `%20` works         | percent-encode before prefixing                                                                                   |
| two `--override-input` flags for the same input: the last one wins, silently, no warning                                  | ordering alone gives correct precedence, but merge by name anyway so the report cannot advertise a shadowed entry |
| an override naming a non-existent input only warns (`does not match any input`), it does not fail                         | filter against the real input set, otherwise global config spams unrelated repos                                  |

## Value shape

A map of input name to flakeref, written as a TOML **inline table**:

```toml
[build]
keep_going = true
override_input = { nix-lib = "~/ghq/github.com/Dauliac/nix-lib", nix-vm-test = "../nix-vm-test" }
```

Inline rather than a `[build.override_input]` section, deliberately. TOML
forbids adding scalar keys to `[build]` after a sub-table of `[build]` has been
opened, so the section form means any key written below it silently lands in the
wrong table. For a file humans hand-edit, the inline form removes that trap
entirely.

A map, rather than the array-of-pairs shape the CLI's `number_of_values = 2`
suggests, because TOML forbids duplicate keys. That makes the ambiguity the CLI
has (last-wins, silently) structurally impossible in the config.

Transitive overrides work unchanged, the key just needs quoting because `/` is
not legal in a TOML bare key:

```toml
override_input = { "nix-lib/flake-parts" = "path:/some/checkout" }
```

Rust side: `BTreeMap<String, String>`. Sorted iteration also makes the reported
order and the generated argv deterministic, which matters for snapshot tests.

## Environment variable grammar

Whitespace-separated `name=flakeref` pairs:

```bash
XI_OVERRIDE_INPUT="nix-lib=~/ghq/github.com/Dauliac/nix-lib nix-vm-test=../nix-vm-test"
```

Splitting on any run of whitespace, which makes the multi-line form work for
free. That is the shape people actually want in an `.envrc`:

```bash
export XI_OVERRIDE_INPUT="
  nix-lib=~/ghq/github.com/Dauliac/nix-lib
  nix-vm-test=../../numtide/nix-vm-test
"
```

Two rules:

- Split each pair on the **first** `=` only. Flakerefs carry their own
  (`?ref=main&rev=abc`).
- Whitespace is the separator because a valid flakeref cannot contain a raw
  space. This is not an assumption, it is the measured behaviour above: a
  literal space makes `path:` stop parsing as a URL. A comma separator would
  have been wrong, commas are legal inside URL query values.

Rejected encodings:

- `XI_OVERRIDE_INPUT_NIX_LIB=...`, one variable per input. The name mangling is
  lossy: `nix-lib` and `nix_lib` both become `NIX_LIB`, and a transitive
  `nix-lib/flake-parts` has no sane spelling.
- `XI_OVERRIDE_INPUT_1` / `_2` indexing. Gaps and ordering noise, no gain.

## Is this value a path at all

Resolve only if the value starts with `path:`, `/`, `./`, `../`, or `~`.
Everything else is passed through verbatim: `github:NixOS/nixpkgs/abc`,
`git+ssh://...`, `https://...`, or a bare indirect ref like `nixpkgs`.

Requiring the `./` prefix for relative paths mirrors nix's own installable
convention, where a bare word is an indirect flakeref. So a bare `nix-lib` is
never guessed to be a directory, and the rule has no ambiguous cases.

`path:../x` must also be resolved: the scheme is stripped, the remainder
resolved per the table below, then the scheme is re-applied.

## Base directory: relative to where the value was written

One principle, four cases:

| source                                 | base for a relative value                                                                                                    |
| -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `.xi.toml` or `.xi.local.toml`         | that file's own directory                                                                                                    |
| `~/.config/xi/config.toml`             | rejected with an error. A machine-global file has no meaningful base, and silently using `$HOME` or the CWD would be a guess |
| `XI_OVERRIDE_INPUT`                    | the flake root, so the value means the same from any subdirectory                                                            |
| `--override-input` on the command line | the process CWD, matching nix exactly                                                                                        |

The CLI case stays CWD-relative on purpose. A path typed at a prompt should mean
what the shell would mean; silently reinterpreting it would be a worse surprise
than the inconsistency, and it keeps `xi nix` a faithful nix for anything typed
directly.

Honest limit worth documenting for users: resolution fixes "depends on where I
stand", not nesting. From a worktree at `.claude/worktrees/<name>`, `../nix-lib`
resolves to `.claude/worktrees/nix-lib`, which does not exist. Relative values
suit a true sibling layout; for worktrees the idiomatic value is
`~/ghq/github.com/Dauliac/nix-lib`, which is short, worktree-proof and portable
across machines once `~` expansion exists.

## Normalization pipeline

Strict order, each step justified by a measurement above:

1. Expand a leading `~`. The shell does this for an unquoted env value but never
   inside a TOML string, so xi must.
2. If relative, join with the base directory from the table above.
3. Clean `..` and `.` **lexically**. Not `fs::canonicalize()`: that resolves
   symlinks the user may have chosen deliberately and errors on paths that do
   not exist yet, which would turn a clear xi error into an opaque io error. The
   `path-clean` crate does this, or it is about twenty lines.
4. Percent-encode the path.
5. Prefix `path:`. Never emit a bare path.

Then validate: the resolved path must be a directory containing `flake.nix`. On
failure, report the value as written **and** the absolute path it resolved to.
Without both, a wrong base directory is undiagnosable.

## Filtering against the real input set

Before emitting, drop any override whose input name is not an input of the flake
being operated on. The names are available by reading `flake.lock`
(`nodes.root.inputs`), which is a plain JSON read: no nix evaluation, no
network, no measurable cost per invocation.

This is what makes machine-global placement practical. Without it, a single
`override_input` entry in `~/.config/xi/config.toml` would print
`warning: the flag '--override-input nix-lib ...' does not match any input` on
every build in every unrelated repo.

Transitive keys (`a/b`) are checked against the parent input name only, since
the child's presence cannot be established without resolving `a` first.

## Suppression on lock-writing subcommands

`--override-input` implies `--no-write-lock-file`. So injecting it into
`nix flake update` or `nix flake lock` makes those commands silently stop doing
their job, which is the worst failure mode this feature could have.

`CommandKind` at `crates/nix-command/src/lib.rs:33` is too coarse for this gate:
its `Flake` variant covers `update`, `lock`, `check`, `show` and `metadata`
alike. The gate therefore has to inspect the flake subcommand, not just the
kind. When suppression happens it must be announced, otherwise a user who has
configured overrides cannot tell whether they applied.

Kinds that never receive the flag at all: `Store`, `Copy`, `PathInfo`, `Config`.

## Merge semantics

Resolve by input name across the cascade, lowest to highest:

```
~/.config/xi/config.toml  <  .xi.toml  <  .xi.local.toml  <  XI_OVERRIDE_INPUT  <  --override-input
```

Emit one flag per surviving name. Merging by name rather than concatenating
means `--override-input nix-lib <other>` replaces only that entry and leaves the
rest of the configured set applied. Concatenation plus nix's last-wins behaviour
would produce the same build, but it cannot produce an honest report and it
cannot support per-name disabling.

`.xi.local.toml` is new, and is the gitignored sibling of the existing
`.xi.toml`. The split is load-bearing: `.xi.toml` is committed project policy,
while an override value is machine state that is wrong on every other machine
and in CI. This change adds `.xi.local.toml` to this repo's `.gitignore` and has
`xi doctor` warn when the file is tracked.

## Implementation note on `apply_build_defaults`

`crates/xi-core/src/args.rs:396` is `pub const fn` and merges scalars by field.
A `BTreeMap` merge cannot happen in a const fn. Add a sibling
`apply_override_input_defaults(&mut self, overrides: &BTreeMap<String, String>)`
rather than making the existing function non-const, which keeps the blast radius
to one new method.

Note also that the field at `:295` is a flat `Vec<String>` consumed in pairs via
`chunks(2)` at `:382`. The config path should produce the same flat pair
representation at the end, so there is exactly one emission site and the
existing test at `override_input_pairs_are_emitted` keeps covering it.

## Rejected alternatives

- **Deriving the path from the input URL and a clone root.** Mapping
  `github:Dauliac/nix-lib` to `$(ghq root)/github.com/Dauliac/nix-lib` would
  mean never typing a path. Rejected for this change: it breaks the property
  that the configured value is exactly what nix receives, it needs an owner
  allowlist to be safe (this machine already has `numtide/treefmt-nix` and
  `numtide/system-manager` cloned locally, and `flake.nix` deliberately keeps
  system-manager off this repo's nixpkgs, so a "link anything cloned" rule would
  silently defeat that pin and trigger large rebuilds), and with one linked
  input today it is speculative machinery. If writing paths becomes the real
  pain, add it later as a separate explicit key, not as an overloaded value
  type.
- **An `xi link` subcommand.** The only thing it bought was discoverability,
  which `xi doctor` plus the per-invocation report already cover. See the naming
  section.
- **The Nix registry, or `--override-flake`.** Measured not to beat
  `flake.lock`. See the table above.
- **Persisting with `nix flake lock --override-input`.** Writes a `path:` entry
  into a tracked file, pollutes every diff, and risks pushing a machine path.
- **Committing a relative path input in `flake.nix`.** Measured to be impossible
  when the target is outside the tree: relative inputs resolve against the store
  path, giving
  `access to absolute path '/nix/store/b2/flake.nix' is forbidden in pure
  evaluation mode`,
  and `--impure` only changes the error. It works only when the target is inside
  the same repo.
- **A second `dev/flake.nix` with local inputs.** Needs its own lock file and
  duplicates the input list, so it drifts from `flake.nix` by construction.
- **A shell wrapper or alias around `nix`.** xi already is that wrapper
  (`modules/shared/lib/nix-wrapper.nix` installs a `nix` shim that execs
  `xi nix "$@"`), so a second layer would be redundant and invisible.
