# Tasks: xi-repl

Live state in beads under `change:xi-repl`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-repl/spec.md`.
- [ ] 1.2 Cross-link with `xi-develop`'s existing eval-daemon capability
      (extended, not replaced) and `nix-command`'s `CommandKind::Repl`
      (unrelated, unmodified passthrough).

## 2. Shared value renderer extraction (P1)

This lands first, independent of the repl itself, so `xi-flake`/ `xi-search` get
the regression-safety win immediately and `xi-repl` has something to depend on
from day one.

- [ ] 2.1 New module (e.g. `crates/xi-core/src/nix_value.rs`) hosting
      `OutputEntry`/`split_name_version`-equivalent parsing, moved out of
      `crates/xi-flake/src/show.rs`.
- [ ] 2.2 `xi-flake/src/show.rs` calls the shared module; existing output
      byte-identical (verified by existing `show` tests, unchanged).
- [ ] 2.3 `xi-search/src/render/packages.rs`'s description/version coloring
      converges onto the same shared module; existing output byte-identical
      (verified by existing `render` tests, unchanged).
- [ ] 2.4 Shared module exposes a stable API (`render_entry`,
      `parse_name_version`, or similar) that does not assume a one-shot CLI
      context - no `println!`, returns formatted strings/structs the caller
      decides how to emit, since `xi-repl` needs to put the same data into a
      `reedline` preview pane, not stdout.

## 3. Daemon eval protocol extension (P1)

- [ ] 3.1 `ExprEval { expr: String, shallow: bool }` request variant in
      `xi-develop/src/daemon/protocol_v3/mod.rs`.
- [ ] 3.2 Response variant: `Attrs(Vec<String>)` (shallow: attribute names only,
      no forcing), `Derivation { name, version, description
      }`,
      `Scalar(serde_json::Value)`, `Lambda`, `Thunk` - in `reply.rs`.
- [ ] 3.3 Handler in `server.rs`/`v3_handlers.rs`: runs
      `nix eval --json
      --expr` against the daemon's already-resolved flake
      input, reusing `ServerState`'s `eval_input_hash`/`eval_cache_mode` gating
      rather than re-deriving its own cache key.
- [ ] 3.4 Concurrency story between `ExprEval` handling and the existing
      scheduled `spawn_eval`/`eval_in_progress` loop is decided and implemented
      here, not left implicit (see proposal Open Question 1). Default direction
      unless task investigation finds otherwise: share `eval_in_progress` for
      correctness, but let read-only `ExprEval` shallow queries bypass the
      full-eval mutex when the cached state is already fresh (`eval_input_hash`
      unchanged since last full eval).
- [ ] 3.5 `client_v3.rs` gains the corresponding request-sending helper for
      `xi-repl` to call.

## 4. Phase 0 - minimal repl shell (P1)

- [ ] 4.1 New crate `crates/xi-repl`; workspace `Cargo.toml` gains `reedline` as
      a workspace dependency.
- [ ] 4.2 `xi repl` subcommand wired in `crates/xi/src/interface.rs` / `lib.rs`.
- [ ] 4.3 Daemon lifecycle decision implemented: auto-spawn a session- scoped
      daemon if none is running for the current flake, `--no-
      daemon` flag
      for a degraded always-cold-eval mode (see proposal Open Question 4).
- [ ] 4.4 Plain `reedline` loop: read expression, send `ExprEval` (not shallow),
      print the raw typed result with basic `yansi` coloring by value kind (no
      completion, no highlighting yet) - this phase exists to measure round-trip
      latency before committing further UI work to it.
- [ ] 4.5 Measure and record warm-eval round-trip latency for a representative
      expression against a nixpkgs-sized flake; this number informs Open
      Question 3 (daemon-only vs. later `tvix-eval` hot path) and is not a
      blocking gate for the rest of this change, just a recorded input to it.

## 5. Phase 1 - completion with preview (P1)

- [ ] 5.1 `.`/Tab-triggered completion: `ExprEval{shallow: true}` on the prefix
      expression, candidate attribute names populate `reedline`'s completion
      menu.
- [ ] 5.2 Only the candidates currently visible in the dropdown are enriched
      with `meta.description`/version (via the shared renderer from task 2),
      shown in `reedline`'s preview pane - never the full candidate set on every
      keystroke.
- [ ] 5.3 Enrichment calls are async/non-blocking relative to the editor loop; a
      slow enrichment never blocks typing or candidate scrolling.

## 6. Phase 2 - highlighting + structured rendering (P2)

- [ ] 6.1 `rnix-parser` added as a workspace dependency; input-line tokenization
      feeds a `reedline` `Highlighter` implementation.
- [ ] 6.2 Structured, foldable rendering for attrset/list results (colorized
      tree, expand/collapse), using the shared renderer from task 2 for any
      derivation-valued leaf.
- [ ] 6.3 Derivation-valued top-level results render as a one-line colored
      name/version/description card, not the raw store-path string `nix repl`
      prints today.

## 7. Phase 3 - docs, history, escape hatch (P2)

- [ ] 7.1 `:doc <expr>` surfaces `meta.description`/`meta.longDescription` for
      package-valued expressions, via the shared renderer. Builtins/`lib` doc
      lookup explicitly deferred past v1 (proposal Open Question 2) - `:doc` on
      a non-package expression says so rather than silently returning nothing.
- [ ] 7.2 Per-flake-root history file (scoped the same way `xi-develop`'s
      flake-root detection already scopes its own state).
- [ ] 7.3 `:raw` command: any expression the daemon path rejects (named error,
      not a hang) can be re-run by dropping into a real `nix repl` subprocess
      for the remainder of that one evaluation.

## 8. Tests (P1)

- [ ] 8.1 Shared renderer (task 2): existing `xi-flake`/`xi-search` test suites
      pass unchanged against the extracted module - this is the primary
      regression gate for the refactor.
- [ ] 8.2 `ExprEval` protocol: shallow listing never triggers a derivation build
      (assert no `nix build`/realisation side effect for an unselected
      candidate).
- [ ] 8.3 Concurrency: a repl session's `ExprEval` requests and the daemon's own
      scheduled hook-eval loop running concurrently against the same flake
      neither deadlock nor produce a stale/corrupted result for either caller.
- [ ] 8.4 `:raw` escape: an expression that errors against the daemon path
      surfaces a named error and the `:raw` fallback actually executes against
      real `nix repl` rather than silently no-op'ing.
- [ ] 8.5 BDD: `xi repl` opens against a fixture flake, completes a known
      attribute path with a preview, evaluates a derivation to a structured
      card - end-to-end happy path.
