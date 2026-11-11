# xi-repl Specification

## Purpose

`xi repl` is a new, `reedline`-backed interactive shell for exploring a flake:
Tab completion with a description/version preview pane, structured (foldable,
colorized) value rendering instead of opaque derivation strings, `rnix-parser`
syntax highlighting of the input line, a `:doc` meta-command for package
metadata, and a `:raw` escape hatch to a real `nix repl` subprocess when the
daemon's evaluation path can't handle an expression. It evaluates expressions
through a new `ExprEval` request/response extension to the `xi-develop` daemon's
`protocol_v3`, reusing the daemon's already-warmed flake eval state instead of
cold-starting a fresh evaluator per expression. It is a distinct, additional
entry point from `nix-command`'s existing `CommandKind::Repl`
(`crates/nix-command/src/lib.rs`), which remains an unmodified, direct
passthrough to `nix repl`.

### Entities

- **Repl session** - one `xi repl` invocation: a `reedline` editor instance, its
  history, multiline input handling, and scoping to the current flake root.
- **ExprEval request/response** - the `protocol_v3` extension used to evaluate
  an expression against the daemon's resolved flake: a request carrying `expr`
  and a `shallow` bool, and a response that is one of `Attrs(Vec<String>)`
  (attribute names only, never forces realisation),
  `Derivation { name, version, description }`, `Scalar(serde_json::Value)`,
  `Lambda`, or `Thunk`.
- **Completion candidate** - one attribute name offered in the Tab-triggered
  completion menu, whose `meta.description`/version preview is enriched lazily,
  only once the candidate is actually visible in the dropdown.
- **Value renderer** - the shared, `println!`-free module that turns a nix value
  into formatted strings/structs for display, consumed by
  `xi-flake/src/show.rs`, `xi-search/src/render/packages.rs`, and `xi-repl`
  alike instead of each keeping its own copy.
- **Daemon lifecycle** - the policy governing how a repl session obtains a live
  `xi-develop` daemon: auto-spawn one scoped to the session when none is running
  for the current flake, or run in a degraded, always-cold-eval mode when
  `--no-daemon` is passed.
- **`:raw` escape** - the fallback path triggered after a named daemon-eval
  error, re-executing the same expression against a real `nix repl` subprocess
  instead of leaving the session stuck or crashed.

### Related Capabilities

- `xi-develop`'s existing scheduled hook-eval loop (`ServerState::should_eval`,
  `spawn_eval`, `eval_in_progress`) is **extended, not replaced**: it gains the
  `ExprEval` request/response pair to serve ad-hoc repl evaluations, sharing the
  same eval gating, while its own scheduled hook-eval behavior is unchanged.
- `nix-command`'s `CommandKind::Repl` (`crates/nix-command/src/lib.rs`) is
  **unrelated and unmodified**: it stays a direct, unwrapped passthrough to
  `nix repl`, independent of `xi repl` as a separate entry point.
