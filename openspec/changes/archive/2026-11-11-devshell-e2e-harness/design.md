# Design: devshell-e2e-harness

## Context

The devshell's behaviour is distributed across four processes: the user's shell,
the `xi develop prompt` invocation it evals every prompt, the background daemon,
and `nix`. A test that stays inside one Rust process can only ever assert on
messages between them, never on the behaviour. The design question is therefore
not "how do we mock this" but "how small can we make the real world and still
have it be real".

Three constraints shape every decision below:

1. **`dirs.rs` is a module of free functions**, called from `prompt.rs`,
   `lifecycle.rs`, `client.rs`, `server.rs`, and `exec.rs`. Its inputs come from
   the process environment. Any seam that is not an environment variable means
   threading a parameter through all of those call sites.
2. **The Rust test harness is multithreaded**, and `std::env::set_var` is
   `unsafe` in edition 2024 (as the crate already acknowledges at
   `exec.rs:36-45`). In-process environment mutation is therefore not available
   as an isolation mechanism. Isolation must happen at the `Command` boundary.
3. **`exec::exec` replaces the process** and subshell cleanup relies on process
   environment isolation. Some behaviour has no in-process representation at
   all.

Together these force one conclusion: the harness drives real child processes
with explicitly-built environments, and never mutates its own.

## Decision 1: runtime-dir override as an environment variable

`dirs::runtime_dir()` (`dirs.rs:83`) hardcodes `/tmp/xi-{uid}` on unix. It must
become overridable, because the daemon socket and `daemon.pid` are the only
state a test cannot currently sandbox.

**Chosen**: consult an explicit `XI_RUNTIME_DIR` environment variable first on
all platforms, falling back to today's `/tmp/xi-{uid}` on unix and to the
existing `XDG_RUNTIME_DIR` path on non-unix.

Rationale:

- It matches the convention the codebase already set with `XI_NIX_BIN`
  (`nix-command/src/lib.rs:266`), including the "explicit override wins" shape.
- The harness is process-level regardless, so an environment variable costs it
  nothing in expressiveness.
- It is one function body changed, versus roughly ten call sites for a
  parameter-threading approach.

**Rejected: make `runtime_dir` take an `&EnvSource`.** The `EnvSource` pattern
at `timeouts.rs:139-160` is the right house style for _layered configuration_
with a `.xi.toml` tier underneath. The runtime dir has no config tier, no
layering, and no in-process consumer that would benefit. Adopting it here buys
only ceremony.

**Rejected: derive the runtime dir from `XDG_STATE_HOME`.** Tempting (one fewer
variable to set) but wrong: the runtime dir is deliberately a different kind of
directory, on a different filesystem, with different lifetime and permissions.
Collapsing them would be a real behaviour change for users, not a test seam.

**Consequence to document in the spec**: the override must be validated or at
least constrained, because a socket path has a hard length limit (`sun_path`,
108 bytes on Linux). A long `TempDir` path plus `/{flake_id}/daemon.sock` can
exceed it. The harness therefore uses a short temp root, and the implementation
should produce a clear error rather than an opaque `bind` failure.

## Decision 2: one deep `TestWorld` module, not a pile of helpers

The harness gets a single type with a small interface and all of the setup
complexity behind it.

```
TestWorld::builder()
    .flake(FlakeFixture)      // which fixture tree to materialise
    .nix(FakeNix)             // staged responses for successive calls
    .trusted(bool)            // pre-write the trust marker, or not
    .build() -> TestWorld
```

and on the world itself, roughly:

```
world.env()                   -> the (key, value) pairs every child gets
world.prompt(shell, cwd)      -> captured stdout/stderr/exit of `xi develop prompt`
world.eval_in(shell, script)  -> resulting environment after a real shell evals it
world.shell(shell)            -> a live PTY-backed interactive shell handle
world.wait_for(Observable)    -> poll until an observable condition holds
world.nix_calls()             -> the recorded fake-nix invocations
```

Three properties matter more than the exact signatures:

- **Nothing leaks.** The world owns one `TempDir`; `HOME`, `XDG_STATE_HOME`,
  `XDG_CONFIG_HOME`, and `XI_RUNTIME_DIR` all point inside it. Dropping the
  world removes everything. This is what `SC-H-001` asserts.
- **The environment is data, not ambient state.** `world.env()` returns pairs
  applied per `Command`. The harness never calls `set_var`, so tests are
  thread-safe and can run concurrently (`SC-H-003`).
- **The binary under test is pinned by file, not by `PATH`.** The world writes
  `env!("CARGO_BIN_EXE_xi")` into `$XDG_STATE_HOME/xi/develop/xi-bin`, which
  both `lifecycle::resolve_xi_bin` (`lifecycle.rs:242`) and
  `prompt::resolve_xi_bin` (`prompt.rs:503`) already read before falling back to
  `current_exe()`. Every daemon the hook spawns, and every `__XI_BIN` baked into
  a spawn command by `subshell::generate_spawn_command`, is then provably the
  build under test. `scripts/devshell-test.sh:62` uses `PATH` prepending for
  this; the `xi-bin` file is exact where `PATH` is merely likely.

The world must also stop daemons on drop. A leaked daemon holding a socket in a
deleted temp directory is a slow-burning source of cross-test interference.
`lifecycle::stop` exists (`lifecycle.rs:117`) and the drop path should use it,
with a SIGKILL fallback for a wedged process.

## Decision 3: fake nix is a scripted fixture, not a stub

`XI_NIX_BIN` points at a committed executable under
`crates/xi-develop/tests/fixtures/`. Its contract:

- **Reads its instructions from the world**, not from its own source. A
  directory in the world (say `fake-nix/`) holds an ordered list of responses
  plus a call log. Call _n_ returns response _n_, with the last response
  repeating. This is what lets one test stage "success, then a changed
  `shellHook` and `PATH`" to drive exactly one re-source (`SC-H-006`), and
  another stage "success, then `error:`" to drive the degraded path
  (`SC-H-005`).
- **Records every invocation** (argv, cwd) to the call log, so tests can assert
  that `--profile` was passed, that `--option connect-timeout 5` is present, and
  that `current_dir` was the flake root. These are contract details of
  `eval_devshell_niced` (`env_file.rs:349`) and `run_nix_eval`
  (`server.rs:1350`) that nothing currently pins.
- **Honours `--profile <path>`** by creating a plausible profile symlink.
  Skipping this silently bypasses the GC-root logic and `capture_dix_diff`
  (`server.rs` circa :1390), which reads profile paths through `dix`. A fake
  that is too lazy here makes tests pass while the real path stays unexercised.
- **Emits plausible `/nix/store/<hash>-<name>-<version>` paths**, because
  `store_path.rs` parses them into `PackageInfo` and the daemon reports those to
  users.
- **Can hang or be slow on demand** (a `sleep` driven by its instructions), so
  timeout and backoff paths are reachable without waiting on real nix.

The response format is the narrow subset `DevEnv::from_nix_json`
(`env_file.rs:92`) actually consumes: a `variables` object where only
`type == "exported"` string values are read, `PATH` is split on `:`, non-blank
`shellHook` is kept, and `SKIP_VARS` (`env_file.rs:9`) are discarded. A fixture
only needs to be faithful to that subset.

**The known risk**: fixture drift. If real nix changes its output shape, the
fast suite keeps passing against a stale fixture. Mitigation is a single
nix-backed check, outside the fast suite, that compares fixture shape against
one real `print-dev-env --json`. Accepting fixture drift risk in exchange for a
suite that runs without nix is a deliberate trade, and it is cheap to detect
because the surface is one JSON shape, not a protocol.

## Decision 4: three ways to run a shell, chosen by what is being tested

| Need                                                              | Mechanism                                                                                    | Why not the cheaper one                                                                                                                                                     |
| ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Does `prompt` emit the right instructions?                        | no shell; assert on captured stdout                                                          | a shell adds nothing; this is a pure function of (cwd, env, pid, shell)                                                                                                     |
| Does a real shell execute those instructions correctly?           | `bash -c` / `zsh -c` / `fish -c` on the captured script, then dump the resulting environment | asserting on script _text_ cannot see `NO_CLOBBER`, quoting, or arithmetic-expansion defects                                                                                |
| Do prompt hooks, exit codes, and parent/child env isolation work? | PTY-backed interactive shell                                                                 | non-interactive bash does not run `PROMPT_COMMAND` as interactive bash does; zsh `precmd` needs `-i`; `exit 200` to parent `__xi_status` needs a real parent and child pair |

For L3, the environment dump uses a NUL-delimited `env` read so values
containing newlines (`shellHook` routinely does) survive the round trip.

For L4, synchronisation uses a **prompt sentinel**, not timing: the harness sets
a unique `PS1`, writes a command, and reads until the sentinel reappears. That
is the only reliable "the shell finished the previous command and ran its prompt
hook" signal, and it is what makes `SC-H-010` (no bare sleeps) achievable.

## Decision 5: no virtual clock; poll observables instead

`server.rs` reads `Instant::now()` on nearly every path (`:153`, `:154`, `:331`,
`:449`, `:540`, `:571`, `:734`, `:838`, `:884`), `lifecycle::ensure`
(`lifecycle.rs:32`) polls with sleeps, and `watcher.rs:98` blocks on a timeout.
Making time injectable means touching all of it.

The payoff would be determinism, but condition-polling delivers the same
determinism for far less, because the daemon already publishes exactly the state
transitions tests care about:

- `DaemonState::Ready` via the status query (`client::status`)
- env and hook generation counters (`dirs::bump_env_generation`,
  `dirs::bump_hook_generation`, `dirs.rs:226-233`)
- the A/B slot files and the `current` symlink (`dirs.rs:142-188`), including
  `active_slot` and `env_hook_files_exist`
- `meta.json` (`meta::load`, consumed at `ServerState::new`, `server.rs:122`)
- the fake-nix call log, which is a direct observable of "an eval happened"

A test that polls "env generation went from 1 to 2, deadline 10s" is both
deterministic and honest about what it is waiting for. A test that sleeps 500ms
is neither. The rule in the spec is therefore phrased as a prohibition on sleeps
as synchronisation, which is enforceable by review and by grep, rather than as a
clock-abstraction requirement.

Where timing genuinely must be compressed, the existing knobs do it: the
`--eval-interval` flag the daemon is already launched with
(`lifecycle.rs:89-92`), `.xi.toml` `[develop.timeouts]`, and the environment
tier that `Timeouts::resolve` overlays on top (`timeouts.rs:111`).

## Decision 6: a layer boundary rule, so the suite does not rot

Every e2e suite drifts toward "put it in the slowest layer, it works there". The
spec encodes the counter-pressure as a rule: a behaviour is tested at the
**cheapest layer that can observe it**, and a test may only live in L4 if it
depends on at least one of (a) a prompt hook firing, (b) a process exit code
crossing a parent/child boundary, or (c) environment isolation between two live
shells. Nothing else qualifies.

This keeps `SC-H-011` (L2 plus L3 under 10 seconds) reachable, and it keeps L4
small enough that its tty and timing dependencies stay manageable.

## The acceptance test for the harness itself

`SC-H-004` is deliberately a meta-criterion: revert the `>|` back to `>` in
`subshell::generate_cd_out_exit` (`subshell.rs:46-52`) and a named test must
fail. That one character pair is a defect this code actually shipped, that the
current 18-file suite cannot see, and that is the entire reason this change
exists. If the harness cannot catch it, the harness is not done, regardless of
how many scenarios pass.

## Open questions

- **PTY crate choice** (`portable-pty`, `expectrl`, `rexpect`). Decided at
  implementation time against: maintenance, whether it supports reading until a
  sentinel without a built-in timing assumption, and whether it works in the nix
  build sandbox. This does not affect any requirement.
- **Where the harness lives.** A `tests/harness/` module shared by test binaries
  duplicates compilation per binary; a `xi-develop-testkit` dev-only crate
  compiles once and can be reused by other crates later. Lean testkit crate if
  more than three test binaries end up using it.
- **Whether L4 runs in `nix flake check`.** Depends on tty availability in the
  sandbox. The fallback (a non-sandboxed CI job) is acceptable and is already
  reflected in the Impact section of the proposal.
