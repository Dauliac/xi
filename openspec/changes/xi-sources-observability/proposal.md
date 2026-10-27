# Change: xi-sources-observability

## Why

An operator cannot debug a daemon they cannot see. [[xi-sources-gitops]]
sketches hooks and a status surface but never names where logs go, how they are
structured, which metrics the daemon publishes, or how an operator discovers
"the last tick failed at 03:14 with a signature error".

Comin publishes a Prometheus exporter (see
`openspec/research/xi-sources/pull-gitops.md`, "Steal list"); xi owes operators
the same minimum. This change names the three operator-facing surfaces
(structured logs, metrics, events) and the single machine-facing surface (health
probe), lifts the Prometheus item out of gitops's P4 backlog, and locks in the
house tracing convention from the start.

## What Changes

- Introduce capability `xi-sources-observability` that owns:
  - **Structured logs via `tracing`** (house convention), one span per tick with
    child spans per phase (fetch, verify-signature, apply-conflict-resolver,
    build, activate, hooks). Fields are typed: `sources_id`, `remote`,
    `old_sha`, `new_sha`, `duration_ms`, `outcome`.
  - **Log destinations by scope**:
    - System: structured ndjson to `journald` via its native socket so operators
      can `journalctl -u xi-sources-daemon`.
    - User: structured ndjson to
      `$XDG_STATE_HOME/xi/logs/
      sources-daemon.log` with daily rotation
      and a 30-day retention default.
  - **Operator readouts**:
    - `xi sources log tail [--follow] [--since <ts>] [--level
      <l>]`
      streams the recent log for the resolved scope.
    - `xi sources events` prints the last N structured events (fetch outcomes,
      advance deltas, build durations, activation transitions, hook outcomes) as
      a filterable table.
  - **Prometheus exporter** on an optional loopback TCP port (opt-in via
    `xi.sources.observability.prometheus.port`, default null). Exposed metrics
    (full list in the spec):
    - `xi_sources_fetch_total{outcome}` counter
    - `xi_sources_fetch_duration_seconds` histogram
    - `xi_sources_sources_head_age_seconds` gauge (now minus last successful
      fetch time)
    - `xi_sources_build_duration_seconds` histogram
    - `xi_sources_build_total{outcome}` counter
    - `xi_sources_activation_total{outcome}` counter
    - `xi_sources_last_failure_timestamp_seconds` gauge
    - `xi_sources_lock_wait_duration_seconds` histogram
    - `xi_sources_hook_total{hook,outcome}` counter
  - **Health probe** on the same optional port (`/healthz` returns 200 when the
    daemon is running and the last tick outcome was not `panic`; 503 otherwise).
    Used by systemd `WatchdogSec` and by container probes.
  - **Opt-in OpenTelemetry trace export** via OTLP endpoint
    (`xi.sources.observability.otlp.endpoint`, default null). When set, every
    tick's root span is exported.
  - **Secret safety**: no log line, metric label, event field, or trace span
    attribute MAY carry a credential value, a token file path contents, or a GPG
    signature body. The lint rule from [[xi-sources-credentials]] enforces at
    build time.

## Impact

- **Affected specs**: `xi-sources-observability` (new capability).
- **Affected code (planned)**: `crates/xi-sources/src/observability.rs` wrapping
  `tracing`, `tracing-subscriber`, `prometheus`, and `opentelemetry-otlp` behind
  feature flags; integration in daemon main. The three CLI verbs (`log tail`,
  `events`, implicit `status`) land in `crates/xi/src/sources/`.
- **Coordination**: [[xi-sources-gitops]]'s P4 Prometheus item is closed and
  relocated here. [[xi-sources-credentials]]'s redaction rule is referenced for
  enforcement. [[xi-sources-nix-modules]] gains the three observability options.
- **Breaking?**: No. Default config disables Prometheus and OTLP; stdout/stderr
  log emission unchanged until operator opts into the structured sinks.
- **Migration**: None.

## Success Criteria

- **SC-O-001**: Every tick emits one root span with the fixed fields listed
  above; a tick that fetches, builds, and activates emits exactly 5 child spans.
- **SC-O-002**: `xi sources log tail --follow` on a system-scope daemon reads
  from journald with `-u xi-sources-daemon` filtering; on a user scope daemon
  reads from the XDG log path.
- **SC-O-003**: Enabling `xi.sources.observability.prometheus.port = 9321` makes
  the exporter bind to `127.0.0.1:9321`; `curl http://127.0.0.1:9321/metrics`
  lists every metric named above.
- **SC-O-004**: `/healthz` returns 200 after a successful tick and 503 after a
  tick that failed to acquire the lock, with distinct response bodies naming the
  reason.
- **SC-O-005**: `xi sources events --since '1h ago'` lists all events in the
  last hour sorted descending by time, as a two-row envelope (schema version
  header + body rows).
- **SC-O-006**: Grep-ing the log, metric endpoint, event stream, and trace
  export for the string of a configured token returns zero matches.
- **SC-O-007**: Log rotation keeps 30 days by default; a daily rotation runs at
  midnight local time.
- **SC-O-008**: OTLP export, when enabled, carries the root span of every tick
  within 5 seconds of tick end.
