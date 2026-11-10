# Tasks

Live state in beads under `change:xi-push-resilient-remote-build`.

## 1. Named, receipted unit

- [ ] 1.1 Generate a per-push `push-id` (short random suffix on `target.name` +
      unix time, collision-checked against existing receipts) and pass
      `--unit=xi-push-<push-id>` to `systemd-run` in `wrap_in_systemd_run`.
- [ ] 1.2 Write the local receipt
      (`<state dir>/push-receipts/
      <push-id>.json`) before the SSH call
      that starts the unit; remove it only on an observed terminal outcome.
- [ ] 1.3 Unit test: receipt is written before the (mocked) transport call and
      removed after a success outcome; a simulated disconnect (transport error)
      leaves the receipt in place.

## 2. Reattach

- [ ] 2.1 `xi push --reattach` with no id: list every local receipt, probe each
      target's reachability, print state (unknown if unreachable).
- [ ] 2.2 `xi push --reattach <id>`: SSH in, `systemctl show` the named unit;
      map `ActiveState`/`SubState`/`ExecMainStatus` to Running/Succeeded/Failed.
- [ ] 2.3 Fallback when the unit has been collected: query
      `journalctl -u
      <unit_name> --since <started_at>` for the final
      status line, since `--collect` unloads the live unit but not its journal
      history.
- [ ] 2.4 Still-running case: stream
      `journalctl -u <unit_name> -f --since
      <started_at>` until a terminal
      line or operator interrupt.
- [ ] 2.5 BDD: a scripted transport simulating disconnect-then-reattach-to-
      success round-trips through receipt write, reattach, receipt removal.
- [ ] 2.6 BDD: reattach to a failed unit reports the failure and does not remove
      the receipt.

## 3. Notify

- [ ] 3.1 `--notify <command>` stored on the receipt; run exactly once, on the
      first terminal observation, whichever process (original or reattached)
      observes it first.
- [ ] 3.2 Unit test: notify fires once even if both the original
      (now-reconnected) process and a concurrent `--reattach` observe the same
      terminal state - an atomic receipt removal decides the winner, not a race.

## Workflow follow-up

- Archive once `xi-sources-retry` lands and `--reattach`'s own reachability
  probe adopts its classifier, so a flaky reconnect during reattach itself does
  not misreport "unit gone."
