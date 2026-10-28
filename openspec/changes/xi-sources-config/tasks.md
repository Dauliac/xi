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

## 5. Switch entrypoint + CLI topology (P1)

See `design.md` for why this replaces the former `xi sources init`.

- [x] 5.1 Mount `sources` under each scope verb
      (`xi {nixos,darwin,system,home} sources`); drop the top-level `xi sources`
      group, the `--scope` flag and the `[sources] scope` key. Scope comes from
      the parent verb.
- [x] 5.2 Add the optional `[<remote-url>]` positional to `xi <scope> switch`,
      with the shape-based discrimination rule (scp-style and bare
      https/http/ssh are remotes; everything xi already parses as an installable
      stays one) plus `--adopt` / `--no-adopt` overrides.
- [x] 5.3 Adopt on switch: create the dirs, clone or fast-forward, record the
      remote as this scope's source. Idempotent for the same remote; refuse a
      different remote without `--force`.
- [x] 5.4 Bare `xi <scope> switch` resolves the recorded source and builds from
      the sources dir.
- [x] 5.5 Installer pre-check wired into every switch, ahead of any evaluation.
      No-op and no network when `nix` is already usable. Owned by
      [[xi-sources-bootstrap]]; this task is the call site.

## 6. Tests (P1)

- [x] 6.1 BDD scenario per precedence level.
- [x] 6.2 Unit: XDG fallback paths when `XDG_DATA_HOME` / `XDG_STATE_HOME` /
      `XDG_CONFIG_HOME` are unset.
- [x] 6.3 Unit: the discrimination rule, one case per row of the design.md
      table.
- [x] 6.4 BDD: adopt on a fresh machine, re-adopt the same remote, refuse a
      different remote without `--force`.
- [x] 6.5 BDD: `xi home sources status` reports user scope and the three system
      verbs report system; `--scope` is rejected.

## 7. Follow-ups (P4)

- [ ] 7.1 Interactive remote-picker TUI for a bare `xi <scope> switch` on an
      unconfigured machine. Deferred.
- [ ] 7.2 Config-file includes (`.xi.toml` sourcing a shared snippet). Deferred.
