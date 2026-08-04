//! Compile-time state catalog for [`DaemonState`].
//!
//! # Purpose
//!
//! The daemon publishes a [`Query::StateCatalog`](crate::daemon::protocol) so
//! every consumer (status bars, `xi agent context`, editor plugins) renders
//! `DaemonState` uniformly. Without a single catalog, consumers drift.
//!
//! # Parity guarantee (SC-016)
//!
//! The state catalog covers 100% of concrete `DaemonState` variants. Adding a
//! `DaemonState` variant without extending the catalog fails at **build time**
//! (the exhaustive `match` in [`kind_of`] fails to compile) or at **startup**
//! (the [`assert_parity`] function panics).
//!
//! # How to extend
//!
//! When you add a variant `Foo` to [`DaemonState`]:
//!
//! 1. Add `Foo` to [`DaemonStateKind`] below.
//! 2. Add the `DaemonState::Foo { .. } => DaemonStateKind::Foo` arm to
//!    [`kind_of`]. The compiler will fail without it.
//! 3. Add a `(DaemonStateKind::Foo, StateMeta { .. })` entry to [`CATALOG`].
//!    The startup [`assert_parity`] check will panic without it.
//!
//! The three edits are enforced by two independent checks — one at compile
//! time, one at startup — so it is not possible to add a variant that ships
//! without a catalog entry.

use serde::{Deserialize, Serialize};

use super::protocol::DaemonState;

/// A discriminant-only mirror of [`DaemonState`]. One unit variant per
/// concrete `DaemonState` variant. Used as the catalog key and as the target
/// of the compile-time exhaustive match in [`kind_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DaemonStateKind {
  Starting,
  Evaluating,
  Ready,
  BuildFailed,
  WatcherDegraded,
  ConfigError,
  ShuttingDown,
  Pending,
  Missing,
  Degraded,
  Stuck,
  SelfHealing,
}

impl DaemonStateKind {
  /// Every variant, in declaration order. Kept in lockstep with the enum
  /// via [`assert_parity`] (a variant added here but forgotten from
  /// [`CATALOG`] panics at startup; a variant added to [`DaemonState`] but
  /// forgotten here fails to compile in [`kind_of`]).
  pub const ALL: &'static [DaemonStateKind] = &[
    DaemonStateKind::Starting,
    DaemonStateKind::Evaluating,
    DaemonStateKind::Ready,
    DaemonStateKind::BuildFailed,
    DaemonStateKind::WatcherDegraded,
    DaemonStateKind::ConfigError,
    DaemonStateKind::ShuttingDown,
    DaemonStateKind::Pending,
    DaemonStateKind::Missing,
    DaemonStateKind::Degraded,
    DaemonStateKind::Stuck,
    DaemonStateKind::SelfHealing,
  ];

  /// Stable string name used on the wire in `Query::StateCatalog`.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      DaemonStateKind::Starting => "Starting",
      DaemonStateKind::Evaluating => "Evaluating",
      DaemonStateKind::Ready => "Ready",
      DaemonStateKind::BuildFailed => "BuildFailed",
      DaemonStateKind::WatcherDegraded => "WatcherDegraded",
      DaemonStateKind::ConfigError => "ConfigError",
      DaemonStateKind::ShuttingDown => "ShuttingDown",
      DaemonStateKind::Pending => "Pending",
      DaemonStateKind::Missing => "Missing",
      DaemonStateKind::Degraded => "Degraded",
      DaemonStateKind::Stuck => "Stuck",
      DaemonStateKind::SelfHealing => "SelfHealing",
    }
  }
}

/// Extract the discriminant of a `DaemonState`.
///
/// **This is the compile-time parity check.** The `match` is intentionally
/// exhaustive without a wildcard arm; adding a variant to `DaemonState`
/// without adding an arm here fails to compile.
#[must_use]
pub const fn kind_of(state: &DaemonState) -> DaemonStateKind {
  match state {
    DaemonState::Starting => DaemonStateKind::Starting,
    DaemonState::Evaluating => DaemonStateKind::Evaluating,
    DaemonState::Ready => DaemonStateKind::Ready,
    DaemonState::BuildFailed { .. } => DaemonStateKind::BuildFailed,
    DaemonState::WatcherDegraded => DaemonStateKind::WatcherDegraded,
    DaemonState::ConfigError { .. } => DaemonStateKind::ConfigError,
    DaemonState::ShuttingDown => DaemonStateKind::ShuttingDown,
    DaemonState::Pending { .. } => DaemonStateKind::Pending,
    DaemonState::Missing => DaemonStateKind::Missing,
    DaemonState::Degraded { .. } => DaemonStateKind::Degraded,
    DaemonState::Stuck { .. } => DaemonStateKind::Stuck,
    DaemonState::SelfHealing => DaemonStateKind::SelfHealing,
  }
}

/// Semantic phase of a daemon state — used by status bars to pick an icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
  /// Lifecycle bring-up (`Starting`).
  Startup,
  /// Actively producing a value (`Evaluating`, `Pending`).
  Working,
  /// Steady, healthy (`Ready`).
  Steady,
  /// Serving with reduced guarantees (`WatcherDegraded`, `Degraded`, `Missing`).
  Degraded,
  /// Non-recoverable without user or restart (`BuildFailed`, `ConfigError`, `Stuck`).
  Failed,
  /// Lifecycle wind-down (`ShuttingDown`, `SelfHealing`).
  Shutdown,
}

/// Broad category grouping for filters and colour selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
  /// Green — steady and healthy.
  Ok,
  /// Yellow — busy but progressing.
  Busy,
  /// Yellow — serving degraded results.
  Warn,
  /// Red — needs attention.
  Error,
  /// Grey — lifecycle transition.
  Neutral,
}

/// Metadata for one `DaemonState` variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateMeta {
  /// Human-facing short name.
  pub display: &'static str,
  /// Palette hint (`"green"`, `"yellow"`, `"red"`, `"grey"`).
  pub color: &'static str,
  /// Broad category grouping.
  pub category: Category,
  /// Lifecycle phase.
  pub phase: Phase,
  /// Whether the state is terminal from the daemon's own lifecycle POV.
  pub terminal: bool,
}

/// The authoritative state catalog.
///
/// One entry per variant of [`DaemonStateKind`]. Order matches
/// `DaemonStateKind::ALL`. [`assert_parity`] verifies at startup that every
/// `DaemonStateKind` variant appears exactly once here.
pub static CATALOG: &[(DaemonStateKind, StateMeta)] = &[
  (
    DaemonStateKind::Starting,
    StateMeta {
      display: "Starting",
      color: "grey",
      category: Category::Neutral,
      phase: Phase::Startup,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::Evaluating,
    StateMeta {
      display: "Evaluating",
      color: "yellow",
      category: Category::Busy,
      phase: Phase::Working,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::Ready,
    StateMeta {
      display: "Ready",
      color: "green",
      category: Category::Ok,
      phase: Phase::Steady,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::BuildFailed,
    StateMeta {
      display: "Build failed",
      color: "red",
      category: Category::Error,
      phase: Phase::Failed,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::WatcherDegraded,
    StateMeta {
      display: "Watcher degraded",
      color: "yellow",
      category: Category::Warn,
      phase: Phase::Degraded,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::ConfigError,
    StateMeta {
      display: "Config error",
      color: "red",
      category: Category::Error,
      phase: Phase::Failed,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::ShuttingDown,
    StateMeta {
      display: "Shutting down",
      color: "grey",
      category: Category::Neutral,
      phase: Phase::Shutdown,
      terminal: true,
    },
  ),
  (
    DaemonStateKind::Pending,
    StateMeta {
      display: "Pending",
      color: "yellow",
      category: Category::Busy,
      phase: Phase::Working,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::Missing,
    StateMeta {
      display: "Missing",
      color: "grey",
      category: Category::Warn,
      phase: Phase::Degraded,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::Degraded,
    StateMeta {
      display: "Degraded",
      color: "yellow",
      category: Category::Warn,
      phase: Phase::Degraded,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::Stuck,
    StateMeta {
      display: "Stuck",
      color: "red",
      category: Category::Error,
      phase: Phase::Failed,
      terminal: false,
    },
  ),
  (
    DaemonStateKind::SelfHealing,
    StateMeta {
      display: "Self-healing",
      color: "yellow",
      category: Category::Busy,
      phase: Phase::Shutdown,
      terminal: false,
    },
  ),
];

/// Look up the metadata for a state kind.
///
/// Returns `None` only if the catalog is out of sync with [`DaemonStateKind`];
/// [`assert_parity`] rules this out at startup.
#[must_use]
pub fn meta_for(kind: DaemonStateKind) -> Option<&'static StateMeta> {
  CATALOG.iter().find_map(|(k, meta)| (*k == kind).then_some(meta))
}

/// Convenience: look up metadata directly for a `DaemonState`.
#[must_use]
pub fn meta_for_state(state: &DaemonState) -> Option<&'static StateMeta> {
  meta_for(kind_of(state))
}

/// Assert (at startup) that the catalog covers every `DaemonStateKind`
/// variant exactly once. This is the runtime half of the SC-016 parity
/// guarantee; the compile-time half is [`kind_of`]'s exhaustive `match`.
///
/// # Panics
///
/// Panics if any `DaemonStateKind` variant is missing from [`CATALOG`] or if
/// any kind appears more than once. This is called from daemon startup so a
/// broken catalog is caught before any client can observe it.
pub fn assert_parity() {
  // Every kind must appear at least once.
  for kind in DaemonStateKind::ALL {
    assert!(
      CATALOG.iter().any(|(k, _)| k == kind),
      "state_meta parity failure: DaemonStateKind::{:?} is missing from CATALOG. \
       Add an entry in state_meta.rs.",
      kind,
    );
  }

  // No kind may appear more than once, and the catalog must not contain
  // spurious kinds (impossible today, but defends against future edits).
  assert_eq!(
    CATALOG.len(),
    DaemonStateKind::ALL.len(),
    "state_meta parity failure: CATALOG has {} entries but there are {} \
     DaemonStateKind variants. Ensure exactly one CATALOG entry per variant.",
    CATALOG.len(),
    DaemonStateKind::ALL.len(),
  );

  for kind in DaemonStateKind::ALL {
    let occurrences = CATALOG.iter().filter(|(k, _)| k == kind).count();
    assert_eq!(
      occurrences, 1,
      "state_meta parity failure: DaemonStateKind::{:?} appears {} times in \
       CATALOG (expected exactly 1).",
      kind, occurrences,
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parity_holds() {
    // Same guard the daemon calls at startup — must pass for the shipped
    // catalog.
    assert_parity();
  }

  #[test]
  fn every_kind_has_meta() {
    for kind in DaemonStateKind::ALL {
      assert!(
        meta_for(*kind).is_some(),
        "no meta for {:?}",
        kind,
      );
    }
  }

  #[test]
  fn kind_of_covers_all_variants() {
    // Sample one instance per DaemonState variant to prove kind_of maps
    // each of them to a distinct DaemonStateKind. Adding a new DaemonState
    // variant without extending kind_of already fails to compile; this test
    // guards against regressions where two variants collapse to the same
    // kind by accident.
    let samples: &[(DaemonState, DaemonStateKind)] = &[
      (DaemonState::Starting, DaemonStateKind::Starting),
      (DaemonState::Evaluating, DaemonStateKind::Evaluating),
      (DaemonState::Ready, DaemonStateKind::Ready),
      (
        DaemonState::BuildFailed {
          error: String::new(),
          retry_count: 0,
        },
        DaemonStateKind::BuildFailed,
      ),
      (DaemonState::WatcherDegraded, DaemonStateKind::WatcherDegraded),
      (
        DaemonState::ConfigError {
          error: String::new(),
        },
        DaemonStateKind::ConfigError,
      ),
      (DaemonState::ShuttingDown, DaemonStateKind::ShuttingDown),
      (
        DaemonState::Pending {
          job_id: String::new(),
        },
        DaemonStateKind::Pending,
      ),
      (DaemonState::Missing, DaemonStateKind::Missing),
      (
        DaemonState::Degraded {
          reason: String::new(),
        },
        DaemonStateKind::Degraded,
      ),
      (
        DaemonState::Stuck {
          job_id: String::new(),
          stuck_ms: 0,
        },
        DaemonStateKind::Stuck,
      ),
      (DaemonState::SelfHealing, DaemonStateKind::SelfHealing),
    ];

    assert_eq!(
      samples.len(),
      DaemonStateKind::ALL.len(),
      "test samples must cover every DaemonStateKind variant",
    );

    for (state, expected) in samples {
      assert_eq!(kind_of(state), *expected, "kind mismatch for {:?}", state);
    }
  }

  #[test]
  fn kind_as_str_is_stable_and_unique() {
    let mut seen = std::collections::HashSet::new();
    for kind in DaemonStateKind::ALL {
      assert!(
        seen.insert(kind.as_str()),
        "duplicate wire name for {:?}",
        kind,
      );
    }
  }
}
