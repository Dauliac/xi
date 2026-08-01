# Feature Specification: xi-agent — structured project context for AI coding agents

**Feature Branch**: `001-xi-agent`

**Created**: 2026-08-01

**Status**: Draft

**Input**: User description: "I want to develop skills and maybe an MCP easy to install with nix hm and to help agents to know, flake outputs, devshell state, files to stage, files to manifest, xi commands to run to validated developments."

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Agent discovers what the flake actually offers (Priority: P1)

A developer working in a Nix flake asks their coding agent to "run the tests" or "build the CLI." Today the agent must guess: read `flake.nix`, run `nix flake show` (slow), or misfire. With xi-agent, the agent invokes one command that returns a structured enumeration of every reachable output — packages, devShells, checks, per-system flake configurations, deployment targets — with a semantic kind attached to each (derivation, module, configuration, overlay, lib). The agent then picks the right target without a round-trip.

**Why this priority**: This is the single most repeated failure mode of coding agents inside a Nix flake — they don't know what exists, so they hallucinate names or shell out to slow evaluators multiple times per turn. Fixing this alone materially changes agent quality even before anything else ships.

**Independent Test**: From a clean flake, invoke the outputs discovery command. The result must list every real output the flake exposes for the current system, categorised by kind, in one call. Delivered value: an agent can now answer "what can I build here?" without guessing.

**Acceptance Scenarios**:

1. **Given** a flake with packages, devShells, and checks for the current system, **When** the agent asks for the outputs, **Then** it receives one structured document naming each output and its kind, using the same output taxonomy as `xi flake show`.
2. **Given** a flake that also declares `nixosConfigurations` or `homeConfigurations`, **When** the agent asks for outputs, **Then** those per-host configurations are returned with the same taxonomy without a second call.
3. **Given** the agent requests outputs while the network is unavailable, **When** the command runs, **Then** it either serves cached data or reports the failure in the structured envelope rather than hanging.

---

### User Story 2 — Agent validates the change before saying "done" (Priority: P1)

A developer edits a Nix module or a Rust crate and asks the agent to "check that it's good." Today the agent runs whatever it remembers (`cargo test`? `nix flake check`? `nix build .#`?) — often the wrong subset, sometimes duplicating work, sometimes skipping the doctor. With xi-agent, the agent asks for a validation plan tailored to what the project supports and what actually changed, then executes it and streams typed results. If any step fails, the agent surfaces the failing step and stops.

**Why this priority**: The whole point of an agent completing work is confidence that the work is correct. Without a project-aware validation list the agent's "done" is not trustworthy, which forces the developer to re-check everything manually. This is what turns xi from a nice CLI into a reliable co-worker.

**Independent Test**: Ask for the validation plan; it lists an ordered set of concrete checks (formatter, doctor, flake check, tests, CI parity) each labelled with why it must run and whether it blocks completion. Executing the plan produces one machine-readable result per step. Delivered value: an agent can answer "have I broken anything?" with a definitive yes/no.

**Acceptance Scenarios**:

1. **Given** a project with a formatter, a doctor check, and flake checks configured, **When** the agent asks for the validation plan, **Then** the plan names each step, its purpose, and whether it blocks completion.
2. **Given** a plan returned to the agent, **When** the agent asks to execute it, **Then** each step's outcome (pass, fail, skipped, duration) streams back as it happens, in order.
3. **Given** one step fails partway through, **When** later steps depend on it, **Then** those dependent steps are reported as blocked rather than silently skipped or re-run.

---

### User Story 3 — Agent understands the current devshell before running tools (Priority: P2)

A developer's devshell is either warm (tools on PATH match the flake), cold (never entered), or stale (a `flake.nix` edit hasn't been picked up). Agents don't know which. They then run `cargo` and get "command not found" or run against the wrong toolchain and get a confusing failure. With xi-agent, one call reports whether the devshell is current, what target it is, how many packages it contains, and whether an evaluation is in flight — reusing the existing `xi develop` daemon status.

**Why this priority**: Prevents a common class of confusing wasted agent turns ("command not found," "wrong rustc version"). Lower priority than P1s only because a developer can usually see this from their prompt; P1s are invisible to them.

**Independent Test**: With the devshell in any state (missing, ready, evaluating, stale), the command reports the state, the target, and whether the current PATH matches. Delivered value: an agent knows whether to prompt the developer to enter the devshell before running tools.

**Acceptance Scenarios**:

1. **Given** the daemon reports state `Ready` and the flake input hash matches the daemon's target, **When** the agent asks for devshell state, **Then** it receives `ready` with the target name and package count.
2. **Given** the daemon has never been started for this project, **When** the agent asks for devshell state, **Then** it receives `not-running` plus the exact command to enter the shell.
3. **Given** the daemon is mid-evaluation, **When** the agent asks, **Then** it receives `evaluating` with the current phase — the call does not block waiting for completion.

---

### User Story 4 — Agent knows which files must be staged before Nix sees them (Priority: P2)

Nix builds only see files known to git (unless flake sources are opted out). Agents forget this and produce "the code is right but the build fails" bug reports. With xi-agent, one call lists tracked-but-modified files, untracked files that the flake likely references, and files ignored by `.gitignore` that a build path depends on — annotated with which flake output would need them. The agent can then propose the exact `git add` list before running `nix build`.

**Why this priority**: A frequent source of "why doesn't my change take effect?" debugging. P2 because it's less common than the P1s and can be worked around by staging everything.

**Independent Test**: With modified files present, invoke the stage inspection command. The result identifies files that would not be visible to Nix and pairs each with the flake output that references it. Delivered value: the agent proposes the minimal correct `git add`.

**Acceptance Scenarios**:

1. **Given** a modified but untracked `.nix` file referenced by `flake.nix`, **When** the agent asks what to stage, **Then** the file is listed with the reference chain that points to it.
2. **Given** a modified tracked file, **When** the agent asks, **Then** the file is listed once, not duplicated, with a `staged: false` flag.
3. **Given** the working tree is clean, **When** the agent asks, **Then** the response is an explicit empty list, not an error.

---

### User Story 5 — Everything installs through one Home Manager option (Priority: P2)

A developer opens a new laptop, sets `programs.xi.agents.enable = true`, and rebuilds. Every skill lands in the agent runtimes they use (Claude Code, Codex). No pip install, no manual `git clone`, no per-agent copy step. On upgrade the skills change with the flake, atomically.

**Why this priority**: This is the difference between "a fun demo" and "a tool I actually keep." Without declarative install, the skills silently drift out of date on every machine.

**Independent Test**: On a fresh machine with Home Manager, toggling the option installs the skills into the expected agent locations; toggling it off removes them; changing the flake input version updates them on next rebuild. Delivered value: reproducible, upgrade-safe agent tooling.

**Acceptance Scenarios**:

1. **Given** `programs.xi.agents.enable = true`, **When** Home Manager activates, **Then** each supported agent runtime finds the skills at its documented location.
2. **Given** the developer disables a specific target (e.g., Codex), **When** Home Manager activates, **Then** only the enabled targets receive skills.
3. **Given** the flake input is bumped, **When** Home Manager activates, **Then** the previously installed skills are replaced atomically with no half-installed state.

---

### Edge Cases

- Flake evaluation fails partway through outputs discovery → response returns partial results with an explicit `errors` section rather than a hard failure.
- Non-flake project (`flake.nix` absent) → every subcommand returns a well-formed "not a flake" envelope, not a stack trace.
- `.xi.toml` overrides conflict with `flake.nix` (e.g., formatter backend disagreement) → context response surfaces both values with a `conflict: true` marker.
- Agent runtime does not exist on the machine (Codex not installed) → the install path skips it silently; enabling the target when the runtime is missing is not an error.
- Two agent runtimes claim the same skill directory → install writes to both without duplication and without symlink loops.
- Multi-user machine, symlinks from multiple `xi` versions → newest wins; older symlinks are replaced atomically.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST expose flake outputs as structured, machine-readable data classified by the taxonomy already defined for `xi flake show` (packages, devShells, checks, apps, per-host configurations, overlays, libraries).
- **FR-002**: The system MUST report devshell state including at minimum: whether it is loaded, its target, whether it is stale relative to `flake.nix`, and whether an evaluation is currently in flight.
- **FR-003**: The system MUST produce an ordered validation plan derived from the project's configured checks, formatter, doctor rules, and test targets, with each step annotated with purpose and whether it blocks completion.
- **FR-004**: The system MUST be able to execute the validation plan and stream one structured result per step (status, duration, error summary if any).
- **FR-005**: The system MUST identify files that would not be visible to a Nix build (untracked or ignored but referenced by the flake) and pair each with the referring flake path.
- **FR-006**: The system MUST enumerate files reachable from `flake.nix` through the module and imports graph as the "flake manifest."
- **FR-007**: The system MUST offer a one-shot "context" call that composes the other calls into a single structured document suitable for an agent's opening turn.
- **FR-008**: Every response MUST use a stable, versioned schema, emit machine output on stdout, and reserve stderr for human-facing progress and diagnostics.
- **FR-009**: Every response MUST include the schema version and MUST be safe to consume from an agent that only knows an older minor version of the schema (additive changes only within a major version).
- **FR-010**: The system MUST ship agent skills — one per user story workflow — using the Agent Skills convention (`SKILL.md` files in named directories, discoverable by Claude Code and Codex without extra configuration).
- **FR-011**: The system MUST provide an install command that writes skills into the standard per-agent locations, atomically, idempotently, and reversibly.
- **FR-012**: The system MUST provide a Home Manager option (`programs.xi.agents.enable`) that installs the skills declaratively, with per-agent-target toggles.
- **FR-013**: The system MUST work offline for read operations that do not require Nix evaluation, and MUST degrade gracefully (reporting the cause in the response envelope) when evaluation is required but not possible.
- **FR-014**: The system MUST NOT require any secret or credential at runtime for the read-only surface.
- **FR-015**: The system MUST NOT modify the developer's project files (`.gitignore`, `flake.nix`, `.xi.toml`) implicitly; any write is explicit and reversible.

### Key Entities

- **Flake context**: composed snapshot of flake path, current system, git state, devshell state, and configured validation targets at a point in time.
- **Flake output**: a single named output on the flake, categorised by output type and semantic kind (derivation, configuration, module, overlay, lib, app, deployment target).
- **Devshell state**: the daemon's view of the current shell — one of `not-running`, `evaluating`, `ready`, `stale`, `degraded` — plus target name, package count, and staleness reason if any.
- **Validation step**: one command in the validation plan — the command to run, why it runs, whether it blocks completion, and its result once executed.
- **Stage entry**: one file (relative path, tracked/untracked/ignored, staged yes/no) and the flake reference chain that reaches it, if any.
- **Manifest entry**: one file reachable from `flake.nix` through imports.
- **Agent skill**: one `SKILL.md` document with YAML frontmatter (`name`, `description`) that describes to an agent when and how to use one of the workflows above.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: For a project with a warmed devshell daemon, an agent can obtain the full context (outputs, devshell state, validation plan) in under 500 ms in the common case, so it fits inside a single agent turn without a visible pause.
- **SC-002**: On a fresh clone, the first cold call returns useful (possibly partial) context in under 5 s, degrading gracefully rather than blocking indefinitely.
- **SC-003**: An agent equipped with the skills, on a repo it has never seen, correctly identifies at least the packages, devShells, checks, and validation plan on its first turn — measured across at least three real projects.
- **SC-004**: A validation-plan execution reports a definitive per-step pass/fail for every configured check; no step is silently skipped.
- **SC-005**: Enabling `programs.xi.agents.enable = true` and rebuilding installs skills to all opted-in agent runtimes in one Home Manager activation cycle, with no manual follow-up.
- **SC-006**: Toggling `programs.xi.agents.enable = false` and rebuilding removes every previously installed skill file with no orphan artefacts.
- **SC-007**: Response schemas are documented once and the same shape is served to every agent runtime (no per-runtime forks), verified by a schema conformance test.

## Assumptions

- The developer is on a Linux or macOS machine with Nix installed and `xi` on PATH. Windows is out of scope.
- The primary agent runtimes are Claude Code and Codex; other Agent-Skills-compatible runtimes (Cursor, Gemini CLI, OpenCode) benefit because they read the same `SKILL.md` format, but explicit wiring is not committed to for v1.
- The developer uses Home Manager for their user environment. Non-HM install (`xi agent install`) is a required alternative but not the primary path.
- The Agent Skills convention as published at agentskills.io in late 2025 remains the stable format for skill directories throughout v1's lifetime.
- The existing `xi develop` daemon and its JSON socket protocol continue to be the source of truth for devshell state. This feature reads from it; it does not reshape the daemon.
- MCP (Model Context Protocol) support is explicitly out of scope for v1 and will be considered in a follow-up feature.
