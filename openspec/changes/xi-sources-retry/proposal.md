# Proposal

## Why

`push::engine::SshTransport` (`crates/xi-deploy/src/push/engine.rs:314`) and
every `rsync`/`nix copy` call downstream of it fail permanently on the first
transient error: a dropped connection mid-handshake, a `ConnectTimeout=10`
expiry during a flaky link, a momentary refusal from a binary cache.
`SshTransport::run` already distinguishes "ssh itself could not be spawned" (an
`Err`) from "the remote command exited non-zero" (an `Outcome{success:
false}`),
but neither path retries anything - a push that would have succeeded on a second
attempt fails the whole target on the first. `xi-develop` already solved exactly
this shape of problem for its own eval loop
(`crates/xi-develop/src/daemon/server.rs:75` `BASE_BACKOFF_SECS`/
`MAX_BACKOFF_SECS`, `crates/xi-develop/src/timeouts.rs:60`
`reconnect_backoff_cap`), but that logic is private to `xi-develop` and nothing
in `xi-deploy` or `xi-core::sources` can reuse it.

Retrying blindly is not safe either: a remote script that partially ran before
the connection dropped (e.g. mid-`switch-to-configuration`) must not be re-run
as if nothing happened. The retry boundary has to sit below "ssh connected and
the remote command ran" and above "ssh could not even reach the host," which
`SshTransport`'s own `run`/`copy_closure`/`sync_dir` methods are each already
positioned to classify, since they are the only code that sees the raw
`ssh`/`rsync`/`nix copy` exit status.

## What Changes

- A new `xi_core::retry` module: a `RetryPolicy` (max attempts, base delay, cap,
  jitter) and a
  `retry<T>(policy, classify, attempt: impl FnMut() ->
  Result<T>) -> Result<T>`
  helper, blocking (`thread::sleep`-based), matching `push::limits`'s own
  documented rationale for staying off `tokio` in a crate where every call is
  already a blocking `std::process` spawn.
- A `Transience` classifier: `Transient | Permanent`, computed from (a) whether
  the call returned `Err` at all (local spawn failure - always transient: the
  binary existing one second and not the next is itself the transient
  condition), and (b) for `ssh`'s own `Outcome`, its process exit code - `255`
  is `ssh`'s own "could not establish or maintain the connection" code and is
  classified transient; any other non-zero is the remote command's own failure
  and is never retried automatically, because the remote side may have already
  made a partial, non-idempotent change.
- `SshTransport::run`, `copy_closure`, and `sync_dir` each wrap their underlying
  spawn in `retry` with this classifier. `RemoteCommand::probe`- shaped calls
  (the magic-rollback sentinel, preflight checks) opt out: a probe's whole point
  is an honest, immediate answer, not a retried one that papers over exactly the
  condition it is testing for.
- A `--retry <n>` / `.xi.toml [push] retry-attempts` ladder entry (CLI >
  per-host > table > compiled default of 3), read the same way
  `resolve_remote_build` already reads its own ladder
  (`crates/xi-deploy/src/push/config.rs:208`).

## Capabilities

### New Capabilities

- `xi-sources-retry`: a shared, classify-then-backoff retry primitive for
  transient SSH/network failures, and its wiring into every `SshTransport`
  method that can see a raw exit code.

### Modified Capabilities

(none - greenfield; `xi-sources-push` is not yet archived into
`openspec/specs/`.)

## Impact

- **Affected code**: new `crates/xi-core/src/retry.rs`;
  `crates/xi-deploy/src/push/engine.rs` (`SshTransport`'s three methods);
  `crates/xi-deploy/src/push/args.rs` (`--retry`),
  `crates/xi-deploy/src/push/config.rs` (the new ladder entry).
- **Coordination**: `xi-deploy-build-config`'s planned `resolve_ladder<T>`
  generalization is the natural home for this change's retry-attempts ladder
  once it lands; this change does not wait for it and hand-rolls the same
  three-rung precedence `resolve_remote_build` already uses. Once
  `xi-sources-gitops`'s daemon tick loop exists, its remote-fetch step is the
  second intended consumer of `xi_core::retry` - not built here, named so the
  dependency direction is explicit.
- **Breaking?**: No. Purely additive; the default (3 attempts, exponential
  backoff) changes no observable behavior on a connection that does not fail.
- **Migration**: None.

## Success Criteria

- **SC-RT-001**: An `SshTransport::run` call whose `ssh` process exits 255
  (connection failure) is retried up to the configured attempt count with
  increasing backoff; a call whose remote command exits 1 (the command ran and
  failed) is never retried.
- **SC-RT-002**: A local spawn failure (`ssh` binary missing) is classified
  transient and retried the same number of times as a 255 exit, then surfaces
  the last underlying error together with the attempt count, not just the final
  attempt's bare error.
- **SC-RT-003**: A magic-rollback probe (`push::rollback::probe`) is unaffected
  by this change: its single-attempt, no-retry semantics (absence means absence,
  immediately) are unchanged.
- **SC-RT-004**: `--retry 0` disables retrying entirely (one attempt, matching
  today's behavior bit-for-bit); the compiled default without any configuration
  is 3 attempts.
