# Phase 1 Data Model: xi-agent

**Date**: 2026-08-01

Canonical serde types for `crates/xi-agent/src/schema.rs`. Every subcommand emits an `Envelope<T>` on stdout. All names use `serde(rename_all = "kebab-case")` unless noted.

## Envelope (all responses)

```text
Envelope {
  schema:    "xi.agent/v1"       // string, versioned
  command:   String              // "context" | "outputs" | "devshell" | ...
  generated: RFC3339 timestamp
  duration_ms: u64
  data:      T                   // command-specific payload, absent on hard error
  errors:    Vec<Diagnostic>     // empty on success; can be non-empty with data set (partial)
  warnings:  Vec<Diagnostic>
}

Diagnostic {
  code:    String                // stable, e.g. "flake.eval.failed", "daemon.not-running"
  message: String                // human line
  source:  Option<String>        // e.g. subprocess name
  hint:    Option<String>        // suggested user action
}
```

Rules:

- `schema` MUST start with `xi.agent/v` followed by an integer major.
- Additive fields inside `data` are allowed within a major; removals or renames bump major.
- A response may carry both `data` and `errors` (partial success).

## `context` payload

```text
AgentContext {
  workspace:   WorkspaceInfo
  flake:       Option<FlakeSummary>
  devshell:    DevshellState
  git:         GitState
  validation:  ValidationPlan            // dry plan, not execution
  xi_config:   Option<XiConfigSummary>
}

WorkspaceInfo {
  root:        PathBuf                    // absolute
  current_system: String                  // "x86_64-linux" etc.
  xi_version:  String
}

FlakeSummary {
  path:        PathBuf                    // flake.nix location
  lock_hash:   Option<String>             // flake.lock narHash if available
  systems:     Vec<String>                // systems declared
}

GitState {
  head:        Option<String>             // commit sha
  branch:      Option<String>
  dirty:       bool
  untracked_count: u32
  ahead_behind: Option<(u32, u32)>
}

XiConfigSummary {
  path:        PathBuf                    // .xi.toml
  sections:    Vec<String>                // present top-level tables
  fmt_backend: Option<String>
}
```

## `outputs` payload

Reuses `xi_core::flake_output::{FlakeOutput, FlakeOutputKind}`.

```text
OutputsPayload {
  system:  String                          // current or --system
  outputs: Vec<OutputEntry>
  hidden:  Vec<String>                     // categories suppressed unless --all
}

OutputEntry {
  category: FlakeOutput                    // Packages | DevShells | Checks | ...
  kind:     FlakeOutputKind                // Derivation | Configuration | Overlay | ...
  name:     String                         // attribute name
  description: Option<String>
  installable: String                      // e.g. ".#packages.x86_64-linux.foo"
}
```

## `devshell` payload

Wraps `xi_develop::daemon::protocol::StatusResponse`.

```text
DevshellPayload {
  state:           DevshellState           // enum
  target:          Option<String>
  package_count:   u32
  daemon_state:    Option<DaemonState>     // proxied from xi-develop
  active_cache_pushes: Vec<String>
  entered_command: String                  // e.g. "xi develop"
}

enum DevshellState { NotRunning, Evaluating, Ready, Stale, Degraded }
```

## `stage` payload

```text
StagePayload {
  entries: Vec<StageEntry>
  clean:   bool
}

StageEntry {
  path:        PathBuf                     // relative to workspace root
  git_status:  GitStatus                   // Untracked | Modified | Ignored | Deleted
  staged:      bool
  referenced_by: Vec<FlakeReference>       // possibly empty
}

FlakeReference {
  from:  PathBuf                           // referring file
  attr:  Option<String>                    // attr path if identifiable
}

enum GitStatus { Untracked, Modified, Ignored, Deleted, Renamed }
```

## `manifest` payload

```text
ManifestPayload {
  root:  PathBuf                           // flake.nix
  files: Vec<ManifestEntry>
}

ManifestEntry {
  path:        PathBuf
  imported_by: Vec<PathBuf>                // one or more files that import this one
  kind:        ManifestKind                // FlakeRoot | Module | Overlay | Lib | Package | Other
}
```

## `validate` payload

```text
ValidationPlan {
  steps: Vec<ValidationStep>
}

ValidationStep {
  id:        String                        // stable, e.g. "fmt-check", "doctor", "flake-check"
  command:   Vec<String>                   // argv
  purpose:   String                        // human why-line
  blocking:  bool                          // failure blocks completion
  depends_on: Vec<String>                  // ids
}
```

Execution mode (`--run`) streams one `ValidationEvent` per JSONL line:

```text
enum ValidationEvent {
  Started  { id, at }
  Progress { id, message }
  Finished { id, status: Passed | Failed | Skipped | Blocked, duration_ms, error: Option<String> }
  Complete { total_ms, all_blocking_passed: bool }
}
```

## `install` payload

```text
InstallPayload {
  target:    InstallTarget                 // User | Project | Codex | ClaudeCode
  scope:     InstallScope                  // Home | Project
  entries:   Vec<InstallEntry>
}

InstallEntry {
  skill:     String                        // "xi-flake-outputs"
  path:      PathBuf                       // written path
  action:    InstallAction                 // Wrote | UpToDate | Skipped(reason)
  sha256:    String
}
```

## Version discipline

- Bump `schema` major (`xi.agent/v2`) only on incompatible change: removed field, renamed field, changed enum semantics.
- Prefer new fields with `#[serde(default)]` over new majors.
- Contract test (`bdd_schema_stability.rs`) asserts every response is `deserialize_from(&write(response))` round-trip stable.
