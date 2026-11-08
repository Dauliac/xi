# Tasks: xi-flake-doctor-checks

Live state in beads under `change:xi-flake-doctor-checks`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-flake-doctor-checks/spec.md`.
- [ ] 1.2 Cross-link with `xi-binary-cache` (`xi-binary-cache-doctor`),
      the source of the substituter/trusted-key pairing logic this change
      reuses at the flake-config level.

## 2. Structural checks (P1)

- [ ] 2.1 `check_flake_description`: parse `flake.nix` (or
      `nix flake metadata --json`'s `description` field) for a top-level
      `description`; `Warn` when absent.
- [ ] 2.2 `check_lock_present_and_tracked`: `flake.lock` missing is already
      a hard `bail!` (`doctor.rs:337-340`)  -  unchanged. New: check the
      path against the repo's `.gitignore` (reuse whatever glob-match
      helper already exists in the codebase for `.gitignore`-style
      patterns, or `ignore`/`gitignore`-crate equivalent if none does);
      `Fail` when matched.
- [ ] 2.3 `check_floating_input_refs`: for each `FlakeInput`, inspect the
      `original.ref` field already parsed by `parse_flake_lock`
      (`doctor.rs:106-184`)  -  currently discarded after extracting
      `git_ref`; extend `FlakeInput` to also carry whether `original` had
      an explicit `rev`/`ref` of tag-shape vs. branch-shape. `Warn` when
      floating.
- [ ] 2.4 `check_duplicate_inputs`: extend `parse_flake_lock` (or a
      sibling function) to walk *all* lock nodes, not just root-level
      direct inputs, grouping by `(locked.owner, locked.repo)`; `Warn`
      when two groups for the same pair have different `locked.rev` and
      no `follows` edge connects them.
- [ ] 2.5 `check_dirty_tree`: shell `nix flake metadata --json` (via
      `nix_command::NixCommand`, same pattern as `check_formatter`),
      inspect `dirtyRevision`/`dirtyShortRev`; `Fail` when present.
- [ ] 2.6 `check_nixconfig_substituter_pairing`: static-parse
      `nixConfig.extra-substituters`/`extra-trusted-public-keys` from
      `flake.nix` (or `nix flake metadata --json`'s `fingerprint`/config
      surface, whichever actually exposes `nixConfig`  -  confirm during
      implementation); `Warn` per unpaired substituter index.

## 3. Derivation metadata checks (P2)

- [ ] 3.1 New eval helper in `crates/xi-flake/src/lib.rs`, alongside
      `discover_leaf_categories`/`discover_single_category`
      (`lib.rs:1283-1322`): one `nix eval <flake_ref>#packages.<system>
      --apply <js> --json` call returning, per package name,
      `{ description, license, maintainers, platforms, version, pname,
      mainProgram, insecure, knownVulnerabilities, binCount }`.
- [ ] 3.2 `check_package_meta_completeness`: `Warn` per package per missing
      one of `meta.description`/`meta.license`/`meta.maintainers`/
      `meta.platforms`, naming both the package and the specific
      attribute (not one combined message).
- [ ] 3.3 `check_package_version`: `Warn` when `version` is absent or
      equals the literal `"unstable"`.
- [ ] 3.4 `check_app_main_program`: cross-reference `apps.<system>.*`
      against the same eval result; `Warn` when `binCount > 1` and
      `mainProgram` is unset.
- [ ] 3.5 `check_package_insecure`: `Warn` when `insecure` or
      `knownVulnerabilities` is set on a package reachable from
      default-visible outputs.
- [ ] 3.6 `[doctor]` config gains a boolean to disable this whole group
      (see task 5.1); document the eval cost tradeoff from the
      proposal's Open Question 1 in the config's doc comment.

## 4. Output hygiene checks (P2)

- [ ] 4.1 Reuse `nix flake show --json` parsing already present in
      `ShowArgs::run` (`crates/xi-flake/src/lib.rs`)  -  extract the
      JSON-fetch+parse into a small shared helper if it isn't already
      one, rather than duplicating the `nix flake show` invocation.
- [ ] 4.2 `check_per_system_output_gaps`: for `packages`/`apps`/
      `devShells`/`checks`, diff the leaf-key set across every system
      listed in the flake's own `systems` (or inferred from the union of
      systems actually present); `Warn` per missing `(key, system)` pair.
- [ ] 4.3 `check_checks_output_present`: `Warn` when `checks` is absent
      for every system.
- [ ] 4.4 `check_nonstandard_output_keys`: diff top-level keys from
      `nix flake show --json` against `FlakeOutput::from_nix_name`
      (`crates/xi-core/src/flake_output.rs`); `Warn`, worded
      informationally, for every key that returns `None`. Do NOT consult
      any `schemas`/`exportedSchemas` output  -  explicitly deferred per
      proposal Open Question 3.

## 5. Config and severity (P1)

- [ ] 5.1 `ProjectDoctorConfig` (`crates/xi-flake/src/project_config.rs:
      73-80`) gains per-group opt-outs (e.g. `disable_structure_checks`,
      `disable_metadata_checks`, `disable_output_checks`, or a single
      `disabled_checks: Vec<String>` naming check names  -  decide shape
      during implementation, following whichever existing boolean-flag
      vs. list convention reads more consistently against
      `require_official_nixpkgs`/`supported_branches`).
- [ ] 5.2 `run_health_checks` (`doctor.rs:330-381`) respects the new
      opt-outs, skipping disabled groups entirely rather than running and
      discarding results.
- [ ] 5.3 Confirm `CheckStatus::Fail` usage is limited to exactly the two
      cases named in the proposal (missing/gitignored `flake.lock`
      beyond the existing hard-`bail!`, and dirty tree); every other new
      check uses `Warn`.

## 6. Tests (P1)

- [ ] 6.1 Structural checks: table-driven tests per check using
      synthetic `flake.lock`/`flake.nix` fixtures, following the existing
      `make_nixpkgs_input`/`parse_lock_file_realistic` test patterns in
      `doctor.rs`'s `#[cfg(test)] mod tests`.
- [ ] 6.2 Duplicate-input detection: a fixture lock with two divergent
      `nixpkgs` nodes, with and without a `follows` edge, asserting the
      warning appears only in the unfollowed case.
- [ ] 6.3 Metadata checks: mock or fixture the eval helper's JSON output
      (avoid shelling real `nix eval` in unit tests); assert per-attribute
      warnings are independent (missing only `license` warns once, not
      once per all four attributes).
- [ ] 6.4 Output hygiene: fixture `nix flake show --json` output with an
      intentional per-system gap and a non-standard key; assert both are
      flagged and worded as the spec describes (informational wording
      for the non-standard-key case).
- [ ] 6.5 Config opt-outs: assert disabling one group suppresses exactly
      that group's warnings and no others, reusing the proposal's
      SC-FD-013 scenario directly as the test case.
- [ ] 6.6 Full workspace `cargo test` green, zero new clippy warnings,
      matching the bar already met by `xi-binary-cache`.
