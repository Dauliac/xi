# Change: devshell-e2e-harness

## Why

The devshell is the one xi surface whose correctness lives in generated shell
code running inside a real interactive shell, driven by a background daemon over
a Unix socket. Everything that has actually broken in it broke there: zsh's
`NO_CLOBBER` silently dropping the cd-out `pwd` write (fixed by `>|` in
`subshell.rs:46-52`), a stale `__XI_SUPPRESS` blocking re-activation with no
message (worked around by hand in `scripts/devshell-test.sh:48-55`),
depth-scoped cleanup of injected vars across nesting levels.

None of that class of bug is reachable from the current test suite. `xi-develop`
has 18 `bdd_*.rs` files and they are almost all pure unit tests: only
`bdd_client_v3.rs` and `bdd_migration.rs` touch a socket or a process, and the
crate's entire `[dev-dependencies]` is `tempfile`. No test has ever executed a
line of the shell code the crate emits.

The only end-to-end coverage that exists is `scripts/devshell-test.sh`, which is
manual, interactive, must be `source`d into a human's own shell, mutates that
shell's `PATH` and prompt hook, and depends on a working `nix` plus a populated
eval cache. It is a good debugging aid. It is not a test: it asserts nothing, it
cannot run in CI, and it cannot reach failure paths (nix eval error, nix hang,
inotify exhaustion) on demand.

The gap is not a missing test file. It is a missing harness. Three of the four
seams such a harness needs already exist in the code; this change inventories
them, closes the one that is missing, and builds the harness on top.

## Verified Against Current Code (2026-11-10)

Read in full: `dirs.rs`, `activate.rs`, `subshell.rs`, `exec.rs`, `env_file.rs`,
`prompt.rs`, `trust.rs`, `daemon/lifecycle.rs`, `daemon/watcher.rs`,
`timeouts.rs`, plus `daemon/server.rs` entry points and `nix-command/src/lib.rs`
binary resolution.

- **Virtual state/config dirs already work.** `dirs.rs` resolves everything
  through the environment: `XDG_STATE_HOME` (`dirs.rs:68`), `XDG_CONFIG_HOME`
  (`dirs.rs:76`), and `HOME` (`dirs.rs:311`, falling back to `/tmp`). Setting
  those three relocates the per-flake state tree, the A/B env/hook slots, the
  generation counters, the nix profile GC roots, and the trust markers.

- **Virtual runtime dir does NOT work.** `dirs::runtime_dir()` (`dirs.rs:83`) is
  hardcoded to `/tmp/xi-{uid}` on unix via `nix_uid()` (`dirs.rs:315`). The
  `XDG_RUNTIME_DIR` branch below it is `#[cfg(not(unix))]` and is dead on Linux
  and macOS. Consequence: the daemon socket (`dirs.rs:109`) and `daemon.pid`
  (`lifecycle.rs:18`) live in a process-global namespace that no test can
  redirect or clean up by dropping a `TempDir`. Collisions are unlikely because
  `flake_id` is a SHA-256 of the canonical flake path and test flakes live in
  unique temp paths, but the leak is real and a concurrently running real daemon
  for the same path would be indistinguishable from the test's own.

- **Virtual nix already works, and is the cheapest high-value seam.**
  `nix_command::find_real_nix_binary()` (`nix-command/src/lib.rs:256`) honours
  an explicit `XI_NIX_BIN` override (`nix-command/src/lib.rs:266`) provided the
  value is an existing file, and both nix eval call sites go through it:
  `env_file::eval_devshell_niced` (`env_file.rs:349`) and the daemon's
  `run_nix_eval` (`server.rs:1350`). Both invoke
  `print-dev-env <installable> --json --option connect-timeout 5 --option
  download-attempts 2`,
  optionally `--profile <path>`, with `current_dir(flake_root)`. The consumed
  format is narrow and documented by `DevEnv::from_nix_json` (`env_file.rs:92`):
  a `{"variables": {KEY: {"type":
  ..., "value": ...}}}` object, where only
  `type == "exported"` string values are read, `PATH` is split on `:` into
  `nix_paths`, a non-blank `shellHook` becomes `shell_hook`, and `SKIP_VARS`
  (`env_file.rs:9`) are dropped.

- **The shell-side contract is already minimal and already a pure function.**
  `activate.rs` emits three non-comment lines per shell. The whole per-prompt
  interaction is `eval "$("$__XI_BIN" develop prompt -s bash --pid $$)"`, so the
  devshell is, from the shell's point of view, a function from (cwd,
  environment, pid, shell type) to a script to evaluate. `prompt.rs:44`
  dispatches on `__XI_IN_DEVSHELL` into `handle_parent` (`prompt.rs:71`) or
  `handle_subshell` (`prompt.rs:171`). This is what makes a harness tractable at
  all.

- **The xi binary under test is already overridable.** Both
  `lifecycle::resolve_xi_bin` (`lifecycle.rs:242`) and `prompt::resolve_xi_bin`
  (`prompt.rs:503`) read `$XDG_STATE_HOME/xi/develop/xi-bin` before falling back
  to `current_exe()`. Writing the test binary's path into that file pins every
  daemon the hook spawns and every `__XI_BIN` baked into a spawn command to the
  build under test, with no `PATH` manipulation. `scripts/devshell-test.sh:62`
  currently achieves the same thing by prepending to `PATH`, which is coarser.

- **There is no clock seam, and the daemon is wall-clock bound.** `server.rs`
  reads `Instant::now()` at construction and on essentially every code path
  (`server.rs:153`, `:154`, `:331`, `:449`, `:540`, `:571`, `:734`, `:838`,
  `:884`). `lifecycle::ensure` (`lifecycle.rs:32`) polls `STARTUP_RETRIES` times
  with a `STARTUP_WAIT_MS` sleep and sleeps a flat 200 ms on daemon version
  mismatch. `watcher.rs:98` blocks on `try_recv(timeout)`. Eval backoff is
  driven by `last_error_time`. None of this is injectable.

- **A dependency-injection pattern for environment lookup already exists in this
  crate.** `timeouts.rs:133-160` defines `EnvSource` as a boxed
  `Fn(&str) -> Option<String>` with three constructors: `process()`, `empty()`,
  and `from_pairs()` for fixtures. `Timeouts::load` layers `.xi.toml`
  `[develop.timeouts]` under the process env. This is the house style to extend,
  not invent.

- **Some behaviour is irreducibly process-level.** `exec::exec` replaces the
  process with `CommandExt::exec()` (`exec.rs:53-56`), and subshell cleanup
  depends on real process environment isolation (`subshell.rs:15-20`). Neither
  can be exercised in-process, which fixes the harness shape: spawn the real
  binary and a real shell inside a sandboxed world.

- **Prior art for binary-level tests exists in the workspace.**
  `crates/xi-sources-daemon/tests/ctl_subcommand.rs:80-83` spawns
  `env!("CARGO_BIN_EXE_xi-sources-daemon")` with a `TempDir`-backed state dir
  via `.env(...)`. The same pattern applies here.

- **Fixtures exist.** `crates/xi-develop/tests/features/` holds the BDD feature
  files the `bdd_*.rs` tests reference, and
  `crates/xi-develop/tests/flake-module/` is a nested-flake fixture already used
  by `bdd_nested_flakes.rs`.

## What Changes

Introduce capability `xi-devshell-test-harness`, owning a four-layer test
pyramid for the devshell, plus the two code changes the harness needs.

### Code changes required

- **Overridable runtime dir.** `dirs::runtime_dir()` MUST consult an explicit
  override before falling back to `/tmp/xi-{uid}`, so the daemon socket and pid
  file can be placed inside a test's `TempDir`. This is the one missing seam.
- **Fake-nix fixture.** A committed, executable fixture script that answers
  `print-dev-env --json` with canned output, selected by `XI_NIX_BIN`,
  scriptable into the failure modes real nix cannot be asked for on demand.

### Harness layers

- **L2, script-contract tests.** Run `xi develop prompt` as a subprocess in a
  virtual world (temp `HOME`/`XDG_STATE_HOME`/`XDG_CONFIG_HOME`/runtime dir,
  fake nix, `xi-bin` pin) and assert on the emitted shell text. No shell
  involved. Covers trust gating, suppress set and clear, flake detection, the
  `flake_likely_has_devshell` skip, and parent-versus-subshell dispatch.
- **L3, real-shell evaluation tests.** Feed the scripts L2 captures to a real
  `bash -c` / `zsh -c` / `fish -c` and assert on the resulting environment.
  Catches quoting, syntax, and `NO_CLOBBER`-class defects that no Rust-side
  assertion on script text can see.
- **L4, PTY end-to-end tests.** Drive a real interactive shell under a
  pseudo-terminal through the full lifecycle: activate, enter on cd, re-eval on
  flake change, nested flake entry, cd-out, explicit exit, suppress, re-entry.
  Required because non-interactive bash does not run `PROMPT_COMMAND` the way
  interactive bash does, zsh `precmd` needs `-i`, and the `exit 200` to parent
  `__xi_status` chain only exists with a real parent and child shell pair.
- **Synchronisation discipline.** Tests MUST NOT sleep for a fixed duration to
  wait on daemon work. They MUST poll an observable: `DaemonState::Ready` via
  the status query, the env and hook generation counters
  (`dirs::bump_env_generation`, `dirs::bump_hook_generation`), the A/B slot
  files and `current` symlink (`dirs.rs:142-188`), or `meta.json`.

### Explicit non-goals

- **No clock abstraction.** Threading a `Clock` trait through `server.rs` is a
  large invasive refactor for little return once condition-polling is in place.
  Deliberately deferred.
- **No fake shells.** A Rust-side simulated shell that parses the emitted script
  and mutates a `HashMap` is explicitly rejected as the e2e mechanism: it cannot
  catch the defect class that has actually shipped. L2 covers the script-content
  contract; real shells cover execution.
- **No real nix in L2 through L4.** Real-nix coverage stays where it is today,
  in the nix checks under `nix/`.
- **No change to `scripts/devshell-test.sh`.** It remains the interactive
  debugging aid. The harness does not replace it.

## Impact

- **Affected specs**: `xi-devshell-test-harness` (new capability).
- **Affected code**:
  - `crates/xi-develop/src/dirs.rs`: `runtime_dir()` gains an override. This is
    the only production behaviour change in the whole proposal.
  - `crates/xi-develop/Cargo.toml`: `[dev-dependencies]` grows beyond `tempfile`
    (a PTY crate for L4, plus whatever the harness crate needs).
  - New: `crates/xi-develop/tests/harness/` (shared world builder),
    `crates/xi-develop/tests/fixtures/fake-nix` (executable),
    `crates/xi-develop/tests/e2e_*.rs`.
- **Related specs**: `xi-devshell-activation` (owns the activation contract L2
  and L3 assert against), `xi-devshell-trust` (trust gating), and
  `xi-devshell-watcher` (the re-eval trigger L4 exercises) all gain an
  executable verification route. This change does not alter their requirements.
- **Breaking?** No. The `runtime_dir()` override is additive and defaults to
  today's `/tmp/xi-{uid}` when unset.
- **Migration**: None.
- **CI cost**: L2 and L3 are millisecond-scale and run on every commit. L4 is
  slower and tty-dependent; it is gated behind a nextest profile or `#[ignore]`
  and runs on a schedule, not per commit.

## Success Criteria

- **SC-H-001**: With `HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, and the
  runtime-dir override all pointed into one `TempDir`, a full L4 run leaves no
  file outside that directory. Specifically, no `daemon.sock` and no
  `daemon.pid` appear anywhere under `/tmp/xi-{uid}/`.
- **SC-H-002**: A complete L2 or L3 test file runs to completion with no `nix`
  binary on `PATH` at all, and no network access.
- **SC-H-003**: Two L4 tests running concurrently against two different flake
  fixtures neither share a daemon nor interfere with each other's state, and
  neither is affected by a real xi daemon running on the developer's machine.
- **SC-H-004**: The `>|` versus `>` zsh `NO_CLOBBER` cd-out defect
  (`subshell.rs:46-52`) is reproducible by reverting that one character pair and
  observing a named L3 or L4 test fail. This is the harness acceptance test: if
  it cannot catch the bug that motivated it, it is not done.
- **SC-H-005**: A fake nix that exits non-zero with an `error:` payload drives
  the daemon to its error state, surfaces a notification, and leaves the
  previously good env files intact and still sourceable.
- **SC-H-006**: A fake nix that returns a different `shellHook` and a different
  `PATH` on its second invocation causes exactly one re-source in the running
  subshell, observable as an env generation counter increment.
- **SC-H-007**: An L4 nesting test confirms that entering a nested flake sets
  `__XI_DEPTH=2`, that exiting it restores the depth-1 environment exactly
  (injected vars from depth 2 removed, depth-1 vars untouched), and that the
  parent shell survives.
- **SC-H-008**: An L4 cd-out test confirms that leaving the flake root exits the
  subshell with code 200 and leaves the parent shell in the directory the user
  cd'd to, not the pre-entry directory.
- **SC-H-009**: An L4 explicit-exit test confirms `__XI_SUPPRESS` is set to the
  flake root, that re-entry does not occur while cwd stays under it, and that
  cwd moving away clears the variable.
- **SC-H-010**: No test in the suite contains a bare sleep used as a
  synchronisation primitive. Waiting is expressed as polling an observable with
  a deadline.
- **SC-H-011**: Total wall-clock for L2 plus L3 stays under 10 seconds on a
  developer machine, so they are cheap enough to run on every commit.

## Assumptions

- `bash` and `zsh` are available in the devshell used for CI. `fish` may not be;
  fish coverage degrades to L2 and L3 only, and L4 fish tests skip rather than
  fail when the binary is absent.
- A pseudo-terminal is available in the CI sandbox. If not, L4 runs only locally
  and in a non-sandboxed CI job, and the gating mechanism from the Impact
  section is what makes that acceptable.
- `nix print-dev-env --json`'s output shape is stable enough that a canned
  fixture stays faithful. The risk of fixture drift is real and is mitigated by
  keeping one nix-backed check that compares fixture shape against real nix
  output, outside the fast suite.
- The fake nix must honour `--profile <path>` by creating a plausible profile
  symlink, and must emit plausible `/nix/store/...` paths, because
  `store_path.rs` parses those into `PackageInfo` and `server.rs`'s
  `capture_dix_diff` reads profile paths through `dix`. A fake that skips this
  silently bypasses real code paths.
- `flake_id` being a hash of the canonical flake path means temp-dir fixtures
  get unique ids for free; the harness does not need to control `flake_id`
  directly.
- The `EnvSource` pattern in `timeouts.rs` is the accepted house style for
  injectable lookups, so the `runtime_dir()` override follows it rather than
  introducing a second convention.
- Timing-sensitive behaviour (coalescing windows, eval intervals, backoff) is
  configurable low enough through `.xi.toml` and the existing timeout env vars
  that L4 does not need a virtual clock. If a specific scenario proves
  unreachable without one, that scenario is dropped rather than the clock seam
  being added under this change.
