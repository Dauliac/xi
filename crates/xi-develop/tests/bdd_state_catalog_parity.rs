//! BDD: async-daemon-cqrs / state catalog parity (MVP task 4.6).
//!
//! Enforces the spec clause `Daemon — state catalog query`:
//!
//! > A new `DaemonState` variant added without extending the catalog MUST
//! > fail at build time or startup rather than serve an incomplete
//! > catalog (SC-016).
//!
//! `state_meta::assert_parity` panics when a variant is missing from
//! `CATALOG`. This suite calls it and also spot-checks that every
//! `DaemonState` variant round-trips through `meta_for_state`.

use xi_develop::daemon::protocol::DaemonState;
use xi_develop::daemon::state_meta::{
  CATALOG, DaemonStateKind, assert_parity, meta_for, meta_for_state,
};

#[test]
fn assert_parity_passes_at_startup() {
  // Panics if any `DaemonStateKind` variant is missing from `CATALOG` or
  // if `CATALOG` has duplicates.
  assert_parity();
}

#[test]
fn every_kind_has_exactly_one_catalog_entry() {
  use DaemonStateKind as K;
  let all = [
    K::Starting,
    K::Evaluating,
    K::Ready,
    K::BuildFailed,
    K::WatcherDegraded,
    K::ConfigError,
    K::ShuttingDown,
    K::Pending,
    K::Missing,
    K::Degraded,
    K::Stuck,
    K::SelfHealing,
  ];
  for kind in all {
    assert!(
      meta_for(kind).is_some(),
      "CATALOG is missing DaemonStateKind::{kind:?}"
    );
  }
  assert_eq!(
    CATALOG.len(),
    all.len(),
    "CATALOG has {} entries; enum has {}",
    CATALOG.len(),
    all.len()
  );
}

#[test]
fn every_daemon_state_variant_resolves_to_meta() {
  let variants = [
    DaemonState::Starting,
    DaemonState::Evaluating,
    DaemonState::Ready,
    DaemonState::BuildFailed {
      error:       "x".into(),
      retry_count: 0,
    },
    DaemonState::WatcherDegraded,
    DaemonState::ConfigError {
      error: "x".into(),
    },
    DaemonState::ShuttingDown,
    DaemonState::Pending {
      job_id: "j".into(),
    },
    DaemonState::Missing,
    DaemonState::Degraded {
      reason: "x".into(),
    },
    DaemonState::Stuck {
      job_id:   "j".into(),
      stuck_ms: 0,
    },
    DaemonState::SelfHealing,
  ];
  for s in &variants {
    assert!(
      meta_for_state(s).is_some(),
      "meta_for_state returned None for {s:?}"
    );
  }
}

#[test]
fn terminal_flag_matches_only_shutting_down() {
  let terminals: Vec<_> = CATALOG
    .iter()
    .filter(|(_, m)| m.terminal)
    .map(|(k, _)| *k)
    .collect();
  assert_eq!(
    terminals,
    vec![DaemonStateKind::ShuttingDown],
    "only ShuttingDown should be terminal"
  );
}
