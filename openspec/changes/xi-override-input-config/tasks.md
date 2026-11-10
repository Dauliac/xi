# Tasks: xi-override-input-config

Live state in beads under `change:xi-override-input-config`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-override-input-config/spec.md`, `design.md` and
      `proposal.md` (this change).

## 2. Config plumbing (P1)

- [ ] 2.1 Add `override_input: BTreeMap<String, String>` to `BuildConfig`
      (`crates/xi/src/config.rs:55`), defaulting to empty. Accept the TOML
      inline-table form documented in `design.md`.
- [ ] 2.2 Add a `[build]` section to `ProjectConfig`
      (`crates/xi-flake/src/project_config.rs:166`), which has no such section
      today, carrying the same `override_input` key.
- [ ] 2.3 Teach the project loader about `.xi.local.toml` alongside the existing
      `.xi.toml` (`:168`), with `.xi.local.toml` ranking higher.
- [ ] 2.4 Record, for each loaded value, which source it came from and that
      source's base directory. The normalizer in section 4 cannot be correct
      without this, so it must not be bolted on later.
- [ ] 2.5 Reject a relative path value read from `~/.config/xi/config.toml` with
      the error wording from the spec.

## 3. Environment variable (P1)

- [ ] 3.1 Parse `XI_OVERRIDE_INPUT` as `name=flakeref` pairs split on any run of
      whitespace, including newlines.
- [ ] 3.2 Split each pair on the first `=` only, so flakeref query strings
      (`?ref=main&rev=abc`) survive.
- [ ] 3.3 Error, naming the offending token, on a pair with no `=` or an empty
      input name. Silently dropping it would hide a typo that changes what gets
      built.

## 4. Path normalization (P1)

- [ ] 4.1 Implement the path discriminator: resolve only values starting with
      `path:`, `/`, `./`, `../`, `~`; forward everything else verbatim.
- [ ] 4.2 Implement the five-step pipeline in order: tilde expansion, join with
      the per-source base directory, lexical clean, percent-encode, `path:`
      prefix. Use `path-clean` (or an equivalent twenty lines), NOT
      `fs::canonicalize()`, per `design.md`.
- [ ] 4.3 Strip and re-apply the scheme for `path:../x` style values so they are
      resolved too.
- [ ] 4.4 Validate the resolved path is a directory containing `flake.nix`, and
      on failure report both the value as written and the resolved absolute
      path.
- [ ] 4.5 Unit tests for the space-containing path case specifically: a raw
      space makes nix stop parsing `path:` as a URL, so this is a correctness
      case and not an edge case.

## 5. Merge, filter, emit (P1)

- [ ] 5.1 Add
      `apply_override_input_defaults(&mut self, &BTreeMap<String, String>)` next
      to `apply_build_defaults` (`crates/xi-core/src/args.rs:396`). Do not make
      the existing `pub const fn` non-const; a map merge cannot happen in a
      const fn.
- [ ] 5.2 Resolve the cascade per input name in the documented order and flatten
      to the existing flat pair `Vec<String>` the field at `:295` uses, so the
      single emission site at `:382` and its existing
      `override_input_pairs_are_emitted` test keep covering the output.
- [ ] 5.3 Filter overrides against `nodes.root.inputs` from `flake.lock`, by
      plain JSON read with no nix evaluation. Check a transitive `a/b` key
      against the parent name `a` only.
- [ ] 5.4 Gate injection on command kind (`crates/nix-command/src/lib.rs:33`):
      never for `Store`, `Copy`, `PathInfo`, `Config`.
- [ ] 5.5 Suppress injection for lock-writing flake subcommands (`update`,
      `lock`) and report the suppression. `CommandKind::Flake` is too coarse, so
      inspect the flake subcommand.
- [ ] 5.6 Emit one report line per applied override showing the resolved
      absolute path, and mark dirty checkouts if that is cheap to detect.

## 6. Tests (P1)

- [ ] 6.1 Assert the generated argv is unchanged when nothing is configured (the
      inert-by-default criterion, SC-012).
- [ ] 6.2 Assert `flake.lock` is byte-identical after a command that applied an
      override.
- [ ] 6.3 Per-source base-directory tests: file value against the file's
      directory, env value against the flake root, CLI value against the process
      CWD.
- [ ] 6.4 Precedence test: config sets two inputs, a flag replaces one, the
      other survives.
- [ ] 6.5 Filter test: an override naming an absent input produces no flag.
- [ ] 6.6 Suppression test: `flake update` injects nothing and still writes the
      lock.
- [ ] 6.7 Integration check against this repo's own `nix-lib` input, asserting
      the input locks as `{"type":"path"}` and not `{"type":"git"}`.
- [ ] 6.8 Add Gherkin scenarios under `tests/features/` per this repo's
      spec-of-truth convention, and update that directory's README index.

## 7. Documentation (P2)

- [ ] 7.1 Fill the env column for `--override-input` at
      `docs/reference/cli.md:537`, which currently holds an empty placeholder.
- [ ] 7.2 Add `override_input` to the `[build]` table at
      `docs/reference/configuration.md:32`.
- [ ] 7.3 Add the row to the mapping table at `docs/explanation/xi-spec.md:61`,
      which is the contract this change exists to honour.
- [ ] 7.4 Document the three nix behaviours users will otherwise hit on their
      own: bare paths silently become `git+file:` and fail on untracked files,
      relative overrides are CWD-relative in raw nix, and spaces must be
      percent-encoded.
- [ ] 7.5 Add a short multi-flake local development guide showing the one-line
      global config form and the `.envrc` env form.

## 8. Repo hygiene (P2)

- [ ] 8.1 Add `.xi.local.toml` to `.gitignore`.
- [ ] 8.2 Add an `xi doctor` check that warns when `.xi.local.toml` is tracked
      by git, since committing machine paths breaks every other machine and CI.
- [ ] 8.3 Consider surfacing active overrides in `xi doctor` output
      (`crates/xi-flake/src/doctor.rs`), which is where the dropped `xi link`
      subcommand's status role belongs.

## 9. Follow-ups filed, not fixed here (P3)

- [ ] 9.1 File a bead for automatic path derivation from the input URL plus a
      clone root (the ghq-layout idea), explicitly rejected in `design.md` for
      this change. Only pick it up if writing paths by hand proves to be the
      real pain, and then as a separate explicit key rather than an overloaded
      value type.
- [ ] 9.2 File a bead for the Nix modules surface: whether the nixos,
      home-manager and flake-parts modules should be able to write
      `build.override_input` into a generated config, and whether that is ever
      desirable given the value is machine state.
