# Phase 0 Research: xi-agent

**Date**: 2026-08-01

Two parallel investigations informed the plan. Findings distilled below; each decision cites the primary source.

## R1 — Agent-Skills is a real, adopted open standard

**Question**: What is the accepted format for shipping "skills" bundled with a CLI?

**Findings**:

- **Agent Skills** (spec at [agentskills.io](https://agentskills.io), stewarded by the Agentic AI Foundation, published Dec 2025) is now the format used by Claude Code, Codex, Cursor, Gemini CLI, Copilot, VS Code, OpenCode, Goose.
- **A skill is a directory** with a required `SKILL.md` (YAML frontmatter: `name` + `description`, then a Markdown body). Optional siblings: `scripts/`, `references/`, `assets/`. **No archive format** (no `.skill`, no `.zip`).
- **Install path** — de-facto: `~/.claude/skills/<name>/SKILL.md` (user), `.claude/skills/<name>/SKILL.md` (project). Codex mirrors this at `~/.codex/skills/<name>/`. Claude hardcodes `~/.claude/`; there is no XDG variable to obey.
- **Custom slash commands and skills merged** in Claude Code: `.claude/commands/deploy.md` and `.claude/skills/deploy/SKILL.md` both create `/deploy`.
- **`openai/skills` is deprecated** — Codex adopted the shared standard.

**Decisions**:

1. Ship skills as directories in `crates/xi-agent/skills/xi-*/SKILL.md`. Source of truth in git, next to the code that uses them.
2. Use `include_dir!` (≈0.7) at compile time to embed the tree into the binary. Trade-off: recompile-on-skill-edit; acceptable given the low churn of a v1 skill set. `rust-embed` was considered but its debug/release split is unnecessary complexity here.
3. Use `directories` (≈5) — not `dirs`, not `xdg` — for cross-platform home resolution, then join `.claude/skills/xi-<name>` or `.codex/skills/xi-<name>` manually.
4. Provide `xi agent install [--user|--project] [--force]` writing atomically via `tempfile::persist`. Skip files whose SHA-256 already matches (`sha2`).
5. HM module symlinks the same in-repo directory tree rather than materialising it — one source, no duplicated bytes.

## R2 — There is no all-in-one Rust crate for agent-runtime config; stack the primitives

**Question**: What crates manage Claude Code / Codex / MCP config programmatically?

**Findings**:

- **`rmcp`** (v3.1.0, official Anthropic/MCP Rust SDK under `modelcontextprotocol/rust-sdk`) covers the protocol only, no config surface. Not needed for v1.
- **`claude_settings`** (v0.7.2) models the four-tier Claude settings hierarchy but **does not model `mcpServers`**. MCP entries live in `~/.claude.json` top-level, not in `~/.claude/settings.json` — a documented drift ([claude-code#4976](https://github.com/anthropics/claude-code/issues/4976)).
- **`toml_edit`** (v0.25.13, ~58M downloads/month) is the standard for format-preserving TOML edits — the right choice for Codex `~/.codex/config.toml`.
- **`jsonc-parser`** (v0.33.1) offers a CST edit API preserving comments and order for JSON with comments. Optional; `serde_json` with `preserve_order` is often enough.
- **AGENTS.md** is an open spec (Linux Foundation Agentic AI Foundation, Dec 2025). Plain Markdown; no parser needed. The `agentsmd` crate is a *template renderer* only, not a parser.
- **Cross-platform home paths**: `directories` (or `etcetera`) beats `dirs` for stability and Windows support (though we do not target Windows).

**Decisions**:

1. v1 does **not** touch runtime configs. Skills are pure directory drops; no `~/.claude.json` edit, no `~/.codex/config.toml` edit. This drops entire classes of drift risk.
2. Reserve `toml_edit` + `serde_json` for a future MCP-install phase (out of scope for v1).
3. Do **not** depend on `rmcp` until MCP is scoped.
4. Reference the four-tier Claude settings model only in docs — do not attempt to write it.

## Consolidated dependency delta

| Crate | Version | Reason | Phase |
| --- | --- | --- | --- |
| `include_dir` | ~0.7 | Embed `skills/` tree | 1 |
| `directories` | ~5 | Resolve `~/.claude`, `~/.codex` | 3 (install) |
| `tempfile` | latest | Atomic write via `persist` | 3 (install) |
| `sha2` | ~0.10 | Content-hash idempotency | 3 (install) |
| `serde` / `serde_json` | already in workspace | Response envelope | 1 |
| `toml_edit` | (deferred) | Codex config edits (post-v1) | out |
| `rmcp` | (deferred) | MCP server (post-v1) | out |

## Non-obvious risks

- **Skill directory naming collision**: multiple installed CLIs may create `~/.claude/skills/context/`. Namespace every skill with `xi-` (already done in the file layout).
- **Symlink vs copy at HM activation**: symlinks are cheaper and always up-to-date, but some agent runtimes stat via `realpath` and may re-resolve into the Nix store on rebuild. Acceptable — that IS the intended behaviour. Documented in `quickstart.md`.
- **`include_dir!` at compile time couples binary size to skill count**: with ≈5 skills each ~40 lines, cost is negligible. Revisit if we hit tens of skills.
- **Daemon coupling**: `xi agent devshell` and `context` call the existing `xi-develop` daemon; failures must be surfaced in the envelope (not panicked). Contract test enforces this.
