# Tasks: devshell-e2e-harness

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads.

Ordering note: sections 2 and 3 are the only hard prerequisites. Everything from
section 4 onward depends on both, and sections 5, 6, and 7 are independent of
each other once the world exists.

## 1. Specification (P1)

- [ ] 1.1 Land the spec delta under `specs/xi-devshell-test-harness/spec.md`.
- [ ] 1.2 Cross-link with `xi-devshell-activation` (owns the activation and
      prompt-output contract that the script-contract layer asserts against),
      `xi-devshell-trust` (trust gating), and `xi-devshell-watcher` (the re-eval
      trigger the interactive layer exercises). None of their requirements
      change; they gain an executable verification route.

## 2. Runtime-dir seam (P1, blocks everything)

- [ ] 2.1 Add the `XI_RUNTIME_DIR` override to `dirs::runtime_dir()`
      (`crates/xi-develop/src/dirs.rs:83`). Explicit override first on all
      platforms; unchanged `/tmp/xi-{uid}` fallback on unix; unchanged
      `XDG_RUNTIME_DIR` fallback under `#[cfg(not(unix))]`. Treat an empty value
      as unset.
- [ ] 2.2 Validate the resulting socket path against the platform `sun_path`
      limit and emit a diagnostic naming path length and limit. Verify the
      failure leaves no `daemon.pid` behind.
- [ ] 2.3 Unit test in `bdd_dirs.rs`: override honoured, empty treated as unset,
      unset byte-identical to the pre-change `/tmp/xi-{uid}` value.
- [ ] 2.4 Audit every `dirs::runtime_dir` / `daemon_runtime_dir` /
      `daemon_socket_path` consumer (`lifecycle.rs`, `client.rs`,
      `client_v3.rs`, `server.rs`, `prompt.rs`) for a cached or precomputed path
      that would bypass the override.

## 3. Fake nix fixture (P1, blocks everything)

- [ ] 3.1 Write `crates/xi-develop/tests/fixtures/fake-nix` as a committed
      executable. Confirm it is mode 0755 in git and that `XI_NIX_BIN`'s
      `p.is_file()` check in `nix-command/src/lib.rs:266` accepts it.
- [ ] 3.2 Implement ordered multi-response staging driven by files in the world
      directory, last response repeating. No state in the script itself.
- [ ] 3.3 Implement the invocation log: argv plus cwd per call, append-only,
      readable mid-test.
- [ ] 3.4 Honour `--profile <path>`: create a plausible profile symlink so
      GC-root logic and `capture_dix_diff` (`server.rs` circa :1390) run against
      real input.
- [ ] 3.5 Emit `/nix/store/<hash>-<name>-<version>` paths that `store_path.rs`
      parses into non-empty `PackageInfo`. Assert this, do not assume it.
- [ ] 3.6 Support staged non-zero exit with an `error:` payload, and staged
      delay.
- [ ] 3.7 Build the response fixtures against the subset `DevEnv::from_nix_json`
      consumes (`env_file.rs:92`): only `type == "exported"` string entries,
      `PATH` colon-split, non-blank `shellHook` retained, `SKIP_VARS`
      (`env_file.rs:9`) discarded.
- [ ] 3.8 Add a nix-backed shape check OUTSIDE the fast suite that compares the
      fixture's shape against one real `print-dev-env --json`. This is the only
      defence against fixture drift; without it the fast suite can pass against
      a stale contract.

## 4. Test world (P1)

- [x] 4.1 Decide harness location. RESOLVED, and it forced a move the original
      plan did not anticipate: process-level tests CANNOT live in
      `crates/xi-develop/tests/`, because `CARGO_BIN_EXE_xi` is only defined for
      the package that declares the binary, and cargo only guarantees the binary
      is built before that same package's integration tests. The harness and
      every test that spawns `xi` therefore live in `crates/xi/tests/`
      (`harness/mod.rs` + `e2e_devshell_*.rs`). Fixture-level tests that need no
      binary stay in `crates/xi-develop/tests/`. Chose a shared `tests/harness/`
      module over a `xi-develop-testkit` crate: the >3-binary heuristic in
      design.md was about compile cost, which is negligible for a small module,
      and a testkit crate would introduce a dev-dependency cycle for no gain.
- [ ] 4.2 Implement the world: one `TempDir`; `HOME`, `XDG_STATE_HOME`,
      `XDG_CONFIG_HOME`, `XI_RUNTIME_DIR` inside it. Environment returned as
      data applied per `Command`. No `set_var` anywhere in the harness.
- [ ] 4.3 Pin the binary: write `env!("CARGO_BIN_EXE_xi")` to
      `$XDG_STATE_HOME/xi/develop/xi-bin` so both `resolve_xi_bin` call sites
      (`lifecycle.rs:242`, `prompt.rs:503`) resolve to the build under test.
- [ ] 4.4 Implement flake fixture materialisation, including a nested-flake
      fixture. Reuse `crates/xi-develop/tests/flake-module/` where it fits.
- [ ] 4.5 Implement trust staging: pre-write the marker under
      `dirs::trust_dir()` rather than shelling out to `xi develop trust`, so
      trust state is a precondition and not a step under test.
- [ ] 4.6 Implement daemon reaping on drop via `lifecycle::stop`
      (`lifecycle.rs:117`) with a SIGKILL fallback for a wedged process.
- [ ] 4.7 Implement the observable pollers: `DaemonState::Ready` via status, env
      and hook generation counters (`dirs.rs:226-233`), A/B slot files and
      `current` symlink (`dirs.rs:142-188`), `meta.json`, fake-nix call log.
      Each takes a deadline and reports what it was waiting for on timeout.
- [ ] 4.8 Leak assertion helper backing SC-H-001: walk the real `$HOME`,
      `$XDG_STATE_HOME`, `$XDG_CONFIG_HOME`, and `/tmp/xi-{uid}/` and assert the
      scenario touched none of them.

## 5. Script-contract layer (P1)

- [ ] 5.1 `crates/xi/tests/e2e_devshell_prompt.rs`: invoke `xi develop prompt`
      as a child in a world; assert on emitted text. Follow the
      `crates/xi-sources-daemon/tests/ctl_subcommand.rs:80-83` pattern.
- [ ] 5.2 Untrusted flake: no spawn command on stdout, trust hint on stderr.
- [ ] 5.3 No-devshell flake: stdout empty, no warning. Exercise the
      imported-module case that `flake_likely_has_devshell` (`prompt.rs:442`)
      regressed on before.
- [ ] 5.4 No flake found: stdout empty.
- [ ] 5.5 Suppress honoured beneath the root, and the unset emitted once cwd
      moves outside it, per shell.
- [ ] 5.6 Dispatch: `__XI_IN_DEVSHELL` set produces subshell-mode behaviour and
      no nested spawn command.
- [ ] 5.7 Assert the recorded fake-nix argv pins the eval contract
      (`print-dev-env`, `.#`, `--json`, `connect-timeout 5`,
      `download-attempts 2`, `--profile`, cwd equal to flake root).

## 6. Real-shell evaluation layer (P1)

- [ ] 6.1 Environment capture via NUL-delimited read so multi-line `shellHook`
      values survive the round trip.
- [ ] 6.2 bash, zsh, fish evaluation of captured scripts. Absent shell binary
      skips with a reported reason, never fails.
- [ ] 6.3 `e2e_cdout_clobber`: zsh with `NO_CLOBBER` and a pre-existing cd-out
      file. This is the SC-H-004 anchor. Name it so the failure message points
      at the cd-out write.
- [ ] 6.4 Verify SC-H-004 by actually reverting `>|` to `>` in
      `subshell.rs:46-52`, watching 6.3 fail, then restoring. A harness that has
      not been shown to catch this defect is not done.
- [ ] 6.5 Depth arithmetic: `__XI_DEPTH` unset gives 1, `__XI_DEPTH=1` gives 2.
- [ ] 6.6 Injected-env round trip: exported variables match the staged response
      exactly; nix paths prepend to the existing `PATH` rather than replacing
      it.
- [ ] 6.7 Cleanup preamble and depth-scoped `__XI_INJECTED_VARS_<depth>`
      behaviour, against the existing `env_file.rs` generators.

## 7. Interactive end-to-end layer (P2)

- [ ] 7.1 Choose the PTY crate against maintenance, read-until-sentinel support
      without built-in timing assumptions, and nix-sandbox compatibility.
- [ ] 7.2 Prompt-sentinel synchronisation: unique `PS1`, write command, read
      until sentinel returns. This is what makes the no-sleep rule achievable.
- [ ] 7.3 Install the activation script into the world's rc file and drive a
      real interactive shell.
- [ ] 7.4 Activation to entry: `__XI_IN_DEVSHELL=1`, `__XI_FLAKE_ROOT`,
      `__XI_DEPTH=1`, staged environment applied.
- [ ] 7.5 Nesting: enter nested flake at depth 2; on exit, depth-2 injected vars
      removed, every depth-1 value restored, depth-1 shell alive.
- [ ] 7.6 cd-out: exit 200, parent lands in the directory the user cd'd to.
- [ ] 7.7 Explicit exit: `__XI_SUPPRESS` set to the flake root, no re-entry
      beneath it, parent not killed.
- [ ] 7.8 Re-entry after leaving and returning.
- [ ] 7.9 Flake change re-eval in a live shell, with the second staged response
      differing; assert exactly one re-source via the generation counter
      (SC-H-006).
- [ ] 7.10 Staged nix failure: daemon error state, notification surfaced,
      previous env and hook files still present and sourceable (SC-H-005).
- [ ] 7.11 Concurrency: two worlds, two fixtures, run in parallel, assert no
      interaction (SC-H-003).

## 8. Discipline and gating (P2)

- [ ] 8.1 Enforce the no-sleep rule: a check that fails if a sleep is used as a
      synchronisation primitive in the harness or its tests (SC-H-010).
- [ ] 8.2 Gate the interactive layer behind a nextest profile or `#[ignore]` so
      a host or sandbox without a pseudo-terminal still runs the rest.
- [ ] 8.3 Measure and assert the fast-layer budget: script-contract plus
      real-shell under ten seconds (SC-H-011).
- [ ] 8.4 Wire the fast layers into the per-commit path and the interactive
      layer into a scheduled job. Decide whether the interactive layer can run
      inside `nix flake check` based on tty availability in the sandbox.
- [ ] 8.5 Document the layer placement rule where test authors will see it, so
      the suite does not drift toward putting everything in the slowest layer.

## 9. Follow-up work (P3)

Scope guards first. These are recorded so a later session does not helpfully do
them anyway. They are deliberately NOT checkboxes and MUST NOT be decomposed
into beads, because a tracker item phrased as a prohibition reads as work once
it reaches a ready queue:

- Do NOT add a clock abstraction to the daemon under this change. If a specific
  scenario proves unreachable without one, drop the scenario and file a separate
  change.
- Do NOT modify the interactive debugging script under the repo scripts
  directory. It stays as it is.

Actual follow-up work:

- [ ] 9.1 File a separate change for an `xi develop exec` end-to-end scenario.
      `exec::exec` replaces the process via `CommandExt::exec` and is covered
      only indirectly by this harness.
