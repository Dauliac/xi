# Tasks: xi-binary-cache

Live state in beads under `change:xi-binary-cache`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-binary-cache/spec.md`.
- [ ] 1.2 Cross-link with `xi-sources-cache-prestage` (related, unmodified) and
      `xi-sources-credentials` (the `Secret` type this change reuses).

## 2. Provider presets (P1)

- [ ] 2.1 `CacheProvider` enum in `xi-core::args`: `R2 { account_id }`,
      `S3 { region }`, `Generic { endpoint, region }`. Closed, kebab-case TOML
      spelling (`cloudflare-r2`, `aws-s3`, `generic`), unknown value is a
      load-time error (same posture as `Installer`/`SyncMode`).
- [ ] 2.2 `CacheTarget::resolved_s3_uri(&self) -> String`: given `provider` +
      `bucket` (+ `region` override if present), produce the
      `s3://bucket?region=...&endpoint=...` string `push_via_nix_copy` already
      consumes. `R2` always sets `region=auto`. `S3` without an explicit
      `endpoint` omits the param entirely (AWS is `s3://`'s built-in default).
- [ ] 2.3 Existing hand-written `push_url` stays valid and takes precedence over
      `provider`+`bucket` when both are present (escape hatch for anything the
      preset table doesn't yet cover).

## 3. Native credentials (P1)

- [ ] 3.1 `[cache.<name>.credentials]`: `access-key-id-file`,
      `secret-access-key-file`, parsed the same closed-key-set way
      `parse_secrets_table` validates `[sources.secrets.<name>]`.
- [ ] 3.2 `push_via_nix_copy` reads both through
      `xi_core::sources::credentials::Secret::from_file`; `NotMounted` surfaces
      as the existing queue's `Pending`-style enqueue, not a hard failure.
- [ ] 3.3 `Secret::with_secret` values are set via `Command::env(...)` on the
      single `nix copy` child process only - never exported to the calling
      process's own environment.
- [ ] 3.4 Extend `xi_sources_observability::redact::lint`'s scan (or add an
      equivalent lint for this crate) to cover the new call sites, so a future
      refactor cannot accidentally log or serialise a credential `Secret`.

## 4. Auto-derived public key (P2)

- [ ] 4.1 When `pull_public_key` is unset and `signing_key` is set, shell
      `nix key convert-secret-to-public < signing_key` once, cache the result
      next to the key file (e.g. `<signing_key>.pub.cache`), invalidate on the
      source file's mtime.
- [ ] 4.2 Explicit `pull_public_key` always wins over the derived value.

## 5. Pull via flags, not files (P1)

- [ ] 5.1 `pull_url` + resolved public key become `--option substituters <url>`
      / `--option trusted-public-keys <key>` appended to every `nix` subprocess
      `xi-core`/`xi-develop`/`xi-nixos`/ `xi-home`/`xi-darwin` build. Space-join
      with whatever the flake/ system already declares; never replace.
- [ ] 5.2 No new `nix.conf` write path. `xi-sources-cache-prestage` is untouched
      and keeps owning the bootstrap-time file write.

## 6. Git-remote / flake match (P1)

- [ ] 6.1 New normaliser (`xi-core::sources::remote_host_path` or similar) built
      on `validate_remote_url`'s existing scheme table: given any accepted
      remote URL form (`https://`, `ssh://`, scp-style, `file://`/absolute
      path), return `(host, path)` or `None` for `file://`/local.
- [ ] 6.2 Resolution ladder: (a) an explicit flake-ref argument on the invoking
      xi subcommand, parsed the same way, outranks (b) the git remote of
      `xi-develop::flake_detect::find_flake_root`'s result for the current CWD.
- [ ] 6.3 `[cache.<name>].match`: list of glob patterns against `host/path`.
      Most-specific match wins (longest literal prefix before the first `*`). No
      match and no `default = true` target: no push, warning printed naming the
      unmatched remote.
- [ ] 6.4 Pull uses the same `match` resolution as push for a given target, no
      separate surface for now (see proposal's Open Questions / design note if a
      real need for divergence shows up later).

## 7. Config include/merge (P1)

- [ ] 7.1 `config.toml` parser recognises leading `!include <path>` /
      `!include_try <path>` lines (same two spellings `nix.conf` itself uses),
      each path resolved relative to the including file.
- [ ] 7.2 `_try` variant: missing file is silently skipped. Bare `include`:
      missing file is a load error naming the path.
- [ ] 7.3 Merge: per top-level table (`[cache.<name>]`, etc.), the last-loaded
      file wins on key collision. Collisions logged at debug with both file
      paths; never a hard error.
- [ ] 7.4 Recursive includes bounded (max depth, cycle detection) - mirrors the
      bounded-walk posture `devshell-flake-discovery` already requires of its
      own filesystem walk.

## 8. Parallel push (P1)

- [ ] 8.1 `push_to_cache` pushes to all resolved targets via a bounded thread
      pool instead of the current sequential `for` loop.
- [ ] 8.2 `max_parallel_jobs`: `[cache]` top-level key,
      `--max-parallel-
      jobs` CLI flag, `XI_CACHE_MAX_PARALLEL_JOBS` env -
      same CLI > env > toml > default ladder used elsewhere in xi. Default:
      `min(std::thread::available_parallelism(), 8)`.
- [ ] 8.3 One target's failure does not block or cancel the others; each
      target's success/failure/enqueue is independent and reported separately
      (reuses today's per-target `CacheNotification` shape).

## 9. Automatic requeue drain (P1)

- [ ] 9.1 `push_to_cache` calls `cache_queue::drain(...)` for the
      currently-configured targets at the start of its own run, before (or
      concurrently with, once task 8 lands) attempting the current build's
      output.
- [ ] 9.2 `xi cache retry` keeps working as an explicit, immediate variant of
      the same drain for an operator who does not want to wait for the next
      build.
- [ ] 9.3 Drained entries use the bounded pool from task 8, not an unbounded
      burst.

## 10. `post-build-hook` registration (P2)

- [ ] 10.1 New capability, structured like `cache_prestage.rs`: atomic write of
      a one-line `post-build-hook = <path-to-xi-hook-script>` (or equivalent)
      into `nix.conf`, with a receipt for `unstage`/undo.
- [ ] 10.2 The hook script itself: reads `$OUT_PATHS`/`$DRV_PATH`, enqueues each
      output path against the currently-configured `[cache.*]` targets (same
      matcher as task 6 - resolved against the _build_'s flake context, not the
      hook script's own CWD, since `post-build-hook` runs as the Nix daemon with
      no meaningful CWD of its own).
- [ ] 10.3 Existing `post-build-hook` in `nix.conf`: refuse to overwrite, name
      the existing value in the error (see proposal Open Question 2; confirm
      exact behaviour against real-world cachix-installer-written configs before
      finalizing).
- [ ] 10.4 CLI: `xi cache posthook enable` / `xi cache posthook disable`.

## 11. Known-cache registry (P2)

- [ ] 11.1 Compiled-in `{name → (url, public_key)}` table: `cache.lix.systems`,
      `nix-community.cachix.org`, `devenv.cachix.org`, `cache.garnix.io`.
      `cache.nixos.org` excluded - already Nix's own default, nothing to opt
      into.
- [ ] 11.2 `preset = "<name>"` on a `[cache.<name>]` target resolves
      `pull_url`/`pull_public_key` from the table; explicit fields still
      override.
- [ ] 11.3 Unknown preset name: load-time error listing the valid names (same
      posture as every other closed enum in this codebase).

## 12. `xi cache doctor` (P2)

- [ ] 12.1 New subcommand: for each configured target, check (a) credential
      files mounted and readable, (b) endpoint reachable (HTTP(S) HEAD/GET with
      the existing `KEY_FETCH_TIMEOUT`-style short timeout), (c) signing key
      readable and produces a public key via `nix key convert-secret-to-public`,
      (d) that derived/declared public key appears in the _effective_
      `trusted-public-keys` xi would pass for a pull from this target.
- [ ] 12.2 Exit non-zero on any failed check; one line per target per check,
      naming the specific problem (not just "misconfigured").

## 13. `required` targets (P1)

- [ ] 13.1 `[cache.<name>].required` (default `false`). A `required` target's
      push failure (immediate, not merely "ended up queued") propagates to the
      calling `xi ... switch`'s own exit code.
- [ ] 13.2 A `required` target still gets queued on failure like any other (task
      9's auto-drain still retries it) - `required` changes _this invocation's_
      exit code, not whether the retry queue applies.

## 14. Tests (P1)

- [ ] 14.1 Provider URI resolution: table-driven tests per `CacheProvider`
      variant, including the `push_url` escape-hatch precedence.
- [ ] 14.2 Credential `Secret` injection: assert the env var is present on the
      spawned `Command` and absent from the parent process's own environment
      snapshot.
- [ ] 14.3 Match resolution: most-specific-wins, no-match-no-push,
      `default = true` fallback, explicit flake-ref outranking git-remote
      detection.
- [ ] 14.4 Include/merge: `_try` on a missing file, collision logging, cycle
      detection.
- [ ] 14.5 Parallel push: all targets attempted even when one blocks (mock a
      slow/hanging target, assert the others still complete within a bound).
- [ ] 14.6 Auto-drain: a queued entry from a prior run is retried on the next
      `push_to_cache` call without an explicit `xi cache retry`.
- [ ] 14.7 `required`: failing a `required` target flips the caller's exit code;
      failing a non-required one does not.
