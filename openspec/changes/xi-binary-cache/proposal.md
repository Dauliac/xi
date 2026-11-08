# Proposal

## Why

Today's `[cache.<name>]` (`crates/xi-core/src/args.rs`,
`crates/xi-core/src/
cache.rs`) is push-only: a bare `push_url` + optional
`signing_key` path, shelled into `nix store sign` then `nix copy --to`, with a
persistent retry queue (`cache_queue.rs`) for failed pushes. Everything else a
personal or team binary-cache setup needs is either missing or lives outside xi
entirely:

- No native pull-side configuration. `xi sources cache-prestage`
  (`crates/xi-core/src/sources/cache_prestage.rs`) writes substituters and
  trusted-public-keys into `nix.conf`, but only during a fleet bootstrap
  (`xi sources bootstrap`) of a brand-new machine - it reads what the flake
  already declares, it does not let an operator declare a cache once and have
  every xi command (and every _other_ `nix` invocation on the box) use it.
- Credentials for push (S3 access key/secret) have no native home. The only
  working pattern today is ambient shell environment (`AWS_ACCESS_KEY_ID`
  exported for an entire session), which is a wider blast radius than necessary
  and sits outside xi's existing `Secret` primitive
  (`crates/xi-core/src/sources/credentials.rs`), already used for git
  credentials and already solving "read from a file, redact everywhere, tolerate
  not-yet-mounted".
- Every S3-compatible provider (Cloudflare R2, AWS S3, Backblaze B2, Wasabi,
  MinIO) needs the same hand-built `s3://bucket?region=...&
  endpoint=...` URI
  with no help computing `endpoint` from the handful of facts (account id,
  region) that actually vary per provider.
- A single `[cache.<name>]` table has no way to say "only for this git remote /
  this group on that remote", so an operator with a personal GitHub cache and a
  corp GitLab cache has no way to stop a personal repo's build from accidentally
  landing on the corp bucket, or vice versa, short of hand-toggling config per
  repo.
- `[cache.<name>]` credentials cannot be split into a separately-managed
  (sops-encrypted) file layered on top of a repo-committed base - the
  `corp_gitlab_host`-style "hide the hostname, not just the token" pattern this
  very dotfiles repo already uses for `corp.nix` has no xi-side equivalent.
- Multiple configured push targets are pushed to **sequentially** today
  (`push_to_cache`'s `for target in &targets` loop in `cache.rs`), and the retry
  queue is only drained on an explicit `xi cache retry` - a build that failed to
  push last time does not automatically get another try on the next build.
- Push coverage is limited to commands that go through xi's own subcommands
  (`xi os/home/nixos/darwin switch`, confirmed by grepping every `push_to_cache`
  call site). A bare `nix build`, a CI job, or any other tool never pushes
  anywhere, even on a machine with `[cache.*]` fully configured.
- No built-in, pre-verified entries for well-known public caches
  (`cache.lix.systems`, `nix-community.cachix.org`, `devenv.cachix.org`,
  `cache.garnix.io`) - every operator who wants one copies a URL and a public
  key from a blog post or README by hand, a well-known source of silent typos in
  `trusted-public-keys`.
- No single command tells an operator "is my cache actually going to work"
  before they find out the hard way mid-build.

## What Changes

- **Provider presets, one generic engine.** `CacheTarget` gains a `provider`
  field (closed enum: `R2 { account_id }`, `S3 { region }`,
  `Generic { endpoint, region }`), used ONLY to pre-fill `endpoint`/`region`
  before the existing `s3://bucket?...` URI is assembled. The actual push/ pull
  mechanics (`nix copy --to`, `nix store sign`, substituter flags) stay
  provider-agnostic - confirmed against the Nix manual's own
  `s3-binary-cache-store` page, which describes one generic store type
  parameterised by `region`/`endpoint`/`profile`/`scheme`, not a per-vendor
  plugin model.
- **Native credentials.** `[cache.<name>.credentials]` carries file paths
  (`access-key-id-file`, `secret-access-key-file`), read through the existing
  `Secret`/`Secret::from_file` type from `xi-core::sources::credentials`
  (redacted `Debug`, refuses `Serialize`, zeroed on drop, `NotMounted` is a
  `Pending` state rather than an error). Values are injected as env vars scoped
  to the single `nix copy` child `Command` xi spawns - never exported into the
  parent shell.
- **Pull configuration, applied as flags, not files.** `pull_url` +
  `pull_public_key` (or an auto-derived public key, see below) become
  `--option substituters <url>` / `--option trusted-public-keys <key>` on every
  `nix` subprocess xi itself spawns. `xi sources cache-prestage`'s
  `nix.conf`-writing path is unchanged and stays reserved for its own narrower
  case (pre-xi bootstrap builds); this change does not route through it for the
  common case.
- **Auto-derived public key.** When `pull_public_key` is absent but
  `signing_key` is set, xi computes the public half via
  `nix key convert-secret-to-public < signing_key`, caches the result next to
  the key (invalidated on the key file's mtime), and uses it for
  `trusted-public-keys`. Removes a field that could otherwise drift out of sync
  with the private key.
- **Git-remote / flake-scoped matching.** `[cache.<name>]` gains `match` (a list
  of `host/path` glob patterns, e.g. `"github:dauliac/*"`,
  `"gitlab.corp.example.com/*"`, `"gitlab.corp.example.com/team-x/*"`).
  Resolution order: (1) an explicit flake-ref argument passed to the xi
  subcommand, if any; (2) the git remote of the flake root resolved by
  `xi-develop`'s existing `find_flake_root` (`flake_detect.rs`), normalised to
  `(host, path)` via a new small parser built on
  `xi-core::sources::config::validate_remote_url`'s existing scheme table. The
  most specific matching pattern wins. No match: no push, with a visible
  warning, unless a cache is explicitly marked `default = true`. Pull uses the
  same `match` by default (one cache == one repo's cache, by default) - no
  separate pull-matching surface introduced until a concrete need for one shows
  up.
- **Multi-file config merge.** `config.toml` supports `!include <path>` /
  `!include_try <path>` directive lines, mirroring the exact semantics
  `cache_prestage.rs` already uses for `nix.conf` (`_try` silently skips a
  not-yet-mounted file - same `Pending` philosophy as `Secret::from_file`). Lets
  a repo-committed base config `!include_try` a sops-rendered fragment carrying
  the sensitive parts of a `[cache.<name>]` entry (e.g. a `match` pattern naming
  an internal hostname) without that hostname ever landing in the committed
  file. Per-table, later-loaded file wins on key collision; collisions are
  logged at debug, not errors.
- **Parallel push across targets.** `push_to_cache` pushes to all configured
  targets concurrently (bounded thread pool) instead of the current sequential
  loop. A new `max_parallel_jobs` setting (`[cache]` top-level, CLI flag,
  `XI_CACHE_MAX_PARALLEL_JOBS` env - same CLI > env > toml > default ladder as
  the rest of xi) bounds concurrency; default is
  `min(available_parallelism(), 8)`. This governs how many `nix copy`
  **subprocesses** xi runs at once; it is independent of (and does not replace)
  Nix's own `http-connections` setting, which governs parallel TCP connections
  _inside_ a single `nix copy` invocation.
- **Automatic requeue drain.** `push_to_cache` drains the persistent queue
  (previously-failed pushes) at the start of every call, before attempting the
  current build's output, instead of requiring an explicit `xi cache retry`. A
  push that failed because the machine was offline gets retried automatically on
  the very next build, no operator action needed.
- **`post-build-hook` registration.** A new capability lets xi install itself as
  the Nix daemon's `post-build-hook` (one line in `nix.conf`, written and
  reversed with the same atomic-write + receipt pattern `cache_prestage.rs`
  already uses), so **every** build on the machine - not only ones run through
  an `xi` subcommand - gets pushed through the same `[cache.*]` targets,
  matching, queue, and parallelism.
- **Known-cache registry.** A small compiled-in table of
  `{name → (url, public_key)}` for well-known public caches
  (`cache.lix.systems`, `nix-community.cachix.org`, `devenv.cachix.org`,
  `cache.garnix.io`), selectable via `preset = "<name>"` instead of a hand-typed
  URL and key. Strictly opt-in per target; no entry is ever added to anyone's
  substituters without an explicit `[cache.<name>]` naming it.
- **`xi cache doctor`.** New subcommand: for each configured target, checks the
  credential files are mounted (not `Pending`), the endpoint is reachable, the
  signing key is readable, and (when derivable) that its public key is present
  in effective `trusted-public-keys` somewhere reachable. Reports problems
  before the next real build hits them.
- **`required` targets.** `[cache.<name>]` gains `required = true|false`
  (default `false`). A `required` target's push failure makes the triggering
  `xi ... switch` exit non-zero instead of silently queuing; a non-required
  target still queues and retries as today.

## Capabilities

### New Capabilities

- `xi-binary-cache-targets`: provider presets, native file-backed credentials,
  auto-derived public keys, `required` targets - the extended `[cache.<name>]`
  schema and its push/pull execution.
- `xi-binary-cache-match`: resolves the current git remote / flake-ref to
  `(host, path)` and selects which configured cache target(s) apply.
- `xi-binary-cache-include`: `!include`/`!include_try` directives for
  `config.toml`, enabling multi-file, partially-secret configuration.
- `xi-binary-cache-parallel`: bounded concurrent push across targets, and
  automatic queue-drain on every push cycle.
- `xi-binary-cache-posthook`: registers xi as the Nix daemon's
  `post-build-hook`, extending push coverage to every build on the machine.
- `xi-binary-cache-registry`: compiled-in presets for well-known public caches.
- `xi-binary-cache-doctor`: `xi cache doctor` diagnostic subcommand.

### Related Capabilities (unchanged)

- `xi-sources-cache-prestage`: keeps its existing, narrower scope (fleet
  bootstrap of a brand-new machine, reading `nix.settings.substituters` from the
  resolved flake). Not modified by this change; the two mechanisms coexist
  (prestage covers the chicken-and-egg bootstrap moment,
  `xi-binary-cache-posthook` covers steady-state operation afterward).

## Impact

- **Affected code**:
  - `crates/xi-core/src/args.rs` (`CacheTarget`/`CacheArgs` schema: `provider`,
    `pull_url`, `pull_public_key`, `match`, `required`, `credentials`).
  - `crates/xi-core/src/cache.rs` (URI assembly from provider presets, scoped
    credential injection via `Command::env`, parallel push, auto-derived public
    key + its on-disk cache, `--option substituters`/
    `--option trusted-public-keys` flag wiring for pull).
  - `crates/xi-core/src/cache_queue.rs` (drain-on-push instead of
    drain-on-demand-only).
  - `crates/xi-core/src/sources/config.rs` (extract a reusable `(host, path)`
    normaliser out of `validate_remote_url`'s scheme table).
  - `crates/xi-develop/src/flake_detect.rs` (expose `find_flake_root`'s result
    to the new matcher; no behaviour change to flake discovery itself).
  - `crates/xi/src/config.rs` (`!include`/`!include_try` resolution before
    `ConfigStore::config()` is read).
  - New: a `post-build-hook` registration capability, structured like
    `cache_prestage.rs` (atomic write, receipt, undo), but writing a one-line
    hook invocation instead of substituter lines.
  - New: `xi cache doctor` subcommand (`crates/xi/src/cache/mod.rs`).
  - `modules/home-manager/settings.nix`, `modules/nixos/settings.nix`: the
    Nix-module surface stays limited to writing xi's `config.toml` (now possibly
    as `!include_try`-linked fragments pointing at `sops.secrets.*.path`) and,
    where the new post-build-hook capability needs a system-level `nix.conf`
    line, invoking that capability's writer the same way `cache-prestage` is
    already invoked today.
- **Coordination**: none outside this repo; the dotfiles repo
  (`Dauliac/dotfiles`) consumes this as a flake input and will gain its own
  follow-up change once this lands, to wire a Cloudflare R2 personal cache and
  (optionally) a corp AWS S3 cache through the new schema.
- **Breaking?**: No. Every new field is optional; `[cache.<name>]` entries using
  only `push_url`/`signing_key`/`push_command` keep working exactly as today.
  `match` defaults to "matches everything" when absent, so existing single-cache
  setups are unaffected.
- **Migration**: None required. Adopting `provider`/`credentials`/`match` is
  opt-in per target.

## Success Criteria

- **SC-BC-001**: A `[cache.<name>]` with `provider = "cloudflare-r2"` and
  `account_id` produces the same `s3://` URI as hand-writing
  `endpoint=https://<account_id>.r2.cloudflarestorage.com&region=auto` today.
- **SC-BC-002**: Push credentials never appear as an environment variable on any
  process other than the single `nix copy` child xi spawns; a `ps -eo pid,cmd` /
  `/proc/<ppid>/environ` snapshot of xi's own process during a push shows no
  `AWS_SECRET_ACCESS_KEY`.
- **SC-BC-003**: A credential file that does not exist yet produces `Pending`,
  not an error; the push is queued, not failed-and-dropped.
- **SC-BC-004**: Running `xi home switch` from inside a repo whose git remote
  matches `gitlab.corp.example.com/team-x/*` pushes only to the target whose
  `match` is that pattern, not to a sibling target matching only
  `gitlab.corp.example.com/*`.
- **SC-BC-005**: Running from a repo matching no configured target pushes
  nowhere and prints a warning, unless some target has `default = true`.
- **SC-BC-006**: A `config.toml` with `!include_try /path/not/mounted/yet` loads
  successfully with that fragment's tables simply absent.
- **SC-BC-007**: Configuring N push targets reduces total push wall-clock for a
  build whose closure is already present at all N destinations (fast no-op case)
  to close to the single slowest target's time, not the sum of all N.
- **SC-BC-008**: A push that fails (destination unreachable) is retried
  automatically on the very next `xi ... switch`, without the operator running
  `xi cache retry`.
- **SC-BC-009**: With the post-build-hook capability enabled, a bare `nix build`
  run outside any xi subcommand still results in the built path being
  queued/pushed to the configured `[cache.*]` targets.
- **SC-BC-010**: `xi cache doctor` exits non-zero and names the specific problem
  when a configured target's credential file is missing, its endpoint is
  unreachable, or its signing key is unreadable.
- **SC-BC-011**: A `required = true` target's push failure makes
  `xi home switch`'s own exit code non-zero; a `required = false` (default)
  target's failure does not.

## Open Questions

1. `max_parallel_jobs`'s default (`min(available_parallelism(), 8)`) is a
   process-count bound for `nix copy` subprocesses pushing to _different_
   targets. Should it also bound the queue-drain's concurrency, or does
   queue-drain get its own (likely smaller, since it runs opportunistically
   alongside a fresh push) setting? Leaning toward sharing one setting and one
   bounded pool across both, to keep the mental model to a single knob, but
   worth confirming once task 4 (parallel push) and task 5 (auto-drain) are both
   implemented and can be measured together.
2. `post-build-hook` is a single global setting in `nix.conf` - only one program
   can be registered. If the operator (or some other tool) already has a
   `post-build-hook` configured, does xi's writer refuse, chain to the existing
   hook, or overwrite with a warning? Leaning toward refuse with a named error
   pointing at the existing hook, matching the "classification, not silent
   fallback" rule `xi-push-target-pull` already set for its own
   connection-failure case - but this needs confirming against what real-world
   `nix.conf`s already carry there (cachix's own installer writes this same
   setting).
3. The `(host, path)` normaliser for `match` needs to decide how
   `ssh://
   git@host:2222/group/repo.git` (non-standard port) and scp-style
   `git@host:group/repo.git` map to the same `host/path` shape `match` patterns
   are written against. `validate_remote_url` already accepts both without
   normalising them; this change is the first caller that needs the normalised
   form, not just validation.
