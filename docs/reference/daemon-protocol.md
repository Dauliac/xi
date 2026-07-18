# Daemon Protocol Reference

The xi develop daemon communicates with shell prompt hooks over a Unix domain
socket using a length-prefixed JSON protocol.

## Socket location

```
$XDG_RUNTIME_DIR/xi-develop/daemon.sock
```

## Wire format

Each message is encoded as:

```
[4 bytes: little-endian uint32 length][N bytes: UTF-8 JSON payload]
```

Maximum message size: 16 MB. Messages exceeding this limit are rejected with
an `InvalidData` error.

## Timeouts

| Operation | Timeout |
|-----------|---------|
| Connect | 500 ms |
| Read | 5 s |
| Write | 5 s |

## Request types

All requests use a tagged union with a `"type"` field.

### PromptRequest

Sent by the shell prompt hook on every prompt display.

```json
{
  "type": "Prompt",
  "consumer_pid": 12345,
  "target": "default",
  "cwd": "/home/user/project",
  "is_subshell": false,
  "parent_pid": null
}
```

### EvalRequest

Trigger an immediate re-evaluation.

```json
{
  "type": "Eval"
}
```

### CachePushRequest

Push a store path to the binary cache. The daemon handles this
asynchronously — it spawns a background thread and returns immediately.

```json
{
  "type": "CachePush",
  "store_path": "/nix/store/...",
  "cache_url": "s3://my-cache",
  "push_command": null,
  "sign_key": "/etc/nix/signing-key"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `store_path` | string | Nix store path to push |
| `cache_url` | string | Cache URL (empty = use default from config) |
| `push_command` | string or null | Custom push command override |
| `sign_key` | string or null | Signing key path |

The daemon also periodically drains its persistent push queue (failed pushes
that were enqueued for retry). The drain interval is controlled by
`cache.queue_drain_interval` in `config.toml`.

### StatusRequest

Query daemon state.

```json
{
  "type": "Status"
}
```

### DeregisterRequest

Remove a consumer from the registry.

```json
{
  "type": "Deregister",
  "consumer_pid": 12345
}
```

### ShutdownRequest

Request daemon shutdown.

```json
{
  "type": "Shutdown"
}
```

## Response types

### PromptResponse

Returned in response to `PromptRequest`.

```json
{
  "type": "Prompt",
  "should_source_env": true,
  "should_source_hook": false,
  "should_exit": false,
  "should_spawn": null,
  "notifications": [
    {
      "kind": "Success",
      "message": "Activated (hello, jq)"
    }
  ],
  "daemon_state": "Ready",
  "is_trusted": true
}
```

| Field | Type | Description |
|-------|------|-------------|
| `should_source_env` | bool | Source the environment file on next prompt |
| `should_source_hook` | bool | Source the hook file on next prompt |
| `should_exit` | bool | Exit the subshell (e.g. untrusted) |
| `should_spawn` | object or null | Spawn a nested subshell |
| `notifications` | list | Notifications to display |
| `daemon_state` | string | Current daemon state |
| `is_trusted` | bool | Whether the flake is trusted |

### StatusResponse

```json
{
  "type": "Status",
  "state": "Ready",
  "version": "4.4.0",
  "consumers": 2,
  "uptime_secs": 3600
}
```

## Daemon states

| State | Description |
|-------|-------------|
| `Starting` | Daemon is initialising |
| `Evaluating` | Running nix evaluation |
| `Ready` | Evaluation complete, serving environment |
| `BuildFailed` | Last evaluation failed, serving cached env |
| `WatcherDegraded` | File watcher failed, daemon still operational |
| `ConfigError` | Configuration error detected |
| `ShuttingDown` | Daemon is shutting down |

## Notification kinds

| Kind | Icon | Description |
|------|------|-------------|
| `Loading` | spinner | Operation in progress |
| `Success` | check | Operation completed |
| `Info` | dot | Informational message |
| `Warn` | triangle | Warning |
| `Error` | cross | Error occurred |

All notifications are prefixed with `[xi]` when displayed.

## Consumer lifecycle

1. Shell prompt hook sends `PromptRequest` on every prompt
2. Daemon registers consumer by PID in the shell registry
3. Each consumer tracks its own generation counters for env and hook
4. Daemon pushes notifications via the bus; per-consumer cursors prevent
   duplicates
5. Dead consumers are reaped via `/proc/{pid}` checks
6. On exit, the prompt hook sends `DeregisterRequest`

## Error recovery

- If the daemon is unresponsive, the client retries up to 3 times
- After 3 failures, the client spawns a new daemon
- On version mismatch (detected via `StatusResponse`), the client sends
  `Shutdown` to the old daemon and starts a new one
- Exponential backoff on evaluation failures: 30s, 60s, 120s, 240s, 300s cap
- File changes reset the backoff timer immediately
