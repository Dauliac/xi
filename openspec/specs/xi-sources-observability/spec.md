# xi-sources-observability Specification

## Purpose

`xi-sources-observability` is the capability an operator reaches for when they
have to answer "what did the daemon just do, and why did it stop working". It
owns three operator-facing surfaces (structured logs, a queryable event store, a
Prometheus exporter) and one machine-facing surface (a health probe), and it
fixes the field vocabulary every other `xi-sources-*` capability emits so a log
line from the fetch loop and a log line from the activation step can be joined
on the same keys.

It deliberately owns no policy. It does not decide when a tick runs
([[xi-sources-gitops]]), what counts as a conflict ([[xi-sources-conflict]]), or
which credentials exist ([[xi-sources-credentials]]). It decides only how those
things become visible, and it guarantees that becoming visible never leaks a
secret.

### Entities

- **Tick span** - one root tracing span per daemon iteration, carrying the
  source's identity and the iteration's outcome.
- **Phase span** - one child span per phase of a tick (fetch, verify, resolve,
  build, activate, hook), carrying the phase's outcome and duration.
- **Log sink** - where structured ndjson lands for a scope: the journal at
  system scope, a daily-rotating file under the state dir at user scope.
- **Event** - one row in the daemon's sqlite event store: a timestamped, typed
  record of something that happened, queryable long after the log that described
  it has rotated away.
- **Metric registry** - the fixed set of Prometheus counters, histograms, and
  gauges the exporter publishes. The set is closed, and every label is a fixed
  enumeration.
- **Health verdict** - the single boolean a container probe or systemd watchdog
  needs, plus the reason behind it.
- **Redaction boundary** - the compile-time and run-time rules that keep a
  credential out of every surface above.

## Requirements

### Requirement: Structured tracing per tick

Every daemon tick MUST emit exactly one root span (`xi.sources.tick`) with these
fixed fields:

- `sources_id` (string)
- `remote` (string)
- `actor_name` ("daemon")
- `outcome` ("ok" | "degraded" | "failed")
- `duration_ms` (integer)

A tick that reaches each phase MUST emit a child span per phase, each with fixed
fields:

- `phase` ("fetch" | "verify" | "resolve" | "build" | "activate" | "hook")
- `outcome` ("ok" | "degraded" | "failed")
- `duration_ms` (integer)
- `old_sha` (optional string)
- `new_sha` (optional string)

A tick that fetches, verifies, resolves, builds, and activates MUST produce
exactly 5 phase child spans in that order.

The field vocabulary is closed. A capability that wants to record something
outside it records an event instead, so a log consumer can rely on the key set.

#### Scenario: Full tick emits 5 child spans

- **GIVEN** a tick that advances the sources dir and successfully activates
- **WHEN** the tick runs
- **THEN** the tracing sink receives one root `xi.sources.tick` span with five
  child spans in order (fetch, verify, resolve, build, activate)

#### Scenario: Panic is reported through tracing, not just the process exit

- **GIVEN** a daemon with the observability panic hook installed
- **WHEN** a tick panics
- **THEN** an error-level event is emitted carrying the panic payload, the panic
  location, and a backtrace
- **AND** the tick's outcome is recorded as `failed`

### Requirement: Log destinations by scope

System-scope daemons MUST emit ndjson structured logs to journald. The service
unit MUST set `StandardOutput=journal`. Operators MUST be able to read logs via
`journalctl -u xi-sources-daemon`.

User-scope daemons MUST emit ndjson structured logs to a daily rotating file at
`$XDG_STATE_HOME/xi/logs/sources-daemon.log` with a default 30-day retention.

Default log level is `info`, configurable via
`xi.sources.observability.log-level` (`trace` | `debug` | `info` | `warn` |
`error`).

#### Scenario: System logs reach journalctl

- **GIVEN** a NixOS build with the daemon enabled
- **WHEN** a tick completes
- **THEN** `journalctl -u xi-sources-daemon -o json --since '1 minute ago'`
  returns one or more lines matching the tick's structured log

#### Scenario: User log rotates daily

- **GIVEN** a daemon running at user scope with default retention
- **WHEN** 31 days elapse
- **THEN** the oldest log file is deleted
- **AND** the current log is `sources-daemon.log`

### Requirement: Operator readout verbs

The capability MUST provide:

- `xi <scope> sources log tail [--follow] [--since <ts>] [--level <l>]`: streams
  the configured sink's log, filtered as specified. Resolves the sink per scope.
- `xi <scope> sources events [--since <ts>] [--kind <k>] [--limit <n>]`: returns
  structured events from the daemon's lightweight sqlite event store at
  `<state_dir>/events.db`.

Event kinds include: `fetch`, `advance`, `verify`, `build`, `activate`, `hook`,
`lock-wait`.

Scope is not a flag. It comes from the verb that owns the group, matching every
other `xi <scope> sources` subcommand.

#### Scenario: Events filtered by kind

- **GIVEN** the event store contains fetches, builds, and activations
- **WHEN** `xi home sources events --kind activate` runs
- **THEN** only activation events appear in the output

#### Scenario: Events carry a schema version

- **GIVEN** any event query
- **WHEN** the result is printed
- **THEN** the output is a two-row envelope: a schema version header followed by
  body rows, sorted descending by time

### Requirement: Prometheus exporter

When `xi.sources.observability.prometheus.port` is a non-null integer in the
range 1024-65535, the daemon MUST start a Prometheus text-exposition HTTP server
bound to `127.0.0.1:<port>`. Non-loopback binds MUST be refused with a clear
error.

The exporter MUST expose these metrics with these names:

- `xi_sources_fetch_total{outcome}` (counter)
- `xi_sources_fetch_duration_seconds` (histogram)
- `xi_sources_sources_head_age_seconds` (gauge, equals now minus last successful
  fetch time)
- `xi_sources_build_duration_seconds` (histogram)
- `xi_sources_build_total{outcome}` (counter)
- `xi_sources_activation_total{outcome}` (counter)
- `xi_sources_last_failure_timestamp_seconds` (gauge)
- `xi_sources_lock_wait_duration_seconds` (histogram)
- `xi_sources_hook_total{hook,outcome}` (counter)

The server MUST serve `/metrics` and `/healthz` and nothing else; any other path
is a 404.

#### Scenario: Non-loopback port refused

- **GIVEN** `xi.sources.observability.prometheus.port = 9321` AND
  `xi.sources.observability.prometheus.bind = "0.0.0.0"` (via a future-expansion
  key or by env override)
- **WHEN** the daemon starts
- **THEN** startup fails with an error naming the non-loopback bind

#### Scenario: Metrics endpoint reachable

- **GIVEN** `xi.sources.observability.prometheus.port = 9321`
- **WHEN** the operator runs `curl http://127.0.0.1:9321/metrics`
- **THEN** the response includes every metric name listed above

#### Scenario: Unknown path is not served

- **GIVEN** a running exporter
- **WHEN** any path other than `/metrics` or `/healthz` is requested
- **THEN** the response is 404

### Requirement: `/healthz` endpoint

The same HTTP server MUST serve `GET /healthz` with:

- 200 OK when the daemon is running AND the most recent tick's outcome is `ok`
  or `degraded` AND the lock is not held by a dead pid.
- 503 Service Unavailable otherwise. The response body names the reason
  (`"last-tick-failed"`, `"stale-lock"`, etc.) in a single JSON object.

#### Scenario: Healthy daemon returns 200

- **GIVEN** a daemon whose last tick was `ok`
- **WHEN** `/healthz` is probed
- **THEN** the response is 200 with body `{"status":"ok"}`

#### Scenario: Failed tick returns 503

- **GIVEN** the last tick failed
- **WHEN** `/healthz` is probed
- **THEN** the response is 503 with a body naming `last-tick-failed` and the
  failure's timestamp

### Requirement: Opt-in OTLP export

When `xi.sources.observability.otlp.endpoint` is set, the daemon MUST export
every root span to the configured endpoint via OTLP, with batching that caps
latency at 5 seconds.

Export failures MUST NOT block the tick loop; they MUST be logged and counted
via `xi_sources_otlp_export_failures_total` (added to the Prometheus set when
OTLP is enabled).

#### Scenario: OTLP endpoint receives root span

- **GIVEN** OTLP enabled with endpoint `http://collector:4318`
- **WHEN** a tick completes
- **THEN** the collector receives the `xi.sources.tick` root span within 5
  seconds

#### Scenario: Unreachable collector does not stall the loop

- **GIVEN** OTLP enabled with an endpoint that refuses connections
- **WHEN** a tick completes
- **THEN** the tick returns without waiting on the export
- **AND** `xi_sources_otlp_export_failures_total` increases

### Requirement: Redaction

Every observable surface (tracing field, metric label value, event-store column,
OTLP span attribute) MUST reject any credential value, token file content, or
GPG signature body. The opaque credential newtype from
[[xi-sources-credentials]] refuses `Serialize`, so a field attempting to log a
credential fails at compile time.

Metric labels MUST use a fixed enumeration per metric. Dynamic labels derived
from user input are forbidden to prevent credential leakage through label
cardinality.

Free-text fields that unavoidably carry third-party output (a git error, a hook
stderr tail) MUST pass through a run-time scrubber that replaces any registered
secret value with `<redacted>` before the text reaches a sink.

#### Scenario: Token never appears in any observable surface

- **GIVEN** a configured HTTPS token
- **WHEN** the operator greps the log file, the metrics endpoint, the event
  store, and (if enabled) the OTLP export for the token's string
- **THEN** all four searches return zero matches

#### Scenario: Credential cannot be serialised at all

- **GIVEN** the opaque credential newtype
- **WHEN** code attempts to serialise it or print it with `Debug`
- **THEN** serialisation is impossible (no `Serialize` impl) and `Debug` prints
  `<redacted>`
