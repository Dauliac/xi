# Implementation Plan: xi-agent

**Branch**: `001-xi-agent` | **Date**: 2026-08-01 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from [`spec.md`](./spec.md)

## Summary

`xi-agent` adds a new workspace crate that exposes xi's project knowledge to AI coding agents through a JSON-first CLI (`xi agent …`) and a set of embedded `SKILL.md` files installable per-user, per-project, or via a new Home Manager module option (`programs.xi.agents.*`). The crate reuses existing xi machinery: `xi-core::flake_output` for the output taxonomy, the `xi-develop` daemon for devshell state, and the existing `nix ... --json` code paths in `xi-flake` for evaluation. Skills are the source of truth in-repo under `crates/xi-agent/skills/`, embedded via `include_dir!` for `xi agent install`, and symlinked by the Nix module for HM users. MCP is deliberately excluded from v1.

## Technical Context

**Language/Version**: Rust (workspace pinned to `cargo-1.95.0` / `rustc-bootstrap-1.95.0` per `MEMORY.md`).

**Primary Dependencies** (new to workspace):

- `include_dir` ~0.7 — compile-time embed of the `skills/` tree
- `directories` ~5 — cross-platform resolution of `~/.claude`, `~/.codex`
- `toml_edit` ~0.25 — format-preserving edits to Codex `config.toml` (for the install path)
- `serde_json` (existing), `serde` (existing) — schema types
- `tempfile` (add if not present) — atomic rename during install
- `sha2` — content hashing so `install --force` only rewrites changed files

**Reused workspace crates**: `xi-core` (flake_output, dirs, style, command), `xi-flake` (show, project_config, doctor), `xi-develop` (daemon client, protocol), `xi-diff` (for stage/manifest diffing).

**Storage**: none — all outputs are computed on demand from `flake.nix`, the `xi-develop` daemon socket, git state, and `.xi.toml`. A short-lived on-disk cache under `$XDG_CACHE_HOME/xi/agent/` is optional (Phase 3+); v1 is stateless.

**Testing**: `cargo nextest` (repo standard, `.config/nextest.toml`). Contract tests use `.feature` files under `crates/xi-agent/tests/`, matching the BDD pattern already used by `xi-develop` (`bdd_*` tests). Schema tests use `insta` snapshots to catch accidental schema drift.

**Target Platform**: Linux x86_64 and aarch64, macOS x86_64 and aarch64 — same set as the rest of the workspace. Windows explicitly out of scope.

**Project Type**: Rust workspace crate + Nix module set (`modules/{flake-parts,home-manager,nixos}/agents.nix`).

**Performance Goals**:

- Warm-daemon `xi agent context`: p95 ≤ 500 ms (SC-001) — dominated by one socket round-trip plus one cached flake-show.
- Cold `xi agent context`: p95 ≤ 5 s (SC-002) — bounded by `nix flake show --json` on a lukewarm evaluator.
- `xi agent install`: idempotent under 200 ms once the binary is loaded.

**Constraints**: read-only by default; every write is opt-in and goes through an atomic `tempfile::persist`. No network access needed for install (`include_dir!` embeds the payload). No secrets read or written.

**Scale/Scope**: designed for one developer machine per install; the daemon-backed calls piggy-back on an existing single-instance daemon. Not designed for a shared server.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

`.specify/memory/constitution.md` is still the template placeholder in this repo. Applying the defaults from the template's example principles as a light-touch gate:

| Principle (default) | Compliance |
| --- | --- |
| Library-first | ✅ New capability lives in its own crate `xi-agent`; no god-crate. |
| CLI interface with text I/O | ✅ Every subcommand emits JSON on stdout, human text on stderr — the exact pattern the template calls out. |
| Test-first | ✅ Contract tests (`.feature`) + schema snapshots authored before implementation for each subcommand. |
| Integration testing at contract seams | ✅ One integration test per subcommand asserts the schema envelope; daemon path is exercised end-to-end with a real socket. |
| Simplicity / YAGNI | ✅ MCP deferred, cache deferred, no daemonisation of `xi-agent` itself, no plugin marketplace. |

No violations to record in Complexity Tracking. When the constitution is filled in, re-run this gate.

## Project Structure

### Documentation (this feature)

```text
specs/001-xi-agent/
├── plan.md              # This file
├── spec.md              # Feature spec (WHAT)
├── research.md          # Phase 0 — accepted standards + crate choices
├── data-model.md        # Phase 1 — response envelope, entity schemas
├── quickstart.md        # Phase 1 — developer setup + first agent turn
├── contracts/
│   └── schema.md        # Phase 1 — per-subcommand JSON contracts
└── checklists/
    └── requirements.md  # Spec quality checklist
```

### Source Code (repository root)

```text
crates/xi-agent/
├── Cargo.toml
├── src/
│   ├── lib.rs           # Re-exports; workspace binding into `xi` interface
│   ├── args.rs          # Clap subcommands (context, outputs, devshell, stage,
│   │                    #                  manifest, validate, install)
│   ├── schema.rs        # Serde envelope + entity types (versioned)
│   ├── context.rs       # `xi agent context` — composes the others
│   ├── outputs.rs       # Wraps xi_core::flake_output + xi_flake::show
│   ├── devshell.rs      # Talks to xi_develop::daemon::client
│   ├── stage.rs         # git + flake source graph
│   ├── manifest.rs      # flake imports/modules graph
│   ├── validate.rs      # Plan derivation + streaming executor
│   └── install.rs       # include_dir! extraction + atomic writes
├── skills/              # Source of truth, embedded via include_dir!
│   ├── xi-flake-outputs/SKILL.md
│   ├── xi-devshell-state/SKILL.md
│   ├── xi-validate-changes/SKILL.md
│   ├── xi-stage-and-manifest/SKILL.md
│   └── xi-agent-context/SKILL.md
└── tests/
    ├── bdd_context.rs
    ├── bdd_outputs.rs
    ├── bdd_devshell.rs
    ├── bdd_stage.rs
    ├── bdd_manifest.rs
    ├── bdd_validate.rs
    └── bdd_install.rs

crates/xi/src/interface.rs      # Add `Agent(xi_agent::args::AgentArgs)` variant
crates/xi/Cargo.toml            # Add xi-agent workspace dep

modules/flake-parts/agents.nix   # New — flake-parts side of the option tree
modules/home-manager/agents.nix  # New — HM install of skills + optional wiring
modules/nixos/agents.nix         # New — parity with HM for system-wide install
shared/lib/wrapper.nix           # Extend `mkToolPackages`-style helper if needed
```

**Structure Decision**: New workspace crate `xi-agent`, mirroring the pattern established by `xi-deploy` and `xi-diff` (small, focused, JSON-first). Skills live in-tree under the crate to keep the source of truth close to the code that consumes them. All three Nix module trees receive an `agents.nix` sibling so users on flake-parts, HM, or NixOS get the same option surface — matching the existing `fmt.nix` / `wrapper.nix` symmetry across `modules/*/`.

### Delivery phases (translated into beads after this plan is accepted)

1. **Crate + schema + context subcommand + one skill** — validates the envelope on a real workflow.
2. **Remaining read subcommands + all 5 skills** — completes the read surface.
3. **`install` subcommand + `programs.xi.agents` Nix module (all three module trees)** — completes v1.

Each phase is one PR-scale unit; each phase is one epic-bead with child task beads.

## Complexity Tracking

> Fill ONLY if Constitution Check has violations that must be justified.

None. Every choice above matches an existing pattern in the workspace.
