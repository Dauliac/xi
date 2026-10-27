# xi-agent Specification

## Purpose

`xi-agent` exposes xi's project knowledge to AI coding agents through a
JSON-first CLI (`xi agent …`) and a set of installable `SKILL.md` files. It lets
an agent discover what a Nix flake actually offers, understand the current
devshell state, know which files must be staged before Nix sees them, and derive
plus execute a project-aware validation plan — all without hallucinating names,
running slow probes, or guessing which checks constitute "done."

### Entities

- **Flake context** — composed snapshot of flake path, current system, git
  state, devshell state, and configured validation targets at a point in time.
- **Flake output** — a single named output on the flake, categorised by output
  type and semantic kind (derivation, configuration, module, overlay, lib, app,
  deployment target), using the taxonomy defined for `xi flake show`.
- **Devshell state** — the daemon's view of the current shell: one of
  `not-running`, `evaluating`, `ready`, `stale`, `degraded`, plus target name,
  package count, and staleness reason if any.
- **Validation step** — one command in the validation plan: the command to run,
  why it runs, whether it blocks completion, and its result once executed.
- **Stage entry** — one file (relative path, tracked/untracked/ignored, staged
  yes/no) and the flake reference chain that reaches it, if any.
- **Manifest entry** — one file reachable from `flake.nix` through imports.
- **Agent skill** — one `SKILL.md` document with YAML frontmatter (`name`,
  `description`) describing to an agent when and how to use one of the workflows
  above.

## Requirements

### Requirement: Structured flake output enumeration

The system MUST expose flake outputs as structured, machine-readable data
classified by the taxonomy already defined for `xi flake show` (packages,
devShells, checks, apps, per-host configurations, overlays, libraries).

#### Scenario: Packages, devShells, and checks for the current system

- **GIVEN** a flake with packages, devShells, and checks for the current system
- **WHEN** the agent asks for the outputs
- **THEN** it receives one structured document naming each output and its kind,
  using the same output taxonomy as `xi flake show`

#### Scenario: Per-host configurations returned in one call

- **GIVEN** a flake that also declares `nixosConfigurations` or
  `homeConfigurations`
- **WHEN** the agent asks for outputs
- **THEN** those per-host configurations are returned with the same taxonomy
  without a second call

#### Scenario: Partial results when evaluation fails midway

- **GIVEN** a flake whose evaluation fails partway through outputs discovery
- **WHEN** the agent asks for outputs
- **THEN** the response returns the partial results with an explicit `errors`
  section rather than a hard failure

### Requirement: Devshell state reporting

The system MUST report devshell state including at minimum: whether it is
loaded, its target, whether it is stale relative to `flake.nix`, and whether an
evaluation is currently in flight.

#### Scenario: Ready devshell reports target and package count

- **GIVEN** the daemon reports state `Ready` and the flake input hash matches
  the daemon's target
- **WHEN** the agent asks for devshell state
- **THEN** it receives `ready` with the target name and package count

#### Scenario: Never-started daemon reports how to enter the shell

- **GIVEN** the daemon has never been started for this project
- **WHEN** the agent asks for devshell state
- **THEN** it receives `not-running` plus the exact command to enter the shell

#### Scenario: Mid-evaluation state is reported without blocking

- **GIVEN** the daemon is mid-evaluation
- **WHEN** the agent asks for devshell state
- **THEN** it receives `evaluating` with the current phase — the call does not
  block waiting for completion

### Requirement: Validation plan derivation

The system MUST produce an ordered validation plan derived from the project's
configured checks, formatter, doctor rules, and test targets, with each step
annotated with purpose and whether it blocks completion.

#### Scenario: Plan enumerates configured steps with purpose and blocking flag

- **GIVEN** a project with a formatter, a doctor check, and flake checks
  configured
- **WHEN** the agent asks for the validation plan
- **THEN** the plan names each step, its purpose, and whether it blocks
  completion

### Requirement: Validation plan execution with streamed results

The system MUST be able to execute the validation plan and stream one structured
result per step (status, duration, error summary if any).

#### Scenario: Streaming per-step outcomes in order

- **GIVEN** a plan returned to the agent
- **WHEN** the agent asks to execute it
- **THEN** each step's outcome (pass, fail, skipped, duration) streams back as
  it happens, in order

#### Scenario: Failed step blocks its dependents rather than silently skipping

- **GIVEN** one step fails partway through
- **WHEN** later steps depend on it
- **THEN** those dependent steps are reported as blocked rather than silently
  skipped or re-run

### Requirement: Stage inspection

The system MUST identify files that would not be visible to a Nix build
(untracked or ignored but referenced by the flake) and pair each with the
referring flake path.

#### Scenario: Untracked .nix file referenced by flake.nix

- **GIVEN** a modified but untracked `.nix` file referenced by `flake.nix`
- **WHEN** the agent asks what to stage
- **THEN** the file is listed with the reference chain that points to it

#### Scenario: Modified tracked file listed once with staged flag

- **GIVEN** a modified tracked file
- **WHEN** the agent asks what to stage
- **THEN** the file is listed once, not duplicated, with a `staged: false` flag

#### Scenario: Clean tree returns an explicit empty list

- **GIVEN** the working tree is clean
- **WHEN** the agent asks what to stage
- **THEN** the response is an explicit empty list, not an error

### Requirement: Flake manifest enumeration

The system MUST enumerate files reachable from `flake.nix` through the module
and imports graph as the "flake manifest."

#### Scenario: Manifest reflects the imports graph

- **GIVEN** a flake that imports Nix modules and additional files
- **WHEN** the agent asks for the flake manifest
- **THEN** it receives the set of files reachable from `flake.nix` through
  imports

### Requirement: One-shot context call

The system MUST offer a one-shot "context" call that composes the other calls
into a single structured document suitable for an agent's opening turn.

#### Scenario: Context composes outputs, devshell state, and validation plan

- **GIVEN** a project with a warmed devshell daemon
- **WHEN** the agent asks for context
- **THEN** it receives a single structured document containing the flake
  outputs, current devshell state, and validation plan

#### Scenario: Non-flake project returns a well-formed envelope

- **GIVEN** a project directory without `flake.nix`
- **WHEN** the agent asks for context (or any subcommand)
- **THEN** it receives a well-formed "not a flake" envelope, not a stack trace

#### Scenario: Conflicting .xi.toml and flake.nix values are surfaced

- **GIVEN** `.xi.toml` overrides that conflict with `flake.nix` (for example a
  formatter backend disagreement)
- **WHEN** the agent asks for context
- **THEN** both values are surfaced with a `conflict: true` marker

### Requirement: Stable versioned response envelope

Every response MUST use a stable, versioned schema, emit machine output on
stdout, and reserve stderr for human-facing progress and diagnostics.

#### Scenario: JSON on stdout, diagnostics on stderr

- **WHEN** any `xi agent` subcommand runs
- **THEN** its machine-readable JSON output is written to stdout and any
  human-facing progress or diagnostics are written to stderr

### Requirement: Backwards-compatible schema evolution

Every response MUST include the schema version and MUST be safe to consume from
an agent that only knows an older minor version of the schema (additive changes
only within a major version).

#### Scenario: Older-minor consumer can still parse the response

- **GIVEN** an agent that only understands an older minor version of the
  response schema
- **WHEN** the current `xi agent` version emits a response of the same major
  version
- **THEN** the older agent can still parse the response because only additive
  changes were made

### Requirement: Agent skills shipped in standard format

The system MUST ship agent skills — one per user story workflow — using the
Agent Skills convention (`SKILL.md` files in named directories, discoverable by
Claude Code and Codex without extra configuration).

#### Scenario: Skills are discoverable at documented locations

- **GIVEN** the skills are installed
- **WHEN** a supported agent runtime (Claude Code, Codex) starts
- **THEN** each skill is discoverable via its `SKILL.md` at the runtime's
  documented location, with no extra configuration

### Requirement: Install command for skills

The system MUST provide an install command that writes skills into the standard
per-agent locations, atomically, idempotently, and reversibly.

#### Scenario: Install into runtimes that exist; skip missing ones silently

- **GIVEN** a machine where some agent runtimes are installed and others are not
  (for example Codex missing)
- **WHEN** the install command runs
- **THEN** it writes skills into the installed runtimes and silently skips the
  missing ones without error

#### Scenario: Two runtimes claim the same skill directory

- **GIVEN** two agent runtimes that claim the same skill directory
- **WHEN** the install command runs
- **THEN** it writes to both without duplication and without symlink loops

#### Scenario: Newer install replaces older symlinks atomically

- **GIVEN** a multi-user machine with symlinks from multiple `xi` versions
- **WHEN** a newer install runs
- **THEN** the newest wins and older symlinks are replaced atomically

### Requirement: Home Manager option installs skills declaratively

The system MUST provide a Home Manager option (`programs.xi.agents.enable`) that
installs the skills declaratively, with per-agent-target toggles.

#### Scenario: Enabling the option installs skills to opted-in runtimes

- **GIVEN** `programs.xi.agents.enable = true`
- **WHEN** Home Manager activates
- **THEN** each supported agent runtime finds the skills at its documented
  location

#### Scenario: Per-target toggle installs only enabled targets

- **GIVEN** the developer disables a specific target (for example Codex)
- **WHEN** Home Manager activates
- **THEN** only the enabled targets receive skills

#### Scenario: Version bump replaces skills atomically

- **GIVEN** the flake input is bumped
- **WHEN** Home Manager activates
- **THEN** previously installed skills are replaced atomically with no
  half-installed state

### Requirement: Offline read operations with graceful degradation

The system MUST work offline for read operations that do not require Nix
evaluation, and MUST degrade gracefully (reporting the cause in the response
envelope) when evaluation is required but not possible.

#### Scenario: Offline outputs discovery serves cache or reports failure

- **GIVEN** the network is unavailable
- **WHEN** the agent asks for outputs
- **THEN** the command either serves cached data or reports the failure in the
  structured envelope rather than hanging

### Requirement: No secrets required at runtime

The system MUST NOT require any secret or credential at runtime for the
read-only surface.

#### Scenario: Read-only surface works without credentials

- **GIVEN** no secrets, tokens, or credentials configured
- **WHEN** the agent invokes any read-only `xi agent` subcommand
- **THEN** the command completes without requesting or requiring credentials

### Requirement: No implicit writes to project files

The system MUST NOT modify the developer's project files (`.gitignore`,
`flake.nix`, `.xi.toml`) implicitly; any write is explicit and reversible.

#### Scenario: Read subcommands never modify project files

- **WHEN** any `xi agent` read subcommand runs
- **THEN** it does not modify `.gitignore`, `flake.nix`, `.xi.toml`, or any
  other project file

#### Scenario: Explicit writes are reversible

- **WHEN** a subcommand that writes to project files is explicitly invoked
- **THEN** the change is explicit (opted into by the user) and reversible

## Success Criteria

- **SC-001**: For a project with a warmed devshell daemon, an agent can obtain
  the full context (outputs, devshell state, validation plan) in under 500 ms in
  the common case, so it fits inside a single agent turn without a visible
  pause.
- **SC-002**: On a fresh clone, the first cold call returns useful (possibly
  partial) context in under 5 s, degrading gracefully rather than blocking
  indefinitely.
- **SC-003**: An agent equipped with the skills, on a repo it has never seen,
  correctly identifies at least the packages, devShells, checks, and validation
  plan on its first turn — measured across at least three real projects.
- **SC-004**: A validation-plan execution reports a definitive per-step
  pass/fail for every configured check; no step is silently skipped.
- **SC-005**: Enabling `programs.xi.agents.enable = true` and rebuilding
  installs skills to all opted-in agent runtimes in one Home Manager activation
  cycle, with no manual follow-up.
- **SC-006**: Toggling `programs.xi.agents.enable = false` and rebuilding
  removes every previously installed skill file with no orphan artefacts.
- **SC-007**: Response schemas are documented once and the same shape is served
  to every agent runtime (no per-runtime forks), verified by a schema
  conformance test.

## Assumptions

- The developer is on a Linux or macOS machine with Nix installed and `xi` on
  PATH. Windows is out of scope.
- The primary agent runtimes are Claude Code and Codex; other
  Agent-Skills-compatible runtimes (Cursor, Gemini CLI, OpenCode) benefit
  because they read the same `SKILL.md` format, but explicit wiring is not
  committed to for v1.
- The developer uses Home Manager for their user environment. Non-HM install
  (`xi agent install`) is a required alternative but not the primary path.
- The Agent Skills convention as published at agentskills.io in late 2025
  remains the stable format for skill directories throughout v1's lifetime.
- The existing `xi develop` daemon and its JSON socket protocol continue to be
  the source of truth for devshell state. This feature reads from it; it does
  not reshape the daemon.
- MCP (Model Context Protocol) support is explicitly out of scope for v1 and
  will be considered in a follow-up feature.
