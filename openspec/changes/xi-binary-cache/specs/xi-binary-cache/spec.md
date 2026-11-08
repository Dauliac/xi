# Capability: xi-binary-cache

## ADDED Requirements

### Requirement: Provider presets resolve to one generic S3 URI

`CacheTarget` MUST accept a closed `provider` field (`cloudflare-r2`, `aws-s3`,
`generic`) that pre-fills `endpoint`/`region` before assembling the same
`s3://bucket?region=...&endpoint=...` URI the existing push path already
consumes. No provider MAY introduce a different push or pull mechanism; the only
difference between providers is which parameters get defaulted.

#### Scenario: Cloudflare R2 preset derives its endpoint

- **GIVEN** `provider = "cloudflare-r2"`, `account_id = "abc123"`,
  `bucket = "nix-cache"`
- **WHEN** the target's push URI is resolved
- **THEN** it equals
  `s3://nix-cache?region=auto&endpoint=https://abc123.r2.cloudflarestorage.com`

#### Scenario: AWS S3 preset omits endpoint

- **GIVEN** `provider = "aws-s3"`, `region = "eu-west-3"`,
  `bucket = "nix-cache"`, no explicit `endpoint`
- **WHEN** the target's push URI is resolved
- **THEN** it equals `s3://nix-cache?region=eu-west-3` with no `endpoint`
  parameter

#### Scenario: Hand-written push_url overrides the preset

- **GIVEN** both `provider`/`bucket` and an explicit `push_url` are set on the
  same target
- **WHEN** the target's push URI is resolved
- **THEN** the explicit `push_url` is used verbatim, unmodified by the provider
  preset

#### Scenario: Unknown provider is a load-time error

- **GIVEN** `provider = "not-a-real-provider"`
- **WHEN** the config is loaded
- **THEN** loading fails, naming the invalid value and the closed set of
  accepted ones

### Requirement: Credentials are native, file-backed, and scoped

Push credentials for a target MUST be declared as file paths under
`[cache.<name>.credentials]`, read via the existing `Secret::from_file`
primitive, and injected only as environment variables on the single `nix
copy`
child process xi spawns for that target.

#### Scenario: Credentials never reach the parent process environment

- **GIVEN** a target with `access-key-id-file`/`secret-access-key-file` both
  pointing at mounted files
- **WHEN** xi pushes to that target
- **THEN** the spawned `nix copy` child process has
  `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` set, AND xi's own process
  environment (before and after the push) does not

#### Scenario: Missing credential file is Pending, not a failure

- **GIVEN** a target whose `secret-access-key-file` does not exist on disk
- **WHEN** xi attempts to push to that target
- **THEN** the push is enqueued for retry (as any other push failure would be),
  and the error surfaced is a `Pending`/`not mounted` kind, not a generic I/O
  error

### Requirement: Pull configuration is per-invocation flags, not a file write

A target's `pull_url` and resolved public key MUST be applied as
`--option substituters` / `--option trusted-public-keys` on every `nix`
subprocess xi itself spawns. This requirement MUST NOT write or amend
`nix.conf`.

#### Scenario: Pull flags are additive

- **GIVEN** a `nix build` invocation xi is about to spawn, and a resolved
  `[cache.<name>]` with `pull_url` set
- **WHEN** xi builds the subprocess command
- **THEN** the cache's `pull_url` is appended to whatever substituters the
  flake/system already declare, never replacing them

#### Scenario: Auto-derived public key from a signing key

- **GIVEN** a target with `signing_key` set and no `pull_public_key`
- **WHEN** the target's effective trusted key is resolved
- **THEN** it equals the output of
  `nix key convert-secret-to-public < signing_key`, computed once and cached
  next to the key file until the key file's mtime changes

#### Scenario: Explicit public key overrides auto-derivation

- **GIVEN** a target with both `signing_key` and an explicit `pull_public_key`
  set
- **WHEN** the target's effective trusted key is resolved
- **THEN** the explicit `pull_public_key` is used, and
  `nix key convert-secret-to-public` is not invoked

### Requirement: Targets are scoped to a git remote or flake-ref

A `[cache.<name>]` target MAY declare `match`, a list of `host/path` glob
patterns. Resolution MUST prefer an explicit flake-ref argument on the invoking
command over the git remote of the resolved flake root for the current working
directory. The most specific matching pattern wins. Absent any match and absent
a `default = true` target, xi MUST push nowhere and print a warning naming the
unmatched remote.

#### Scenario: Most specific match wins

- **GIVEN** `cache.a` with `match = ["gitlab.corp.example.com/*"]` and `cache.b`
  with `match = ["gitlab.corp.example.com/team-x/*"]`
- **WHEN** resolving for a repo whose remote is
  `gitlab.corp.example.com/team-x/widget`
- **THEN** only `cache.b` is selected

#### Scenario: Explicit flake-ref outranks git-remote detection

- **GIVEN** the current directory's git remote matches `cache.a`, but the
  invoking command was given an explicit flake-ref matching `cache.b`
- **WHEN** resolving targets for that invocation
- **THEN** `cache.b` is selected, not `cache.a`

#### Scenario: No match, no default, no push

- **GIVEN** no configured target's `match` matches the current remote, and no
  target has `default = true`
- **WHEN** a build completes
- **THEN** no push is attempted, and a warning is printed naming the resolved
  remote and the fact that no cache matched it

#### Scenario: Default target catches the unmatched case

- **GIVEN** the same setup as above, but one target additionally has
  `default = true`
- **WHEN** a build completes
- **THEN** the push is attempted against that default target

### Requirement: Config supports multi-file include with graceful absence

`config.toml` MUST support `!include <path>` and `!include_try <path>` directive
lines, each resolved relative to the including file. `!include_try` of a missing
file MUST be silently skipped; bare `!include` of a missing file MUST be a load
error naming the path. Later-loaded files win on a per-table key collision.

#### Scenario: Secret fragment not yet mounted

- **GIVEN** a base `config.toml` with
  `!include_try
  /run/secrets/rendered/corp-cache.toml`, and that path does not
  exist yet
- **WHEN** the config is loaded
- **THEN** loading succeeds, and no `[cache.corp]` table is present

#### Scenario: Included file's table overrides the base file's

- **GIVEN** a base file declaring `[cache.corp] bucket = "placeholder"` and an
  included file declaring `[cache.corp] bucket = "real-bucket"`
- **WHEN** the config is loaded
- **THEN** the merged `[cache.corp].bucket` is `"real-bucket"`, and the
  collision is logged at debug level, not raised as an error

### Requirement: Push to multiple targets runs concurrently, bounded

`push_to_cache` MUST push to all resolved targets using a bounded concurrency
pool rather than a sequential loop. The bound MUST be configurable
(`max_parallel_jobs`: CLI flag, env var, `[cache]` top-level TOML key, in that
precedence order) and defaults to `min(available_parallelism(), 8)`.

#### Scenario: One slow target does not block the others

- **GIVEN** three configured targets, one of which is unreachable and times out
  slowly
- **WHEN** a build's output is pushed
- **THEN** the two reachable targets complete without waiting for the slow
  target's timeout

#### Scenario: Concurrency respects the configured bound

- **GIVEN** `max_parallel_jobs = 2` and five configured targets
- **WHEN** a build's output is pushed
- **THEN** at most two `nix copy` child processes for this push run concurrently
  at any instant

### Requirement: Failed pushes are retried automatically on the next push cycle

`push_to_cache` MUST attempt to drain the persistent retry queue
(previously-failed pushes, not yet expired) at the start of its own run, in
addition to pushing the current build's output. `xi cache retry` remains
available as an explicit, immediate variant of the same drain.

#### Scenario: A queued push is retried without operator action

- **GIVEN** a prior push to `cache.a` failed and was enqueued
- **WHEN** a subsequent, unrelated build triggers `push_to_cache` again
- **THEN** the queued entry for `cache.a` is retried as part of that same call,
  without the operator running `xi cache retry`

### Requirement: xi can register as the Nix daemon's post-build-hook

A new capability MUST let xi write a `post-build-hook` line into `nix.conf`
(atomic write, with a reversal receipt, mirroring `xi-sources-cache-prestage`'s
existing write/receipt pattern), so that every build on the machine - not only
ones run through an `xi` subcommand

- is pushed through the configured `[cache.*]` targets.

#### Scenario: A bare nix build still gets pushed

- **GIVEN** the post-build-hook capability is enabled, and `[cache.*]` targets
  are configured
- **WHEN** an operator runs `nix build` directly, outside any `xi` subcommand
- **THEN** the built output is enqueued/pushed to the matching configured
  targets exactly as if `xi home switch` had built it

#### Scenario: Existing post-build-hook is not silently replaced

- **GIVEN** `nix.conf` already declares a `post-build-hook` pointing at
  something xi did not write
- **WHEN** xi's post-build-hook registration runs
- **THEN** it refuses, naming the existing hook's path, rather than overwriting
  it

### Requirement: Known public caches are available as opt-in presets

A compiled-in registry of well-known public caches (at minimum
`cache.lix.systems`, `nix-community.cachix.org`, `devenv.cachix.org`,
`cache.garnix.io`) MUST be selectable via `preset = "<name>"` on a
`[cache.<name>]` target, resolving `pull_url`/`pull_public_key` without the
operator hand-typing either. An unknown preset name MUST be a load-time error
naming the valid set. No preset is ever added implicitly; a target naming one is
the only way it takes effect.

#### Scenario: Preset resolves without a hand-typed key

- **GIVEN** `[cache.community] preset = "nix-community"`
- **WHEN** the target's pull configuration is resolved
- **THEN** `pull_url` and `pull_public_key` are both populated from the
  compiled-in registry entry, with no value typed by the operator

### Requirement: `xi cache doctor` diagnoses targets before a real push

A new `xi cache doctor` subcommand MUST check, per configured target: credential
files are mounted and readable, the endpoint is reachable, the signing key is
readable and yields a public key, and that public key is present among the
`trusted-public-keys` xi would actually apply for a pull from that target. Any
failing check MUST exit non-zero and name the specific problem.

#### Scenario: Missing credential file is named specifically

- **GIVEN** a target whose `secret-access-key-file` does not exist
- **WHEN** `xi cache doctor` runs
- **THEN** it exits non-zero and its output names that target and that specific
  missing file, not a generic "misconfigured" message

### Requirement: Required targets fail the build on push failure

A `[cache.<name>]` target MAY set `required = true` (default `false`). A
required target's push failure MUST propagate to the invoking `xi ...
switch`
command's own exit code. A non-required target's failure MUST continue to only
enqueue for later retry, with no effect on the invoking command's exit code.

#### Scenario: Required target failure fails the switch

- **GIVEN** `[cache.ci] required = true`, and that target is unreachable
- **WHEN** `xi nixos switch` builds successfully but the push to `cache.ci`
  fails
- **THEN** `xi nixos switch` itself exits non-zero

#### Scenario: Non-required target failure does not fail the switch

- **GIVEN** `[cache.mirror] required = false` (the default), and that target is
  unreachable
- **WHEN** `xi nixos switch` builds successfully but the push to `cache.mirror`
  fails
- **THEN** `xi nixos switch` exits zero, and the failed push is enqueued for
  retry
