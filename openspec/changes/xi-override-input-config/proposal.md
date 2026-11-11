# Change: xi-override-input-config

## Why

`docs/explanation/xi-spec.md:61` states that every setting follows
`CLI flag > Environment variable > config.toml > Built-in default`, and that "No
exceptions. The mapping is predictable." `--override-input` is the one
passthrough argument that breaks that promise: it exists as a CLI flag
(`crates/xi-core/src/args.rs:295`, emitted at `:382`) but has no environment
variable and no config key. `docs/reference/cli.md:537` records the gap
literally, with an empty placeholder in the env column.

The practical cost shows up in multi-flake local development. This repo pins
`nix-lib` to `github:Dauliac/nix-lib`, and the local clone lives at
`~/ghq/github.com/Dauliac/nix-lib`. Working on both repos at once today means
retyping `--override-input nix-lib path:/absolute/path/to/nix-lib` on every
single build, check, develop and run. There is no way to persist it.

That is not a gap we can close outside xi, because Nix offers no alternative.
Measured on nix 2.34.8 with isolated test flakes:

| mechanism                          | overrides an input already in `flake.lock`?                                                                                                                 |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `nix registry add` (user registry) | No. The user registry is not consulted for flake inputs at all: `nix flake lock` still reports `error: cannot find flake 'flake:b' in the flake registries` |
| global `flake-registry` file       | No. It resolves an unlocked input, but once locked the lock wins and changes are ignored                                                                    |
| `--override-flake`                 | No. It loses to the lock, and when no lock exists it writes one                                                                                             |
| `--override-input`                 | Yes. The only mechanism that wins, and it is documented to imply `--no-write-lock-file`                                                                     |

So `--override-input` is not one option among several, it is the only one. It is
also the safe one: `flake.lock` was byte-identical after every override run,
including without passing `--no-write-lock-file`.

Nix additionally leaves three traps that only a wrapper can close, all verified
against this repo's own `nix-lib` input:

1. **A bare path silently becomes a different fetcher.**
   `--override-input nix-lib /abs/path` locks as
   `{"type":"git","rev":"cb3f79a","ref":"refs/heads/main"}`, pinned to the last
   commit, while `path:/abs/path` locks as `{"type":"path"}` reading the live
   working tree. The git form sees uncommitted edits to tracked files but
   **hard-errors** on untracked or gitignored files with
   `error: Path 'extra.nix' in the repository "..." is not tracked by Git.`,
   which is exactly the file you just created in the sibling repo.
2. **Relative paths resolve against the current working directory**, not the
   flake, so the same override means different things depending on which
   subdirectory you ran it from. In a worktree-heavy checkout this is worse than
   useless: from `.claude/worktrees/<name>`, `../nix-lib` points at
   `.claude/worktrees/nix-lib`.
3. **A literal space breaks flakeref parsing entirely.** `path:` is parsed as a
   URL, so a path containing a space stops being recognised and nix reports
   `error: getting status of "<cwd>/path:/home/.../sp test/b"`, having prepended
   the CWD to the literal string `path:`. Percent-encoding (`sp%20test`) works.
   The error is unguessable from the symptom.

## What Changes

- Fill the cascade hole with the mechanical names the spec's own rule produces:
  `--override-input` / `XI_OVERRIDE_INPUT` / `build.override_input`.
- `build.override_input` becomes a map of input name to flakeref, readable from
  `~/.config/xi/config.toml` and from per-project `.xi.toml` / `.xi.local.toml`.
- `XI_OVERRIDE_INPUT` accepts whitespace-separated `name=flakeref` pairs, so
  multiple inputs need no new syntax and multi-line `.envrc` export works.
- Values that are filesystem paths are resolved by xi to absolute,
  percent-encoded, `path:`-prefixed flakerefs. Relative values resolve against
  the context the value was written in, not the process CWD.
- Config-derived overrides are filtered against the current flake's actual
  inputs, so a machine-global entry does not emit
  `warning: the flag '--override-input ...' does not match any input` in every
  unrelated repo.
- Overrides are suppressed, with notice, on lock-writing flake subcommands,
  because `--override-input` implies `--no-write-lock-file` and would otherwise
  turn `xi nix flake update` into a silent no-op.
- Every applied override is reported on one line per invocation, with the
  resolved absolute path.
- No new subcommand and no new CLI flag. The flag already exists.

## Impact

- **Affected specs**: new capability `xi-override-input-config`.
- **Affected code**: `crates/xi-core/src/args.rs` (a `BTreeMap` field, the
  name-keyed merge, and `apply_build_defaults` at `:396` which is currently
  `pub const fn` and cannot allocate), `crates/xi/src/config.rs:55`
  (`BuildConfig` gains the key), `crates/xi-flake/src/project_config.rs:166` (a
  `[build]` section, which `ProjectConfig` does not have yet), and the emission
  path in `crates/nix-command/src/lib.rs` where the `CommandKind` at `:33` gates
  which commands may receive the flag.
- **Affected docs**: `docs/reference/cli.md:537` (fill the env column), the
  `[build]` table at `docs/reference/configuration.md:32`, and the mapping table
  at `docs/explanation/xi-spec.md:61`.
- **Breaking?**: No. With no config key and no env var set, behaviour is
  byte-identical to today.
- **Security and footgun note**: a config file that silently redirects a build
  input is a real hazard, which is why the one-line-per-override report and the
  "never commit `.xi.local.toml`" guidance are requirements here and not polish.
- **Deliberately out of scope**: deriving the local path automatically from the
  input's own URL plus a clone root (for example mapping
  `github:Dauliac/nix-lib` to `$(ghq root)/github.com/Dauliac/nix-lib`). It was
  considered and rejected for this change, see `design.md`.

## Success Criteria

- **SC-001**: With
  `override_input = { nix-lib = "~/ghq/github.com/Dauliac/nix-lib" }` in
  `~/.config/xi/config.toml` and nothing else set, `xi nix build .#xi` builds
  against the local clone, and `nix flake metadata` style inspection shows the
  input locked as `{"type":"path"}` rather than `{"type":"git"}`.
- **SC-002**: `flake.lock` is unchanged on disk after any command that applied
  an override, verified by hash comparison before and after.
- **SC-003**: `XI_OVERRIDE_INPUT="a=./x b=../y"` overrides both inputs. The same
  value written across multiple lines behaves identically.
- **SC-004**: A relative value resolves against the context it was written in:
  the config file's own directory for a file value, the flake root for
  `XI_OVERRIDE_INPUT`, and the process CWD for a value typed as a CLI flag,
  matching nix exactly in that last case.
- **SC-005**: A path containing a space is percent-encoded before being prefixed
  with `path:`, and the resulting build succeeds rather than failing with the
  CWD-prepended parse error.
- **SC-006**: A local directory value always reaches nix as `path:<abs>`, never
  as a bare path, so a newly created untracked file in the overridden flake is
  visible instead of raising the not-tracked-by-Git error.
- **SC-007**: Precedence holds per input name: with `nix-lib` and `nix-vm-test`
  both set in config, passing `--override-input nix-lib <other>` replaces only
  `nix-lib` and leaves `nix-vm-test` applied.
- **SC-008**: An override naming an input absent from the current flake is
  dropped before invocation, and nix emits no does-not-match warning.
- **SC-009**: `xi nix flake update` and `xi nix flake lock` apply no overrides,
  say so on stderr, and still write `flake.lock`.
- **SC-010**: A value pointing at a missing directory, or at a directory with no
  `flake.nix`, fails with an error naming both the value as written and the
  absolute path it resolved to.
- **SC-011**: Every applied override is reported once per invocation with its
  resolved absolute path.
- **SC-012**: With no override configured anywhere, the generated nix argv is
  identical to today's, proving the change is inert by default.
