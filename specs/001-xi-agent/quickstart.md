# Quickstart: xi-agent

## For a developer using xi + Claude Code / Codex

**Declarative install (recommended)** — in your Home Manager config:

```nix
programs.xi.agents = {
  enable = true;
  targets = {
    claude-code.enable = true;
    codex.enable = true;
  };
};
```

Rebuild HM. Skills appear at `~/.claude/skills/xi-*/` and `~/.codex/skills/xi-*/`.

**Imperative install** — inside any project directory:

```bash
xi agent install --user            # installs into ~/.claude and ~/.codex
xi agent install --project         # installs into ./.claude and ./.codex
xi agent install --dry-run         # preview only
```

## For an agent (Claude Code, Codex, ...)

Skills are auto-discovered. The agent chooses among:

- `xi-flake-outputs` — invoke `xi agent outputs` when the user asks "what can I build here?"
- `xi-devshell-state` — invoke `xi agent devshell` before running any language toolchain that lives in the devshell.
- `xi-stage-and-manifest` — invoke `xi agent stage` before recommending `git add` or `nix build`.
- `xi-validate-changes` — invoke `xi agent validate --run` before saying "done".
- `xi-agent-context` — one-call opening turn: `xi agent context`.

Every command outputs one JSON envelope on stdout with a `schema` field. Parse it strictly; unknown fields inside `data` are safe to ignore per the versioning contract.

## For a contributor working on xi-agent

**Iterating on a skill**: edit `crates/xi-agent/skills/xi-<name>/SKILL.md`, then rebuild the crate. Because skills are `include_dir!`-embedded, changes require a `cargo build`. To iterate faster during dev, `xi agent install --project --force` writes the current skills into the project so Claude Code picks them up without a rebuild.

**Adding a new subcommand**:

1. Add a variant to `AgentArgs` (`crates/xi-agent/src/args.rs`).
2. Add the payload type to `schema.rs`.
3. Implement the runner in a new module.
4. Add a BDD `.feature` and `bdd_<name>.rs`.
5. Update `contracts/schema.md` in the spec directory.

**Adding a new agent target** (e.g. Cursor):

1. Extend `InstallTarget` and the resolver in `install.rs`.
2. Add the target's install path to `modules/{flake-parts,home-manager,nixos}/agents.nix`.
3. Extend the install BDD to cover the new target.

**Testing**:

```bash
# Use the pinned toolchain (see MEMORY.md)
PATH="/nix/store/6gcrqjdzbx71bavmmrqpl3hw1ljml7vi-cargo-1.95.0/bin:\
/nix/store/dcyd3k01988mgcjndnfxbh0qm787jh5j-rustc-bootstrap-1.95.0/bin:$PATH" \
  cargo nextest run -p xi-agent
```

## Failure modes and how they surface

| Situation | Behaviour |
| --- | --- |
| Not in a flake | `Envelope<...>` with `errors: [{ code: "workspace.not-a-flake" }]` and `data: null`. Exit 0. |
| Daemon down | `devshell.state = NotRunning`, `hint: "run xi develop"`. Exit 0. |
| `nix flake show` fails | Partial outputs (whatever was reachable) + `errors: [{ code: "flake.eval.failed", source: "nix", message }]`. |
| Skill destination unwritable | Install command emits `InstallEntry { action: Skipped("permission-denied") }` and returns exit 1. |
| SIGINT during `validate --run` | Emits `ValidationEvent::Finished { status: Skipped }` for the current step, then `Complete { all_blocking_passed: false }`. Exit 130. |
