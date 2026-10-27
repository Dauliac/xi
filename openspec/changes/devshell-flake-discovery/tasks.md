# Tasks: devshell-flake-discovery

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads.

## 1. Specification (P1)

- [ ] 1.1 Land the spec delta under `specs/xi-devshell-flake-discovery/spec.md`.
- [ ] 1.2 Cross-link with `xi-devshell-trust` and `xi-devshell-daemon` — both
      consume `flake_id` derived from the discovered canonical root.

## 2. Canonicalisation + exclusion filter (P1)

- [ ] 2.1 Canonicalise the start path in `find_flake_stack` before the walk;
      document the fallback if canonicalisation fails (return empty stack, do
      not silently walk the un-canonicalised path).
- [ ] 2.2 Add an exclusion filter for `.git/`, `node_modules/`, `target/` — an
      ancestor whose basename matches MUST NOT be recorded as a flake root even
      if it contains `flake.nix`.

## 3. Tests (P1)

- [ ] 3.1 Unit test `discovery_canonical_symlink`: start from a symlinked alias
      of a flake root, assert the returned path is the resolved target and
      matches the alias's canonicalisation.
- [ ] 3.2 Unit test `discovery_nested_precedence`: nested flakes; assert
      `find_flake_root` returns innermost, `find_flake_stack` returns
      outermost-first.
- [ ] 3.3 Unit test `discovery_excluded_dirs`: place a `flake.nix` inside each
      of `.git/`, `node_modules/`, `target/` and assert none is picked up.
- [ ] 3.4 Unit test `discovery_no_flake_returns_none`: CWD with no ancestor
      `flake.nix` returns `None` / empty stack; no fabricated fallback.
- [ ] 3.5 Unit test `discovery_stat_budget`: instrument the walk to count `stat`
      calls; assert exactly `depth` calls for a CWD `depth` levels below a flake
      root.

## 4. Follow-ups (P4 — deferred to future changes)

- [ ] 4.1 User-configurable exclusion list (needs a config surface proposal —
      deferred).
- [ ] 4.2 `.envrc`/direnv integration — separate proposal establishing the
      handshake with direnv's env dump.
- [ ] 4.3 Per-stat timeout for network-backed filesystems (deferred until a real
      user reports a stuck prompt from an NFS/CIFS walk).
