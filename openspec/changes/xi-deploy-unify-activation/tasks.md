# Tasks

Live state in beads under `change:xi-deploy-unify-activation`.

## 1. `ActorName::Push`

- [ ] 1.1 Add `ActorName::Push` to `crates/xi-core/src/sources/lock.rs`, lowest
      priority in the existing wait-order comparison, and verify with a unit
      test that a waiting `Switch` actor is still granted ahead of a waiting
      `Push` actor.
- [ ] 1.2 Unit test: two concurrent lock acquisitions, one `ActorName::Push`
      and one `ActorName::Daemon`, serialize rather than both succeeding.

## 2. Push execs the local entrypoint remotely

- [ ] 2.1 Replace `orchestrate.rs::activation_script`'s raw
      `nix-env --set` / `switch-to-configuration` script with a remote
      `xi <scope> switch <store-path>` invocation carrying
      `XI_APPLIED_ORIGIN=push:$(hostname)`; verify via the existing
      `bdd_push_builtin.rs` fixture, updated to assert the new command shape.
- [ ] 2.2 Replace `mode_sources.rs::switch_script`'s raw
      `nixos-rebuild switch --flake` invocation the same way, keeping the
      existing `systemd-run --wait --pipe --collect` wrapping; verify via
      `bdd_push_builtin.rs`'s remote-build scenario.
- [ ] 2.3 Verify end-to-end: a fixture `Transport` records the exact remote
      command for both modes and asserts it is an `xi <scope> switch`
      invocation, not the old raw script, in both
      `bdd_push_builtin.rs` and `bdd_push_orchestrator.rs`.

## 3. State-dir markers

- [ ] 3.1 `record_activation_at` reads `XI_APPLIED_ORIGIN` and writes
      `applied-origin` (`local` | `daemon` | `push:<host>`), falling back to
      today's SSH-env-var inference when the variable is unset; unit test
      covers all three values.
- [ ] 3.2 `record_activation_at` writes `inventory-role` when the triggering
      push supplied a `[[push.hosts]]` name; unit test covers presence and
      absence.
- [ ] 3.3 `DeploymentStatusArgs::run` (`deployment.rs`) reads and reports both
      new markers in both the human and `--json` output; verify with a test
      asserting the JSON envelope's `applied_origin` and `inventory_role`
      fields.
- [ ] 3.4 BDD: push a target end-to-end against a fixture transport and
      assert its state dir carries `applied-origin: push:<host>` after.

## 4. Working-dir mismatch guard

- [ ] 4.1 In `switch.rs::prepare`'s case 3, compare the canonicalized
      installable path against the scope's configured `working_dir` when both
      resolve to local paths; unit test the comparison in isolation (no
      prompt, no filesystem side effects).
- [ ] 4.2 Add `--force-working-dir` to `SwitchSourcesArgs`; wire the
      interactive `inquire::Confirm` prompt (mirroring `--ask`'s existing
      shape) gated on a TTY check, refusing outright with no TTY and no flag.
- [ ] 4.3 BDD: a mismatched path with `--force-working-dir` proceeds without
      prompting; the same path with no TTY and no flag exits non-zero naming
      the mismatch.

## 5. Backend-detection collapse

- [ ] 5.1 Move `push::discover::detect_backend`'s precedence into
      `xi-deploy` as the single implementation; `xi-deploy::detect_backend`
      becomes a thin wrapper that discovers targets from the given flake ref
      and calls it.
- [ ] 5.2 Update `push::orchestrate::delegate` and the `xi deploy` entrypoint
      to both call the collapsed function; verify no existing backend-
      detection test in `bdd_push_builtin.rs` or `xi-deploy`'s own test suite
      regresses.
- [ ] 5.3 Unit test: the flake-probing and target-list entry points resolve
      the same backend for the same flake, proving they share one precedence
      table rather than two.

## Workflow follow-up

- Archive this change once `xi-deploy-mechanism-detection` and
  `xi-deploy-build-config` have confirmed they can dispatch through the new
  `xi <scope> switch`-based activation step without further changes to it.
