# xi-devshell-test-harness Specification

## Purpose

TBD - created by archiving change devshell-e2e-harness. Update Purpose after
archive.

## Requirements

### Requirement: Overridable runtime directory

`dirs::runtime_dir()` MUST consult an explicit `XI_RUNTIME_DIR` environment
variable before any platform default, and MUST treat an empty value as unset.

#### Scenario: Override redirects socket and pid file

- **WHEN** `XI_RUNTIME_DIR` is set to an existing writable directory and a
  daemon is started for a flake
- **THEN** the daemon socket (`dirs::daemon_socket_path`) and the daemon pid
  file are created under that directory in a `{flake_id}` subdirectory, and no
  file is created under `/tmp/xi-{uid}/`

#### Scenario: Unset override preserves today's behaviour

- **GIVEN** `dirs::runtime_dir()` as of 2026-11-10 returns `/tmp/xi-{uid}` on
  unix via `nix_uid()`, with the `XDG_RUNTIME_DIR` branch compiled only under
  `#[cfg(not(unix))]`
- **WHEN** `XI_RUNTIME_DIR` is unset or empty on a unix host
- **THEN** the runtime directory is `/tmp/xi-{uid}`, byte-identical to the
  pre-change value

### Requirement: Socket path length validation

The daemon socket path MUST be validated against the platform `sockaddr_un` path
limit before bind, and an over-long path MUST produce a diagnostic naming the
limit rather than an opaque bind failure.

#### Scenario: Over-long socket path is diagnosed

- **WHEN** `XI_RUNTIME_DIR` is set such that the resulting socket path exceeds
  the platform `sun_path` limit
- **THEN** daemon startup fails with an error naming the path length and the
  limit, and does not leave a pid file behind

### Requirement: Hermetic test world

The harness MUST provide a single world abstraction that owns one temporary
directory and points `HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`, and
`XI_RUNTIME_DIR` inside it. The world MUST expose its environment as data
applied per child process. The harness MUST NOT call `std::env::set_var` to
establish isolation.

Dropping the world MUST terminate any daemon it started and remove every file it
created.

#### Scenario: No state escapes the world

- **WHEN** a full end-to-end scenario runs to completion inside one world
- **THEN** every file written by xi (per-flake state, A/B env and hook slots,
  generation counters, nix profile symlinks, trust markers, daemon socket,
  daemon pid file) lies within the world's temporary directory, and nothing
  appears under the real `$HOME`, the real `$XDG_STATE_HOME`, the real
  `$XDG_CONFIG_HOME`, or `/tmp/xi-{uid}/`

#### Scenario: Concurrent worlds do not interact

- **WHEN** two end-to-end tests run concurrently against two distinct flake
  fixtures in two distinct worlds
- **THEN** each drives its own daemon, neither observes the other's state
  transitions or notifications, and neither is affected by a real xi daemon
  running on the same host

#### Scenario: World drop reaps the daemon

- **WHEN** a world that started a daemon is dropped
- **THEN** the daemon process is terminated and its socket is removed, including
  when the daemon is unresponsive to a graceful shutdown request

### Requirement: Binary under test pinned by state file

The harness MUST pin the xi binary by writing the test binary's path to
`$XDG_STATE_HOME/xi/develop/xi-bin`, which both `lifecycle::resolve_xi_bin` and
`prompt::resolve_xi_bin` already consult before falling back to `current_exe()`.
The harness MUST NOT rely on `PATH` ordering to select the binary under test.

#### Scenario: Spawned daemon is the build under test

- **WHEN** the prompt hook causes `lifecycle::ensure` to start a daemon
- **THEN** the spawned process is the binary named in `xi-bin`, and the
  `__XI_BIN` value embedded in the generated spawn command is that same path

#### Scenario: A different xi on PATH is not used

- **WHEN** an unrelated executable named `xi` precedes the test binary on `PATH`
- **THEN** no xi behaviour under test resolves to it

### Requirement: Scriptable fake nix

The harness MUST supply an executable, selected through `XI_NIX_BIN`, that
answers `print-dev-env --json` from staged fixture data. It MUST support an
ordered sequence of responses across successive invocations, with the final
response repeating, and MUST record every invocation to a readable log.

#### Scenario: Suite runs with no nix present

- **WHEN** the script-contract and real-shell layers run on a host with no `nix`
  binary anywhere on `PATH` and with no network access
- **THEN** every test in those layers completes, and no invocation of a real nix
  is attempted

#### Scenario: Eval arguments are pinned

- **WHEN** the daemon performs an eval for target `default`
- **THEN** the recorded invocation contains `print-dev-env`, the installable
  `.#`, `--json`, `--option connect-timeout 5`, `--option download-attempts 2`,
  and a `--profile` path, and its working directory is the flake root

#### Scenario: Staged failure drives the degraded path

- **WHEN** the first staged response succeeds and the second exits non-zero with
  an `error:` payload
- **THEN** the second eval moves the daemon to its error state, a notification
  surfaces the error summary, and the env and hook files produced by the first
  eval remain present and sourceable

#### Scenario: Staged change drives exactly one re-source

- **WHEN** the second staged response differs from the first in both `shellHook`
  and `PATH`
- **THEN** the env generation counter increments exactly once and a live
  subshell re-sources exactly once

#### Scenario: Staged delay drives the timeout path

- **WHEN** a staged response sleeps beyond the configured eval timeout
- **THEN** the timeout path is taken without the test having waited on real nix

### Requirement: Fake nix output realism

Fake nix responses MUST be faithful to the subset `DevEnv::from_nix_json`
consumes, and MUST exercise the same downstream parsing that real output does.

#### Scenario: Only the consumed subset is relied upon

- **WHEN** a response declares a `variables` object containing non-exported
  entries, non-string values, and `SKIP_VARS` entries alongside exported strings
- **THEN** the parsed environment contains exactly the exported string entries
  that are not in `SKIP_VARS`, `PATH` is colon-split into nix paths, and a
  non-blank `shellHook` is retained

#### Scenario: Profile symlink is created

- **WHEN** the fake is invoked with `--profile <path>`
- **THEN** a profile symlink exists at that path afterwards, so GC-root
  registration and `dix`-backed profile diffing run against real input rather
  than a missing path

#### Scenario: Store paths parse into packages

- **WHEN** a response's `PATH` contains entries of the form
  `/nix/store/<hash>-<name>-<version>/bin`
- **THEN** `store_path.rs` parsing yields a non-empty package list for them

### Requirement: Script-contract layer

The harness MUST provide tests that invoke `xi develop prompt` as a child
process inside a world and assert on its emitted shell text, without executing
that text. This layer MUST cover: untrusted-flake gating, the
`flake_likely_has_devshell` skip, no-flake-found silence, `__XI_SUPPRESS` honour
and clear, and the parent-versus-subshell dispatch keyed on `__XI_IN_DEVSHELL`.

#### Scenario: Untrusted flake emits no spawn command

- **WHEN** `prompt` runs in parent mode with cwd inside an untrusted flake that
  has a devshell
- **THEN** stdout contains no subshell spawn command, and stderr carries the
  trust hint

#### Scenario: Flake without a devshell is skipped silently

- **WHEN** `prompt` runs with cwd inside a trusted flake whose `flake.nix` and
  imported modules contain no devshell keyword
- **THEN** stdout is empty and no trust prompt or warning is emitted

#### Scenario: Suppress is honoured then cleared

- **WHEN** `__XI_SUPPRESS` names a flake root and cwd is beneath it
- **THEN** stdout is empty
- **WHEN** cwd then moves outside that root
- **THEN** stdout contains the unset of `__XI_SUPPRESS` for the requested shell

#### Scenario: Subshell dispatch is keyed on the in-devshell marker

- **WHEN** `__XI_IN_DEVSHELL` is set and `prompt` is invoked in what the
  activation hook treats as parent mode
- **THEN** subshell-mode behaviour is produced and no nested spawn command is
  emitted

### Requirement: Real-shell evaluation layer

The harness MUST provide tests that evaluate emitted scripts in a real `bash`,
`zsh`, and `fish` process and assert on the resulting environment, read with NUL
delimiters so values containing newlines survive. Where a shell binary is absent
from the host, its tests MUST skip with a reported reason rather than fail.

#### Scenario: zsh cd-out write survives NO_CLOBBER

- **GIVEN** a zsh process with `NO_CLOBBER` set and an existing cd-out file, as
  the parent creates via `mktemp` before spawning
- **WHEN** the cd-out exit snippet is evaluated
- **THEN** the file contains the current directory and the exit status is the
  cd-out sentinel 200

#### Scenario: Depth arithmetic increments across levels

- **WHEN** a spawn command is evaluated with `__XI_DEPTH` unset, then again with
  `__XI_DEPTH=1`
- **THEN** the child environment carries `__XI_DEPTH=1` and `__XI_DEPTH=2`
  respectively

#### Scenario: Injected environment survives round trip

- **WHEN** a generated env file whose `shellHook` contains newlines and quoting
  metacharacters is sourced
- **THEN** the resulting environment matches the staged nix response exactly for
  every exported variable, and nix paths are prepended to the pre-existing
  `PATH` rather than replacing it

#### Scenario: Missing shell skips rather than fails

- **WHEN** `fish` is not installed on the host
- **THEN** fish evaluation tests report as skipped with the reason, and the
  suite's exit status is unaffected

### Requirement: Interactive end-to-end layer

The harness MUST provide tests that drive a real interactive shell under a
pseudo-terminal through the devshell lifecycle, synchronising on a unique prompt
sentinel rather than on elapsed time. This layer MUST cover activation, entry on
cd, re-eval on flake change, nested flake entry and exit, cd-out, explicit exit
with suppression, and re-entry.

#### Scenario: Activation to entry

- **WHEN** the activation script for the shell is installed in the world's rc
  file and the shell cds into a trusted flake with a devshell
- **THEN** a subshell is entered with `__XI_IN_DEVSHELL=1`, `__XI_FLAKE_ROOT`
  set to the flake root, `__XI_DEPTH=1`, and the staged nix environment applied

#### Scenario: Nesting isolates and restores

- **WHEN** a nested flake is entered from inside a depth-1 devshell
- **THEN** `__XI_DEPTH` is 2 and the nested environment is applied
- **WHEN** the nested level is left via the cd-out path (cwd moves out of the
  nested flake but stays inside the outer one), which is the only exit that pops
  exactly one level
- **THEN** variables injected at depth 2 are removed, every depth-1 variable is
  restored to its depth-1 value, and the depth-1 shell is still alive

#### Scenario: cd-out follows the user

- **WHEN** the user cds from the flake root to a directory outside it from
  within the devshell
- **THEN** the subshell exits with status 200 and the parent shell's working
  directory is the directory the user cd'd to, not the directory the parent
  occupied before spawning

#### Scenario: Explicit exit cascades to the parent

- **GIVEN** the exit-code convention documented in `subshell.rs`: status 200
  means "left the flake directory" and the parent stays alive, while any other
  status means the user explicitly exited and the parent re-exits with the same
  status, cascading up through every wrapper level so the top-level terminal
  closes
- **WHEN** the user exits the devshell with `exit` or Ctrl-D
- **THEN** the parent shell re-exits with that same status rather than
  surviving, because async activation is transparent on the way in and an
  explicit exit means "close this terminal", not "pop one invisible layer"

#### Scenario: Re-entry after leaving and returning

- **WHEN** cwd leaves the flake root, taking the cd-out path so the parent
  survives, and later returns to the flake root
- **THEN** a devshell is entered again

#### Scenario: Flake change triggers re-eval in a live shell

- **WHEN** `flake.nix` is modified while a devshell is live and the next staged
  nix response differs
- **THEN** the shell re-sources on a subsequent prompt and the new environment
  is in effect, observed by polling the env generation counter

### Requirement: No elapsed-time synchronisation

Tests MUST NOT use a fixed sleep as a synchronisation primitive. Waiting on
daemon or shell progress MUST be expressed as polling an observable with a
deadline. Permitted observables are the daemon status query reaching
`DaemonState::Ready`, the env and hook generation counters, the A/B slot files
and `current` symlink, `meta.json`, the fake-nix call log, and the interactive
prompt sentinel.

#### Scenario: Suite contains no sleep-based waits

- **WHEN** the harness and every test file in it are inspected
- **THEN** no sleep call is used to wait for a state transition; each wait names
  the observable it polls and the deadline it allows

#### Scenario: Deliberate delay is staged, not slept

- **WHEN** a test needs an eval to be slow
- **THEN** the delay is produced by the fake nix under test instruction, and the
  test still waits by polling an observable

### Requirement: Layer placement rule

A behaviour MUST be tested at the cheapest layer able to observe it. A test MAY
live in the interactive layer only if it depends on a prompt hook firing, a
process exit status crossing a parent and child shell boundary, or environment
isolation between two live shells.

#### Scenario: Script-content assertion is not promoted

- **WHEN** a behaviour is fully observable from the text `prompt` emits
- **THEN** it is tested in the script-contract layer and not in the interactive
  layer

### Requirement: Fast-layer budget and interactive gating

The combined script-contract and real-shell layers MUST complete in under ten
seconds on a developer machine and MUST run on every commit. The interactive
layer MUST be gated so a host without a pseudo-terminal can run the rest.

#### Scenario: Fast layers stay fast

- **WHEN** the script-contract and real-shell layers run on a developer machine
- **THEN** combined wall-clock is under ten seconds

#### Scenario: Interactive layer is gated

- **WHEN** the suite runs on a host or sandbox with no pseudo-terminal available
- **THEN** the interactive layer does not run, the remaining layers pass, and
  the skip is reported

### Requirement: Regression anchor for the cd-out clobber defect

The suite MUST contain a named test that fails if the force-write redirection in
`subshell::generate_cd_out_exit` is reverted to a plain redirection. This is the
acceptance condition for the harness as a whole.

#### Scenario: Reverting the fix fails a named test

- **GIVEN** the cd-out snippet's `>|` replaced by `>`
- **WHEN** the suite runs
- **THEN** a named test fails, and its failure message identifies the cd-out
  write as the cause
