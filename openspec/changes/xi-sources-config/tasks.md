# Tasks: xi-sources-config

Live state in beads under `change:xi-sources-config`.

## 1. Specification (P1)

- [x] 1.1 Land `specs/xi-sources-config/spec.md`.
- [x] 1.2 Cross-link with `xi-sources-model` (identity) and every downstream
      change (gitops/push/bootstrap/cache-prestage) that reads from this
      surface.

## 2. `.xi.toml [sources]` schema (P1)

- [x] 2.1 Extend `xi-flake/src/project_config.rs` with a `SourcesConfig` struct
      carrying: `remote: Option<String>`, `branch: Option<String>` (default
      `main`), `working_dir: Option<PathBuf>`,
      `poll_interval:
      Option<Duration>`, `require_signed: Option<bool>`,
      `allowed_signers: Option<PathBuf>`.
- [x] 2.2 Validate URL + path at load time; errors carry file+line.

## 3. Env surface (P1)

- [x] 3.1 `XI_SOURCES_REMOTE`, `XI_SOURCES_BRANCH`, `XI_SOURCES_WORKING_DIR`,
      `XI_SOURCES_POLL_INTERVAL`, `XI_SOURCES_STATE_DIR`,
      `XI_SOURCES_REQUIRE_SIGNED`, `XI_SOURCES_SYNC_MODE`,
      `XI_SOURCES_INSTALLER`.
- [x] 3.2 Document precedence: flag > env > toml > nix module > default.

## 4. Nix modules (P1)

- [x] 4.1 `modules/nixos/sources.nix`: option tree `xi.sources.*` with the same
      schema as the toml. Writes `/etc/xi/sources.toml` at activation.
- [x] 4.2 `modules/home-manager/sources.nix`: same, writes to
      `$XDG_CONFIG_HOME/xi/sources.toml`.
- [x] 4.3 `modules/flake-parts/sources.nix`: flake-parts facade so a flake can
      declare its own sources config declaratively.

## 5. `xi sources init` (P1)

- [x] 5.1 Positional `<remote-url>`, flags `--branch`, `--working-dir`,
      `--scope {system|user|auto}` (auto picks system when running as root, user
      otherwise).
- [x] 5.2 Writes `.xi.toml` if not present; appends `[sources]` block if present
      without one; refuses to overwrite an existing block without `--force`.
- [x] 5.3 Performs the first clone into the sources dir; emits `Ready` on
      success, `Degraded` if clone succeeded but trust requires signatures the
      HEAD lacks.

## 6. Tests (P1)

- [x] 6.1 BDD scenario per precedence level.
- [x] 6.2 Unit: XDG fallback paths when `XDG_DATA_HOME` / `XDG_STATE_HOME` /
      `XDG_CONFIG_HOME` are unset.
- [x] 6.3 Unit: `init` refuses to overwrite without `--force`.

## 7. Follow-ups (P4)

- [ ] 7.1 Interactive `xi sources init` TUI. Deferred.
- [ ] 7.2 Config-file includes (`.xi.toml` sourcing a shared snippet). Deferred.
