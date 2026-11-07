# Capability: xi-sources-retry

## ADDED Requirements

### Requirement: Transient transport failures are retried with backoff; permanent ones are not

`SshTransport::run`, `copy_closure`, and `sync_dir` MUST classify every
failure as `Transient` or `Permanent` before deciding whether to retry. A
local spawn failure, or a remote `ssh` process exiting 255, MUST be
classified `Transient` and retried up to the configured attempt count with
increasing backoff between attempts. Any other non-zero exit from the
remote command itself MUST be classified `Permanent` and MUST NOT be
retried automatically.

#### Scenario: A connection failure is retried

- **GIVEN** an `SshTransport::run` call whose `ssh` process exits 255
- **WHEN** the call is made under the default retry policy
- **THEN** it is retried, with increasing backoff, up to the configured
  attempt count before giving up

#### Scenario: A remote command's own failure is not retried

- **GIVEN** an `SshTransport::run` call whose remote command ran and
  exited 1
- **WHEN** the call completes
- **THEN** it is not retried, and the single `Outcome{success: false}` is
  returned immediately

#### Scenario: A local spawn failure is retried like a connection failure

- **GIVEN** the local `ssh` binary cannot be spawned at all
- **WHEN** the call is made under the default retry policy
- **THEN** it is retried the same number of times as a 255 exit, and on
  final exhaustion the returned error names both the last underlying error
  and the number of attempts made

#### Scenario: Probes bypass retry entirely

- **GIVEN** a `RemoteCommand::probe`-shaped call (e.g. the magic-rollback
  sentinel probe)
- **WHEN** the probe fails
- **THEN** it returns immediately with no retry, since absence must be
  reported as it is observed, not papered over by a later attempt

### Requirement: Retry attempts are configurable through a precedence ladder

The number of retry attempts MUST be resolved through a ladder: an explicit
CLI flag, then a per-host `.xi.toml` override, then a table-level
`.xi.toml` default, then a compiled floor of 3 attempts - the same
precedence shape `resolve_remote_build` already uses for the build-location
ladder.

#### Scenario: `--retry 0` disables retrying

- **GIVEN** `xi push --retry 0`
- **WHEN** a transient failure occurs
- **THEN** exactly one attempt is made and the failure surfaces
  immediately, matching behavior with no retry primitive at all

#### Scenario: The compiled default is 3 attempts

- **GIVEN** no `--retry` flag and no `.xi.toml [push] retry-attempts` entry
- **WHEN** a transient failure occurs
- **THEN** up to 3 attempts are made before giving up
