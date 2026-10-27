# Tasks: xi-sources-observability

Live state in beads under `change:xi-sources-observability`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-observability/spec.md`.
- [ ] 1.2 Cross-link with gitops (closes P4 Prometheus item), credentials
      (redaction), nix-modules (option tree).

## 2. Tracing wire-up (P1)

- [ ] 2.1 `crates/xi-sources/src/observability.rs` initialises a
      `tracing_subscriber::Registry` with a ndjson formatting layer.
- [ ] 2.2 Fields fixed per span type: tick root carries
      `sources_id, remote, actor_name`; phase child carries
      `phase, outcome, duration_ms, old_sha, new_sha`.
- [ ] 2.3 Panic hook funnels panics to a tracing error with full backtrace.

## 3. Log destinations (P1)

- [ ] 3.1 System scope: use `tracing-journald` to write to the journal; service
      unit sets `StandardOutput=journal`.
- [ ] 3.2 User scope: use `tracing-appender::rolling::daily` writing to
      `$XDG_STATE_HOME/xi/logs/sources-daemon.log`.
- [ ] 3.3 Log level configurable via `xi.sources.observability.log-level` (enum
      trace|debug|info| warn|error, default info).

## 4. Operator readouts (P1)

- [ ] 4.1 `xi sources log tail [--follow] [--since] [--level]` reads from the
      configured sink.
- [ ] 4.2 `xi sources events` queries a lightweight sqlite db in the state dir
      (`events.db`) that the daemon writes alongside the log; each event is one
      row.

## 5. Prometheus exporter (P1)

- [ ] 5.1 `prometheus` crate; counters, histograms, gauges per spec list.
- [ ] 5.2 Expose on `127.0.0.1:<port>` only; refuse non-loopback binds.
- [ ] 5.3 `/metrics` for data, `/healthz` for health; nothing else.

## 6. OTLP export (P2)

- [ ] 6.1 `opentelemetry-otlp` crate behind a cargo feature flag.
- [ ] 6.2 When `xi.sources.observability.otlp.endpoint` is set, export root
      spans with batching (max 5s latency).

## 7. Redaction (P1)

- [ ] 7.1 The opaque credential type from [[xi-sources-credentials]] refuses
      `Serialize`; the tracing layer rejects any field attempting to serialize
      it.
- [ ] 7.2 A compile-time lint runs over the daemon crate rejecting
      `info!(token = ...)`, `metric.set(... token ...)`, etc.

## 8. Tests (P1)

- [ ] 8.1 BDD: tick emits root + 5 phase spans.
- [ ] 8.2 BDD: Prometheus endpoint reachable and non-empty.
- [ ] 8.3 BDD: `/healthz` 200 vs 503 transition.
- [ ] 8.4 BDD: redaction (grep for configured token returns zero).
- [ ] 8.5 BDD: log rotation at midnight.

## 9. Follow-ups (P4)

- [ ] 9.1 Grafana dashboard JSON template. Deferred.
- [ ] 9.2 Alertmanager rule stubs. Deferred.
- [ ] 9.3 Push gateway support for ephemeral workstation daemons. Deferred.
