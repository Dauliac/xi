# xi Codebase Map for xi-sources Feature

This document maps the xi codebase for the `xi sources` feature, a GitOps daemon
managing remote flake sources with state sharing across switch commands and
xi-deploy.

## 1. Entry Points for `switch` Commands

### xi system switch

**Path:** `crates/xi/src/system/mod.rs:50-94`

```rust
pub fn rebuild(self, variant: &SystemRebuildVariant, elevation: ElevationStrategy) -> Result<()> {
  let installable = self.common.installable
    .clone()
    .resolve_or_default(CommandContext::System)?;
  let toplevel = resolve_toplevel(
    &hostname,
    installable,
    xi_core::flake_output::FlakeOutput::SystemConfigs,
    &[],
  )?;
}
```

**Key:** Flake resolution via `InstallableArgs` (env vars: `XI_SYSTEM_FLAKE`,
`XI_FLAKE`) → the new sources resolver hooks at `resolve_or_default()`.

### xi os switch (NixOS)

**Path:** `crates/xi-nixos/src/nixos.rs:82-106`

```rust
pub fn run(self, elevation: ElevationStrategy) -> Result<()> {
  match self.subcommand {
    OsSubcommand::Switch(args) => {
      args.rebuild_and_activate(&Switch, None, elevation)
    },
  }
}
```

**Key:** Calls `resolve_installable_and_toplevel()` at line 176; flake
resolution follows the same env-var priority: `XI_OS_FLAKE` → `XI_FILE` →
`XI_FLAKE`.

### xi home switch

**Path:** `crates/xi-home/src/args.rs` (via HomeArgs trait bounds) **Key:**
Similar pattern; uses `XI_HOME_FLAKE` env-var chain and `resolve_or_default()`.

### xi darwin switch

**Path:** `crates/xi-darwin/src/args.rs` (via DarwinArgs trait bounds) **Key:**
Similar pattern; uses `XI_DARWIN_FLAKE` env-var chain and
`resolve_or_default()`.

### Flake Resolution Entry Point

**Path:** `crates/xi-core/src/installable/mod.rs:347-390`

```rust
impl InstallableArgs {
  pub fn uses_flakes(&self, context: CommandContext) -> bool { ... }
  fn resolve(self, context: CommandContext) -> Result<Option<Installable>> {
    env_installable_source(context).map(...).transpose()
  }
  pub fn resolve_or_default(
    self, context: CommandContext
  ) -> Result<Installable> { ... }
}
```

**Why it matters:** This is the single point where flake references are parsed.
The sources feature must insert a resolver middleware here before `Installable`
is evaluated into a store path.

---

## 2. xi-deploy Crate Overview

### DeployBackend Trait

**Path:** `crates/xi-deploy/src/backend/mod.rs:14-37`

```rust
pub trait DeployBackend {
  fn name(&self) -> &'static str;
  fn discover_targets(&self, flake_ref: &str) -> Result<Vec<DeployTarget>>;
  fn deploy(&self, flake_ref: &str, args: &DeployArgs) -> Result<()>;
}
```

**Why it matters:** The sources feature can add a fourth backend (`sources`)
alongside `deploy_rs`, `colmena`, and `builtin`.

### Backend Detection

**Path:** `crates/xi-deploy/src/lib.rs:31-81`

```rust
pub fn detect_and_deploy(args: &DeployArgs) -> Result<()> {
  let backend: Box<dyn DeployBackend> = if let Some(ref name) = args.backend {
    resolve_explicit_backend(name, flake_ref)?
  } else {
    detect_backend(flake_ref)
  };
}

fn detect_backend(flake_ref: &str) -> Box<dyn DeployBackend> {
  // 1. deploy-rs: check for `deploy` output
  // 2. colmena: check for `colmenaHive` output
  // 3. builtin: always available as fallback
}
```

**Why it matters:** Sources outputs (`xi.sources.*`) must be detected at line
68-81 before falling back to builtin.

### Backend Modules

- **deploy-rs:** `crates/xi-deploy/src/backend/deploy_rs.rs` - extracts
  `deploy.nodes.<n>.profiles.<p>`
- **colmena:** `crates/xi-deploy/src/backend/colmena.rs` - extracts
  `colmenaHive` output
- **builtin:** `crates/xi-deploy/src/backend/builtin.rs` - remote rebuild with
  SSH

---

## 3. xi-develop Daemon (IPC & State)

### Daemon Lifecycle

**Path:** `crates/xi-develop/src/lib.rs:71-101`

```rust
fn run_daemon(args: &args::DaemonArgs) -> Result<()> {
  let fid = dirs::flake_id(&canonical);
  let state_dir = dirs::state_dir(&fid);
  let socket_path = dirs::daemon_socket_path(&fid);
  
  let state = std::sync::Arc::new(daemon::server::ServerState::new(...));
  daemon::server::run(&socket_path, &state)
}
```

### Directory Structure

**Path:** `crates/xi-develop/src/dirs.rs:39-111`

```rust
// Per-flake state directory: $XDG_STATE_HOME/xi/develop/{flake_id}/
pub fn state_dir(fid: &str) -> PathBuf { state_base().join(fid) }

// Daemon socket: /tmp/xi-{uid}/{flake_id}/daemon.sock
pub fn daemon_socket_path(flake_id: &str) -> PathBuf {
  daemon_runtime_dir(flake_id).join("daemon.sock")
}

// Trust directory: $XDG_CONFIG_HOME/xi/develop/trusted/
pub fn trust_dir() -> PathBuf { ... }
```

**Why it matters:** The sources daemon can reuse this socket hierarchy
(`/tmp/xi-{uid}/{flake_id}/sources.sock`) and state layout. A second daemon is
cleaner than sharing one.

### State Management

**Path:** `crates/xi-develop/src/dirs.rs:176-209`

```rust
pub fn write_and_switch(
  state_dir: &Path,
  prefix: &str,
  ext: &str,
  content: &str,
) -> Result<()> {
  // A/B slot pattern for atomic file updates
  let active = active_slot(state_dir, prefix, ext).unwrap_or(Slot::A);
  let target_slot = active.other();
  // Write to inactive, then atomically repoint symlink
}
```

**Why it matters:** Sources state (clone metadata, fetch timestamps) can use
this A/B slot pattern for atomic updates.

### Configuration Reading

**Path:** `crates/xi-develop/src/args.rs` defines `DaemonArgs` with
`--eval-cache`, `--eval-interval`, `--watch-extra`. **Why it matters:** Sources
daemon can extend with `--watch-sources`, `--sources-config-path`.

---

## 4. Config Loading (`.xi.toml` Schema)

### ProjectConfig Definition

**Path:** `crates/xi-flake/src/project_config.rs:6-14`

```rust
pub struct ProjectConfig {
  pub ci:          ProjectCiConfig,
  pub consumer:    ProjectConsumerConfig,
  pub doctor:      ProjectDoctorConfig,
  pub test:        ProjectTestConfig,
  pub fmt:         ProjectFmtConfig,
  pub materialize: ProjectMaterializeConfig,
}
```

### Loading Function

**Path:** `crates/xi-flake/src/project_config.rs:164-385`

```rust
pub fn load_project_config(flake_dir: Option<&Path>) -> ProjectConfig {
  let config_path = dir.join(".xi.toml");
  let doc = std::fs::read_to_string(&config_path)?
    .parse::<toml_edit::DocumentMut>()?;
  
  // [ci], [doctor], [fmt], [test], [consumer], [materialize] sections
  config
}
```

### Env-Var & CLI Precedence

**Path:** `crates/xi/src/interface.rs:176-192`

```rust
pub fn apply_cache_config(&mut self, cache_config: &CacheConfig) {
  // CLI > env > config.toml defaults
}
```

### New `[sources]` Section Schema (to be added)

```toml
[sources]
# GitOps sync strategy: "on-demand", "auto", or "manual"
strategy = "on-demand"

# Cache directory for cloned remote repos
cache-dir = ".xi/sources"

# Per-source configuration
[[sources.remote]]
name = "nixpkgs-unstable"
url = "github:nixos/nixpkgs/nixos-unstable"
# How to resolve: "git-fetch", "flake-registry", or "local-path"
resolver = "flake-registry"
# Trust model: "unsigned", "signed-commit", "signed-tag"
trust = "unsigned"
```

**Why it matters:** New sources config plugs into the same toml_edit
infrastructure at line 172-385.

---

## 5. Nix Modules Overview

### Module Tree

```
modules/
├── flake-parts/
│   ├── package.nix          -  Exposes xi package & helpers
│   ├── agents.nix           -  Flake output for xi agents
│   ├── devshell.nix         -  Devshell integration
│   ├── fmt.nix              -  Formatter backend registration
│   ├── test.nix             -  Test framework discovery
│   ├── settings.nix         -  xi option tree (options.xi.*)
│   └── completion.nix       -  Bash/Zsh completion setup
├── nixos/
│   ├── package.nix          -  NixOS service/package
│   ├── agents.nix           -  systemd services for agents
│   ├── settings.nix         -  options.xi.* tree
│   └── [others same pattern]
└── home-manager/
    ├── package.nix          -  Home-manager activation
    ├── agents.nix           -  User systemd services
    ├── settings.nix         -  options.xi.* tree
    └── [others same pattern]
```

### Module Signatures (to find where xi.* option tree lives)

**Path:** `modules/nixos/settings.nix` (and home-manager/settings.nix,
flake-parts/settings.nix) Each exposes `options.xi.*` - the sources feature adds
`options.xi.sources.* = { ... }`

**Why it matters:** The new `xi.sources.*` option must be defined in all three
modules to ensure NixOS, home-manager, and flake-parts users can configure it.

---

## 6. FlakeOutput Taxonomy (xi-core)

### Enum Definition

**Path:** `crates/xi-core/src/flake_output.rs:24-48`

```rust
pub enum FlakeOutput {
  Packages,
  DevShells,
  Checks,
  Apps,
  Formatter,
  LegacyPackages,
  Overlays,
  NixosModules,
  NixosConfigurations,
  HomeConfigurations,
  HomeModules,
  DarwinModules,
  DarwinConfigurations,
  SystemConfigs,
  Templates,
  Lib,
  // -- Deployment tool outputs --
  Deploy,              // deploy-rs: deploy.nodes.<n>.profiles.<p>
  ColmenaHive,         // Colmena: colmenaHive
  DiskoConfigurations, // disko: declarative disk partitioning
}
```

### String Mapping

**Path:** `crates/xi-core/src/flake_output.rs:76-98`

```rust
pub const fn as_str(self) -> &'static str {
  match self {
    Self::Deploy => "deploy",
    Self::ColmenaHive => "colmenaHive",
    Self::DiskoConfigurations => "diskoConfigurations",
    // ...
  }
}
```

### New Variant (to be added)

```rust
// In enum definition (after DiskoConfigurations):
Sources,             // xi-sources: per-flake remote registry

// In as_str() match:
Self::Sources => "sources",
```

**Why it matters:** This centralizes flake output metadata. New variants for
sources (e.g., `SourcesRegistry`, `SourcesSnapshots`) belong here.

---

## 7. Response Envelope (Agent Schema)

### Envelope Definition

**Path:** `crates/xi-agent/src/schema.rs:15-26`

```rust
pub struct Envelope<T> {
  pub schema:      String,        // "xi.agent/v1"
  pub command:     String,        // Command name
  pub generated:   DateTime<Utc>,
  pub duration_ms: u64,
  pub data:        Option<T>,
  pub errors:      Vec<Diagnostic>,
  pub warnings:    Vec<Diagnostic>,
}

pub struct Diagnostic {
  pub code:    String,
  pub message: String,
  pub source:  Option<String>,
  pub hint:    Option<String>,
}
```

**Schema Version:** `SCHEMA_V1 = "xi.agent/v1"` at line 13.

### Constructors

```rust
Envelope::ok(command, data, duration_ms)           // data: Some(T), no errors
Envelope::partial(command, data, duration_ms, errors)  // data: Some(T) + errors
Envelope::fail(command, error, duration_ms)        // data: None + error
```

**Why it matters:** Sources commands (`xi sources list`, `xi sources push`,
`xi sources status`) must emit `Envelope<SourcesResponse>` with this schema.

### Example SourcesResponse (to define)

```rust
#[derive(Serialize, Deserialize)]
pub struct SourcesResponse {
  pub status: Status,  // Ready|Pending|Missing|Degraded
  pub sources: Vec<SourceStatus>,
}

pub enum Status {
  Ready,
  Pending,
  Missing,
  Degraded,
}
```

---

## 8. Trust Model (Devshell Pattern to Mirror)

### Specification

**Path:**
`openspec/changes/devshell-trust-model/specs/xi-devshell-trust/spec.md:1-143`

### Key Principles (Mirror for Sources)

1. **Refuse-by-default** - Untrusted remote repos do not sync without explicit
   trust grant
2. **Single throttled prompt** - User prompted once per flake per shell for
   `xi sources trust <repo>`
3. **Explicit grant only** - `xi sources trust` creates a single marker file
   under `$XDG_CONFIG_HOME/xi/sources/trusted/`
4. **Per-user & non-shareable** - Trust directory mode `0700`, marker files mode
   `0600`
5. **Survives source edits** - Trust bound to flake identity (canonical path
   hash), not content
6. **Canonical identity** - Trust keyed by `sha256(canonical_flake_path)` (see
   `xi-develop/src/dirs.rs:18-23`)
7. **Revocation is immediate** - `xi sources untrust <repo>` removes marker;
   next evaluation sees it as untrusted

### Mapping to Sources

| Devshell Concept                            | Sources Equivalent                                |
| ------------------------------------------- | ------------------------------------------------- |
| Untrusted flake → no daemon spawn           | Untrusted remote → no fetch/sync                  |
| `xi develop trust <path>`                   | `xi sources trust <repo>`                         |
| `$XDG_CONFIG_HOME/xi/develop/trusted/{fid}` | `$XDG_CONFIG_HOME/xi/sources/trusted/{repo-hash}` |
| Trust survives flake.nix edit               | Trust survives `sources.toml` edit                |

---

## 9. Existing openspec Changes (Conflict Avoidance)

| Change Directory                 | Purpose                                                 | Status  |
| -------------------------------- | ------------------------------------------------------- | ------- |
| `ci-nix-publish/`                | Publish built derivations to binary cache               | active  |
| `devshell-activation-contract/`  | Refined activation semantics for shells                 | active  |
| `devshell-flake-discovery/`      | Auto-discover flake.nix via parent walk                 | active  |
| `devshell-trust-model/`          | Trust markers for untrusted flakes                      | active  |
| `devshell-watcher-scope/`        | Which files trigger daemon re-eval                      | active  |
| `nixos-wrap-nix-nh-compat/`      | Wrap nix-nh for NixOS deployment                        | active  |
| **`xi-sources-bootstrap/`**      | **Entry point & bootstrap logic for `xi sources init`** | **NEW** |
| **`xi-sources-cache-prestage/`** | **Cache pre-staging before deployments**                | **NEW** |
| **`xi-sources-config/`**         | **`.xi.toml` [sources] section schema**                 | **NEW** |
| **`xi-sources-conflict/`**       | **Conflict resolution strategy**                        | **NEW** |
| **`xi-sources-gitops/`**         | **GitOps watch daemon**                                 | **NEW** |
| **`xi-sources-model/`**          | **Data model & state schema**                           | **NEW** |
| **`xi-sources-push/`**           | **`xi sources push` command**                           | **NEW** |

**Why it matters:** No conflict with existing changes. Sources changes are
isolated under `xi-sources-*` prefix.

---

## 10. Current Flake URL Resolution

### Can `xi nixos switch` Take a `github:owner/repo` URL Today?

**YES - Confirmed at multiple layers:**

1. **CLI Parsing:** `crates/xi-core/src/installable/mod.rs:301-321` accepts any
   Nix flake reference string including `github:owner/repo#attribute`.
2. **Env-Var Precedence:**
   `XI_OS_FLAKE=github:nixos/nixpkgs/nixos-unstable xi os switch` works today.
3. **Flake Resolution:**
   `nix build github:owner/repo#nixosConfigurations.myhostname.config.system.build.toplevel`
   is a standard Nix operation.

**Evidence:**

```rust
// installable/mod.rs:167-176
if let Some(i) = installable {
  let (reference, attribute) = parse_flake_reference(i)? // Accepts github:owner/repo
  return Ok(Self::Specified(Installable::Flake {
    reference,
    attribute,
  }));
}
```

**Implication for Sources Feature:**

- **NOT a breaking change** - adding sources management wraps the existing URL
  resolution
- **Sources resolver acts as middleware** - between flake reference string and
  `nix build` invocation
- **Insertion point:** `crates/xi-core/src/installable/mod.rs:379-391` in
  `resolve()` or `resolve_or_default()` methods

---

## Summary: Integration Points for `xi sources`

| Component                  | File                                        | Line           | Integration                                              |
| -------------------------- | ------------------------------------------- | -------------- | -------------------------------------------------------- |
| Entry point (all switch)   | `xi-core/src/installable/mod.rs`            | 379–391        | Wrap `resolve()` with sources resolver middleware        |
| Deploy backend auto-detect | `xi-deploy/src/lib.rs`                      | 68–81          | Add sources backend detection before builtin fallback    |
| DeployBackend trait        | `xi-deploy/src/backend/mod.rs`              | 14–37          | Implement `SourcesBackend` struct                        |
| Daemon IPC/state           | `xi-develop/src/dirs.rs`                    | 39–111         | Use socket pattern for sources daemon                    |
| Config loading             | `xi-flake/src/project_config.rs`            | 164–385        | Add `[sources]` section parsing                          |
| Flake output taxonomy      | `xi-core/src/flake_output.rs`               | 24–98          | Add `Sources` variant                                    |
| Nix modules                | `modules/{flake-parts,nixos,home-manager}/` | `settings.nix` | Add `options.xi.sources.*` trees                         |
| Agent schema               | `xi-agent/src/schema.rs`                    | 15–26          | Define `SourcesResponse` struct                          |
| Trust model                | devshell pattern                            | (new)          | Implement `SourcesTrustStore` mirroring devshell pattern |
