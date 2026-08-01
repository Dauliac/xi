# Phase 1 Contracts: xi-agent JSON surface

Every subcommand emits a single `Envelope` on stdout except `validate --run`, which emits newline-delimited `Envelope`s (JSONL). Stderr is reserved for human progress.

## Contract 1 — `xi agent context`

**Invocation**: `xi agent context [--system <name>] [--no-daemon]`

**Behaviour**:

- Composes `outputs`, `devshell`, `git`, `validation` (plan-only) into one call.
- MUST return within 500 ms with a warm daemon and cached flake-show; MUST return partial data with `errors[]` populated within 5 s on cold state.
- `--no-daemon`: never touches the socket; `devshell.state` returns `NotRunning`.

**Response**: `Envelope<AgentContext>` (see [data-model.md](../data-model.md)).

**Exit codes**: 0 on any parseable response (including partial with errors). 2 on argument error. 1 on unrecoverable panic (should never occur — asserted in contract test).

## Contract 2 — `xi agent outputs`

**Invocation**: `xi agent outputs [--all-systems] [--system <name>] [--include-hidden]`

**Behaviour**:

- Wraps `nix flake show --json` (already used in `xi-flake/src/lib.rs`).
- Applies the same hidden-category filter as `xi flake show` unless `--include-hidden`.
- Never mutates the flake.

**Response**: `Envelope<OutputsPayload>`.

## Contract 3 — `xi agent devshell`

**Invocation**: `xi agent devshell`

**Behaviour**:

- Opens the xi-develop daemon Unix socket; sends `DaemonRequest::Status`.
- If the socket does not exist: returns `DevshellState::NotRunning` with a `hint` pointing at `xi develop`.
- Never blocks the socket for more than 500 ms; on timeout returns `Degraded` with a `daemon.timeout` diagnostic.

**Response**: `Envelope<DevshellPayload>`.

## Contract 4 — `xi agent stage`

**Invocation**: `xi agent stage`

**Behaviour**:

- Reads git status (via `git2` or by shelling out — decided in implementation).
- Cross-references `flake.nix` and its transitive imports to identify which unstaged files a build would need.
- `entries[]` is deterministically sorted by path.

**Response**: `Envelope<StagePayload>`.

## Contract 5 — `xi agent manifest`

**Invocation**: `xi agent manifest`

**Behaviour**:

- Walks `flake.nix` imports statically (no evaluation) plus any `modules/`, `shared/`, etc. explicitly imported from flake code.
- Returns absolute-relative paths from workspace root, deduplicated.

**Response**: `Envelope<ManifestPayload>`.

## Contract 6 — `xi agent validate`

**Invocation**: `xi agent validate [--run] [--jobs N] [--fail-fast]`

**Behaviour**:

- Without `--run`: emits one `Envelope<ValidationPlan>` and exits 0.
- With `--run`: emits one `Envelope<ValidationEvent>` per line (JSONL) as steps execute, then a final `Envelope<ValidationEvent::Complete>` and exits 0 if all `blocking=true` steps passed, else 1.
- Plan derivation reads `.xi.toml` for enabled backends and inspects the flake for `checks`, `formatter`, `nixosConfigurations` (dry-build), etc.

**Response**: as above.

## Contract 7 — `xi agent install`

**Invocation**: `xi agent install [--user|--project] [--target claude-code|codex|all] [--force] [--dry-run]`

**Behaviour**:

- Extracts embedded skills via `include_dir!`.
- Writes to `<home>/.claude/skills/xi-<name>/` for Claude Code, `<home>/.codex/skills/xi-<name>/` for Codex (per Phase 0 research).
- Idempotent: SHA-256 of each file compared to on-disk; unchanged files marked `UpToDate`.
- `--project` writes into `./.claude/skills/xi-<name>/` and `./.codex/skills/xi-<name>/` under the workspace root.
- `--dry-run` emits the plan without writing.
- Atomic per file: `tempfile::NamedTempFile::persist` into the final location.

**Response**: `Envelope<InstallPayload>`.

## Envelope stability contract

A dedicated test suite (`bdd_schema_stability.rs`) enforces:

1. Every command emits a valid `Envelope<_>` on stdout, even on error.
2. `stderr` never contains JSON — always human text — so pipes can be trusted.
3. Snapshot tests (`insta`) freeze the shape of each payload; changes require reviewer opt-in.
4. Every published field is present on either success or in an unambiguously-typed error variant — no free-form untyped strings in place of enums.
