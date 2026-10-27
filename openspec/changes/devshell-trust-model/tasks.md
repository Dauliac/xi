# Tasks: devshell-trust-model

This is a task snapshot. Live execution state (claims, in-progress, closed)
lives in beads.

## 1. Specification (P1)

- [ ] 1.1 Land the spec delta under `specs/xi-devshell-trust/spec.md` (this
      proposal).
- [ ] 1.2 Cross-link with `xi-devshell-daemon` (`async-daemon-cqrs`): the "no
      interactive stderr" client requirement must reference this spec as the
      sole documented exception.

## 2. Hook refusal guard (P1)

- [ ] 2.1 Add an untrusted-flake guard at the top of `hook::tick` that returns
      early (no daemon connect, no eval) when `trust::is_trusted` is false.
- [ ] 2.2 Emit exactly one throttled interactive-stderr prompt per shell
      lifetime per flake id when untrusted; throttle key persisted in the shell
      environment (`__XI_TRUST_PROMPT_SHOWN_<flake_id>=1`).
- [ ] 2.3 BDD `bdd_trust_hook_refusal`: walking into an untrusted flake with the
      hook installed produces zero daemon connections and one stderr prompt
      across N ticks.

## 3. Marker filesystem contract (P1)

- [ ] 3.1 Enforce `0700` on `dirs::trust_dir()` at first write; enforce `0600`
      on the marker file itself.
- [ ] 3.2 BDD `bdd_trust_marker_perms`: after `xi develop trust <path>`, assert
      `stat(trust_dir()) & 0o777 == 0o700` and
      `stat(marker) & 0o777
      == 0o600`.
- [ ] 3.3 BDD `bdd_trust_edit_survives`: trust a flake, edit `flake.nix`, re-run
      prompt — activation proceeds without re-prompting.
- [ ] 3.4 BDD `bdd_trust_move_invalidates`: trust a flake, `mv` it to a new
      path, re-run prompt in the new location — behaves as untrusted.

## 4. CLI surface (P2)

- [ ] 4.1 Add `xi develop trust --list` subcommand emitting one line per marker:
      `<flake_id>\t<canonical_path_or_missing>`.
- [ ] 4.2 BDD `bdd_trust_list`: after two trust grants and one manual marker
      removal, `--list` returns two entries in a stable order (sorted by id).

## 5. Follow-ups (P4 — deferred to future changes)

- [ ] 5.1 Scoped trust (per-target grants) — deferred until a real consumer
      requires it.
- [ ] 5.2 Signed/shareable trust markers for multi-user hosts.
