//! `xi repl`  -  an interactive session against a flake's Nix evaluation
//! context.
//!
//! Phase 0 (`openspec/changes/xi-repl/tasks.md` tasks 4.1-4.5): a plain
//! `reedline` read-eval-print loop over the `xi-develop` daemon's
//! `ExprEval` query, with basic `yansi` coloring by value kind.
//!
//! Phase 1 (tasks 5.1-5.3, [`completion`]): `.`/Tab-triggered attribute-path
//! completion with a preview pane, backed by the same `ExprEval` query.
//!
//! Phase 2 (tasks 6.1-6.3): `rnix-parser`-based syntax highlighting
//! ([`highlight`]) and a colorized, indented tree view for attrset/list
//! `ExprEval` results ([`render`]).
//!
//! Phase 3 (tasks 7.1-7.3): the `:doc <expr>` meta-command ([`doc`]),
//! per-flake-root history (`session::history_path`), and the `:raw` escape
//! hatch into a real `nix repl` subprocess ([`raw`]).

pub mod args;
mod completion;
mod doc;
pub mod error;
mod highlight;
// `pub` (rather than the private `mod` every sibling module but `args`/
// `error`/`render`/`session` uses) solely so `tests/bdd_raw_escape.rs` can
// reach `raw::repl_command` and `raw::enter` - integration tests only see a
// crate's public API, and task 8.4 needs to assert on the exact `nix repl`
// invocation this module builds without duplicating its construction logic
// in the test itself.
pub mod raw;
pub mod render;
pub mod session;
mod suggest;
pub mod theme;
