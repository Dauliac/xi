# Project: xi

Rust CLI that wraps `nix` commands and orchestrates a per-flake devshell daemon,
formatter, CI pipeline, and agent-context surface. Cross-platform (Linux,
macOS); Nix is required at runtime.

## Workspace

```
crates/
├── xi              # binary entry point + top-level CLI plumbing
├── xi-core         # shared types: FlakeOutput taxonomy, installable parsing, suggestions, completions
├── xi-flake        # flake subcommand family (show, check, ci, fmt, project config)
├── xi-agent        # agent-facing structured context (outputs, devshell, stage, validate)
├── xi-develop      # per-flake devshell daemon (state, jobs, IPC over Unix socket)
├── xi-nixos / xi-home / xi-darwin  # config subcommands per platform
├── xi-search       # search / info surface
├── xi-remote       # remote deploy backends (deploy-rs, colmena, builtin)
├── xi-diff         # diff surface
├── nix-command     # low-level Nix invocation wrapper
└── xtask           # repo automation
```

## Conventions

- **Errors**: `color-eyre` at the binary; `eyre::Result` or typed enums inside
  crates. Never `unwrap` on a runtime path.
- **CLI**: `clap` derive. Subcommand-per-crate; shared arg types live in
  `xi-core`.
- **Logs**: `tracing` with structured fields. `stdout` is machine-readable
  output; `stderr` is human-facing progress and diagnostics.
- **Serialization**: `serde_json` for machine surfaces; every response envelope
  carries a schema version.
- **Testing**: unit tests colocated with modules; BDD-style scenario tests under
  `crates/*/tests/bdd_*.rs`.
- **Nix modules**: three parallel entry points under
  `modules/{flake-parts,home-manager,nixos}/`; shared helpers under
  `modules/shared/lib/`.

## Build environment

- No global `cargo` / `rustc` — always use nix store paths or `nix develop`.
- The `nh` wrapper intercepts `nix develop -c cargo …`; prefer explicit
  `PATH="<cargo>:<rustc>:$PATH" cargo …` when scripting.
- Formatter dispatch: `xi fmt` picks the flake formatter if present, else
  `nixfmt`. Backend is a string, not an enum — any external command works.

## Cross-cutting invariants

- **FlakeOutput taxonomy** is the single source of truth for output categories
  (`xi-core/src/flake_output.rs`). Every crate that talks about "packages",
  "devShells", "checks", etc. must go through it.
- **Devshell state** is owned by the `xi-develop` daemon. Other crates read;
  they never re-derive.
- **Response envelope**: schema version + typed status
  (`Ready | Pending | Missing | Degraded`) on every machine surface, so an agent
  one minor version behind still parses the reply.
- **Never modify user files implicitly** (`.gitignore`, `flake.nix`,
  `.xi.toml`). Any write is explicit and reversible.

## Tracking

- Design work: this directory (`openspec/`).
- Live task state: `bd` (beads). Cross-reference the change folder from bead
  descriptions.
- Legacy `specs/00N-*/` directories are kept until the openspec migration is
  fully reviewed, then removed.
