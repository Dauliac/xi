# Tasks: xi-sources-crash-recovery

Live state in beads under `change:xi-sources-crash-recovery`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-crash-recovery/spec.md`.
- [ ] 1.2 Cross-link with race, model, gitops, bootstrap, sync, cache-prestage.

## 2. Transaction wrapper (P1)

- [ ] 2.1 `crates/xi-sources/src/transaction.rs` with `begin(actor, intent)`,
      `commit()`, `rollback()`, and a Drop impl that calls rollback on panic.
- [ ] 2.2 Sentinel at `<state_dir>/transaction.json` carrying
      `{actor, intent, started_at, pid}`.
- [ ] 2.3 Each actor's main loop wraps its critical section in a transaction
      scope.

## 3. Shadow-write for sources dir (P1)

- [ ] 3.1 Fetch into `<sources_dir>.incoming`; use `git fetch` with an alternate
      `--git-dir`.
- [ ] 3.2 On fetch success, atomic rename `<sources_dir>` to
      `<sources_dir>.previous`, then `<sources_dir>.incoming` to
      `<sources_dir>`, then remove `.previous`.
- [ ] 3.3 On fetch failure, remove `.incoming`.

## 4. Atomic state-dir writes (P1)

- [ ] 4.1 Helper `write_atomic(path, content)` writes to `<path>.tmp.<pid>` then
      renames to `<path>` on the same filesystem.
- [ ] 4.2 All state-dir file writes route through this helper.

## 5. `xi sources repair` (P1)

- [ ] 5.1 Discovery pass: list dangling `.incoming`, dangling
      `.transaction.json` with dead pid, mismatched sources-head-vs-HEAD,
      dangling `xi.conf`, dangling sync sentinel.
- [ ] 5.2 Action pass: for each discovered item, execute the documented repair.
- [ ] 5.3 `--dry-run`: skip action pass; print the planned actions.
- [ ] 5.4 Report: single JSON envelope summarising what was done.

## 6. Daemon auto-repair on startup (P1)

- [ ] 6.1 Daemon main calls `xi sources repair` before anything else, before
      lock acquisition.
- [ ] 6.2 If repair finds destructive work (e.g. would delete incoming) log a
      loud warning.

## 7. Receipt consistency (P1)

- [ ] 7.1 Bootstrap and cache-prestage receipts are written INSIDE the
      transaction and the receipt append is the last action before commit.
- [ ] 7.2 Repair reconstructs receipt entries from on-disk evidence when the
      write-and-commit sequence was interrupted.

## 8. Tests (P1)

- [ ] 8.1 BDD: SIGKILL mid-fetch, assert `.incoming` cleaned on next startup.
- [ ] 8.2 BDD: SIGKILL between fetch and state write, assert state corrects
      itself.
- [ ] 8.3 BDD: SIGKILL mid-sync, assert working dir either resumes or rolls back
      cleanly.
- [ ] 8.4 Unit: `write_atomic` concurrent-reader never sees partial file.
- [ ] 8.5 Property: repair is idempotent.

## 9. Follow-ups (P4)

- [ ] 9.1 fsync fencing on battery-backed caches (ext4/btrfs specific).
      Deferred.
- [ ] 9.2 Pre-reboot clean-shutdown hook that runs repair. Deferred.
