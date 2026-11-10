# devshell end-to-end suite

Spec: `openspec/changes/devshell-e2e-harness/` (capability
`xi-devshell-test-harness`)

The devshell is the one xi surface whose correctness lives in generated shell
code, running inside a real interactive shell, driven by a background daemon
over a Unix socket. This suite exists because that whole class of defect was
previously unreachable from tests. The defect that motivated it: zsh's
`NO_CLOBBER` silently dropping the cd-out `pwd` write, so the parent shell could
not follow the user out of a flake.

## The layers, cheapest first

| Layer            | File                                               | Needs                 | Runs                      |
| ---------------- | -------------------------------------------------- | --------------------- | ------------------------- |
| Fixture contract | `../../xi-develop/tests/bdd_fake_nix.rs`           | nothing               | always                    |
| Runtime-dir seam | `../../xi-develop/tests/bdd_runtime_dir.rs`        | nothing               | always                    |
| Script contract  | `e2e_devshell_prompt.rs`                           | the `xi` binary       | always                    |
| Real shell       | `e2e_devshell_shell_eval.rs`                       | `bash`, `zsh`, `fish` | always, per shell present |
| Interactive      | `e2e_devshell_interactive.rs`                      | a pseudo-terminal     | opt-in                    |
| Discipline       | `e2e_devshell_discipline.rs`                       | nothing               | always                    |
| Fixture drift    | `../../xi-develop/tests/nix_backed_shape_check.rs` | real nix, network     | `--ignored`               |

```bash
# fast layers (what CI runs on every commit)
cargo test -p xi-develop
cargo test -p xi

# the interactive pseudo-terminal layer
XI_E2E_PTY=1 cargo test -p xi --test e2e_devshell_interactive

# the real-nix fixture-drift tripwire
cargo test -p xi-develop --test nix_backed_shape_check -- --ignored
```

## Rule 1: test at the cheapest layer that can observe the behaviour

A test may live in the **interactive** layer only if it depends on at least one
of:

- a prompt hook firing,
- a process exit status crossing a parent/child shell boundary,
- environment isolation between two live shells.

Everything else goes in a cheaper layer. `e2e_devshell_discipline.rs` enforces
this by asserting the cheap layers keep more tests than the interactive one, and
by asserting every interactive test consults the opt-in gate.

Why it matters: the fast layers run in about two seconds. Each behaviour
promoted into the interactive layer that did not need to be there makes the
suite slower and more tty-dependent for no extra coverage.

## Rule 2: never sleep to synchronise

There is no fixed `sleep` anywhere in this suite, and
`suite_has_no_sleep_based_synchronisation` fails the build if one appears. Wait
on an observable with a deadline instead:

- `TestWorld::wait_for(what, deadline, probe)` for daemon state. Observables:
  env/hook generation counters, the A/B slot files, `meta.json`, the fake-nix
  call log, socket existence.
- `PtyShell::run` for interactive commands. It anchors on a unique per-command
  end marker and then the prompt sentinel.
- `PtyShell::wait_for_exit` for a cascading exit.

Where a test genuinely needs slowness, stage it in the fake nix
(`responses/NNN.delay`) so the delay is the subject under test, not a guess
about timing.

## Rule 3: the environment is data

The harness never calls `std::env::set_var`. It is `unsafe` in edition 2024 and
races across the multithreaded test harness. `TestWorld::env()` returns pairs
applied per `Command`, which is what makes concurrent worlds safe and is tested
by `concurrent_worlds_do_not_interact`.

## What the world gives you

`TestWorld` owns one `TempDir` with `HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`
and `XI_RUNTIME_DIR` inside it, so nothing escapes and dropping it reaps any
daemon it started. It pins the binary under test by writing `CARGO_BIN_EXE_xi`
into the `xi-bin` state file, which both `resolve_xi_bin` call sites consult
before `current_exe()`; `PATH` ordering is never relied on.

`nix` is faked via `XI_NIX_BIN`, pointing at the committed
`crates/xi-develop/tests/fixtures/fake-nix`. Stage responses with
`TestWorld::stage` or the builder's `.nix(&[...])`; successive calls advance
through the list and the last one repeats.

## Two traps that cost real debugging time

**The `__XI_BIN` pin must come AFTER the activation script.** The generated
script contains its own `export __XI_BIN="$(command -v xi)"`. Prepending the pin
lets the script overwrite it, which silently runs the machine's _installed_ xi
instead of the build under test. That build has no `XI_RUNTIME_DIR` support, so
it escapes the sandbox entirely. Loud symptom, silent cause.

**Flake fixtures must be git repositories.** The daemon's watcher is git-aware:
`GitWatcher::new` opens the flake root as a repo and derives its watch set from
tracked files. A plain directory makes the watcher unavailable and the daemon
falls back to hook-triggered eval, so a re-eval test would pass while never
exercising the watcher. `write_flake` runs `git init` and `git add` for this
reason.

## What runs where

CI runs tests with `nix develop --command cargo test` (see
`.github/workflows/test.yaml`), not through the `memberTests` derivations in
`nix/crane.nix` and not through `nix flake check` (`test-xi` is not in
`checks`). So the devshell's package list is what decides which shells the
real-shell layer can actually use, which is why `pkgs.zsh` is in
`nix/devShells.nix`.

Current state:

- `bash` and `zsh` are both present in the devshell, so the real-shell layer
  including the `NO_CLOBBER` cd-out anchor genuinely executes in CI.
- `fish` is not in the devshell, so fish tests skip. Add `pkgs.fish` to
  `nix/devShells.nix` to close that gap.
- The interactive layer never runs in CI, by design: it needs a pseudo-terminal,
  and it is gated behind `XI_E2E_PTY=1`. Run it locally before trusting a change
  to the activation, nesting, or cd-out paths.
