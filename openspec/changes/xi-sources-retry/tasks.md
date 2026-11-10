# Tasks

Live state in beads under `change:xi-sources-retry`.

## 1. Retry primitive

- [ ] 1.1 `xi_core::retry::RetryPolicy` and `retry<T>` with exponential
      backoff + jitter + cap, blocking (`thread::sleep`), no tokio dependency.
- [ ] 1.2 Unit test: N transient failures followed by success returns the
      success and the right attempt count; N+1 transient failures exhausts
      attempts and returns the last error.
- [ ] 1.3 Unit test: a single permanent (non-transient) failure returns
      immediately with no sleep and no retry.

## 2. Classifier

- [ ] 2.1 `Transience` classification for `SshTransport`'s raw process result:
      exit 255 or spawn `Err` => `Transient`; any other non-zero => `Permanent`.
- [ ] 2.2 Unit test against a fake exit-status fixture for each of: 255, 1, 0,
      spawn failure.

## 3. Wiring

- [ ] 3.1 Wrap `SshTransport::run`, `copy_closure`, `sync_dir` in `retry` using
      the classifier; leave `RemoteCommand::probe`-shaped calls (rollback probe,
      preflight) unwrapped.
- [ ] 3.2 `--retry <n>` CLI flag, `.xi.toml [push] retry-attempts` (table +
      per-host), ladder resolution mirroring `resolve_remote_build`.
- [ ] 3.3 BDD: a scripted transport that fails twice then succeeds completes a
      push with no operator-visible difference except a log line per retry.
- [ ] 3.4 BDD: `--retry 0` reproduces today's single-attempt failure behavior
      exactly.

## Workflow follow-up

- Archive this change once `xi-sources-push`'s own push engine has adopted it
  and `xi-deploy-build-config`'s `resolve_ladder<T>` exists to absorb the
  hand-rolled ladder in task 3.2, if that lands first.
