# Proposal

## Why

`xi` already ships several pieces of nix-evaluation tooling, but none of them
are wired into an interactive session, and the interactive entry point that
exists today is a bare passthrough:

- `nix-command`'s `CommandKind::Repl` (`crates/nix-command/src/lib.rs:129`,
  `:190`) is a direct, unwrapped invocation of `nix repl` - no color beyond what
  `nix repl` itself prints, no completion beyond its own plain-text
  attribute-name list, and no reuse of anything else `xi` already knows how to
  do.
- `nix repl`'s completion forces a fresh evaluation on every Tab press with no
  persistent cache across the session; on a nixpkgs-sized flake that is seconds
  of latency per keystroke, and the result carries no
  `meta.description`/version - just the bare attribute name.
- `xi-flake/src/show.rs` (`OutputEntry`, `split_name_version`) and
  `xi-search/src/render/packages.rs` each independently implement "take a nix
  value or search-index doc, pull out name/version/description, print it in
  `yansi` color" - the exact logic a completion preview pane needs, but it is
  private to each one-shot command and not reusable.
- `xi-develop`'s daemon (`crates/xi-develop/src/daemon/server.rs`) already
  solves "keep a flake's eval state warm, invalidate only on relevant file
  change" (`ServerState::should_eval`, `EvalCacheMode`, `eval_input_hash`) - for
  its own file-watch hook loop. Nothing else in `xi` can talk to it; its
  `protocol_v3` wire format (`daemon/protocol_v3/mod.rs`, `v3_handlers.rs`,
  `reply.rs`) is shaped for that one caller, not for arbitrary ad-hoc expression
  evaluation.

The net result: "explore this flake interactively, with the kind of
discoverability `mycli` gives MySQL" is not available from `xi` today. The
choice is cold `nix repl` (plain text, no docs, slow) or separate one-shot
subcommands (`show`, `search`) that do not compose into a single session.

## What Changes

- **New `xi repl` subcommand**, new crate `xi-repl`, built on `reedline` (new
  workspace dep) for colored multiline editing, history, and a completion menu
  with a side preview pane. This is an additional entry point; `nix-command`'s
  existing `CommandKind::Repl` passthrough to real `nix repl` is untouched.
- **New daemon protocol variant.** `protocol_v3` gains
  `ExprEval { expr,
  shallow }` → a typed result (`Attrs(Vec<String>)` of
  attribute names only when `shallow`, never forcing a derivation build to list
  them; `Derivation { name, version, description }`;
  `Scalar(serde_json::
  Value)`; `Lambda`; `Thunk`). Backed by
  `nix eval --json --expr` run against the daemon's already-resolved flake
  input, gated by the same `eval_input_hash`/`eval_cache_mode` `ServerState`
  already tracks.
- **Completion with preview**, mycli-style: on `.`/Tab,
  `ExprEval{shallow:
  true}` lists candidate attribute names without forcing
  anything; only the candidates actually visible in the dropdown are then
  enriched with `meta.description`/version, using the parser extracted in the
  next bullet - never the full candidate set, to keep each keystroke cheap.
- **Shared value renderer, extracted, not duplicated.**
  `xi-flake/src/
  show.rs`'s `OutputEntry`/`split_name_version` and
  `xi-search/src/
  render/packages.rs`'s description/version coloring move into
  one shared module (new `xi-core::nix_value` or similar). `show.rs` and
  `render/
  packages.rs` become callers of it instead of each carrying their
  own copy; `xi-repl` becomes a third caller. Net: less code in the two existing
  files, not just more code added for the new one.
- **Structured, foldable value display.** Attrsets/lists render as colorized
  trees; a derivation-valued result renders as a one-line colored
  name/version/description card instead of `nix repl`'s opaque
  `«derivation /nix/store/...»` string.
- **Syntax highlighting** of the input line via `rnix-parser` (new workspace
  dep, the same parser `nixd`/`nil` use), feeding `reedline`'s `Highlighter`
  trait.
- **`:doc <expr>`** meta-command surfacing `meta.description`/
  `meta.longDescription` for package-valued expressions. Builtins/`lib` function
  docs are explicitly **out of scope for v1** (see Open Questions) - flagged,
  not silently dropped.
- **Explicit escape hatch, never a silent stall.** Any expression the daemon's
  eval path can't handle (needs full IFD, daemon not running, etc.) surfaces a
  named error with a `:raw` command that re-executes the session in real
  `nix repl` instead.

## Capabilities

### New Capabilities

- `xi-repl-session`: the `xi repl` subcommand itself - `reedline`-backed editor,
  history, multiline input.
- `xi-repl-eval-protocol`: the `ExprEval` daemon request/response extension to
  `protocol_v3`.
- `xi-repl-completion`: shallow attribute-name listing plus
  visible-candidate-only preview enrichment.
- `xi-repl-render`: the value renderer extracted out of
  `xi-flake/src/
  show.rs` and `xi-search/src/render/packages.rs` into one
  shared implementation, consumed by all three.
- `xi-repl-highlight`: `rnix-parser`-based syntax highlighting of the input
  line.
- `xi-repl-doc`: `:doc` meta-command, package-`meta.*` only for v1.

### Related Capabilities (unchanged)

- `nix-command`'s `CommandKind::Repl`: stays exactly as today, a direct
  `nix repl` passthrough, independent entry point from `xi repl`.
- `xi-develop`'s existing scheduled hook-eval loop (`ServerState::should_eval`,
  `spawn_eval`): gains a new request variant to serve (`ExprEval`); its own
  scheduled eval behaviour is unchanged.

## Impact

- **New crate** `crates/xi-repl` (deps: `reedline`, `rnix-parser`, `serde_json`,
  `yansi`; depends on `xi-core`, the daemon's `client_v3`, and the shared
  renderer).
- `crates/xi-develop/src/daemon/protocol_v3/{mod.rs,reply.rs}`,
  `v3_handlers.rs`: new `ExprEval` request/response variants.
- `crates/xi-develop/src/daemon/server.rs`: new handler reusing `ServerState`'s
  existing eval gating; must not starve or race the existing scheduled hook-eval
  loop (see Open Questions - concurrency story between `ExprEval` and
  `spawn_eval`/`eval_in_progress` needs to be decided before this lands, not
  discovered by a flaky test later).
- `crates/xi-flake/src/show.rs`, `crates/xi-search/src/render/
  packages.rs`:
  refactored to call the new shared renderer; expected to shrink, not grow.
- `crates/xi/src/interface.rs`, `crates/xi/src/lib.rs`: wire `xi repl` as a new
  top-level subcommand alongside `xi flake show`, `xi cache`, etc.
- Workspace `Cargo.toml`: add `reedline`, `rnix-parser` as workspace
  dependencies.
- **Breaking?**: No. Entirely new subcommand plus a new, additive daemon
  protocol variant. `CommandKind::Repl` and every existing one-shot command keep
  their current behaviour; the `show.rs`/`render/
  packages.rs` refactor is
  required to be output-identical (see Success Criteria), not a behaviour
  change.
- **Migration**: None. Nothing existing changes shape; the extraction is purely
  internal.

## Success Criteria

- **SC-REPL-001**: `xi repl` reaches an interactive prompt on a given flake in
  no worse time than `xi flake show` takes on the same flake (both pay the same
  cold-eval cost; the repl must not add its own extra cold-start tax on top).
- **SC-REPL-002**: Typing a partial attribute path (e.g. `pkgs.ne`) and pressing
  Tab against nixpkgs shows a completion dropdown with description previews
  without forcing a derivation build for any candidate not selected.
- **SC-REPL-003**: Evaluating a derivation-valued expression renders a colorized
  name/version/description card, never a raw, unparsed store-path string.
- **SC-REPL-004**: An expression the daemon's eval path cannot handle surfaces a
  named error and offers `:raw`; it never silently hangs the session or crashes
  it.
- **SC-REPL-005**: The shared renderer extracted from
  `show.rs`/`render/
  packages.rs` produces output identical to today's
  `xi flake show` / `xi search` on their existing test fixtures - the refactor
  is a pure move, verified by the existing test suites continuing to pass
  unchanged against the new shared implementation.
- **SC-REPL-006**: Running `xi repl` against a flake whose `xi-develop` daemon
  is simultaneously running its own scheduled hook-eval loop does not corrupt,
  deadlock, or starve either the hook loop or the repl's `ExprEval` requests.

## Open Questions

1. **Locking/ordering** between the daemon's existing scheduled hook-eval loop
   (`spawn_eval`/`eval_in_progress`) and new ad-hoc `ExprEval` requests from a
   repl session: share the existing lock and a single queue, or run on separate
   lanes with their own concurrency budget? Needs resolving before the
   `server.rs` task lands, not left to whichever behaviour the first
   implementation happens to produce.
2. **`:doc` for builtins/`lib` functions** (as opposed to package `meta.*`,
   which is in scope for v1): a curated static table (accurate but drifts from
   nixpkgs over time and is manual upkeep), a best-effort scrape of doc-comments
   via `rnix-parser` the way `nixdoc` does (more "automatic" but unproven output
   quality against real-world `lib`), or explicitly out of scope beyond
   package-level `meta.*` for v1. Leaning toward the last option for v1,
   revisited once the rest of the repl has shipped and real usage shows whether
   it's actually missed.
3. **In-process evaluator (`tvix-eval`)** as a later optimization for the
   completion hot path specifically, instead of a daemon + `nix eval` round-trip
   per keystroke: not committing either way until Phase 0's latency numbers
   exist (see tasks.md). `tvix-eval` does not support 100% of real-world nixpkgs
   (restrict-eval quirks, some builtins gaps), so it is a candidate optimization
   behind the daemon path, not a replacement for it.
4. **Daemon lifecycle for `xi repl`**: auto-spawn a daemon scoped to the repl
   session if none is already running for this flake, or require an
   already-running `xi-develop` daemon and otherwise fall back to a degraded
   "always-cold-eval" mode (still colored/completable, just without the
   warm-eval speedup)? Leaning toward auto-spawn-scoped-to- session as the
   default with an explicit `--no-daemon` flag for the degraded mode, but this
   depends on whether `xi-develop` already has an "ephemeral daemon for one
   invocation" pattern anywhere, or whether its daemon lifecycle has only ever
   assumed a long-lived dev-loop process.
