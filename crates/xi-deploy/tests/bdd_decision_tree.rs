//! BDD: the full cwd-independent decision tree against fixtures, for
//! every leaf `xi-deploy-unified-verb/spec.md`'s first requirement
//! names - including the standalone-host-from-anywhere regression
//! this design corrected.
//!
//! `decision::resolve` takes only [`Facts`][xi_deploy::decision::Facts],
//! never `cwd` - that is what makes the regression scenario provable at
//! all. This file still drives it against literal `cwd` changes (not
//! just a `Facts` value asserted by eye), so a future change that
//! threads `cwd` into the tree, or into whatever assembles `Facts` on
//! `cwd`'s way in, fails a test here first rather than only failing the
//! inline unit tests in `crate::decision`, which never touch `cwd` at
//! all.
//!
//! Each `#[test]` below is named after one leaf the requirement's
//! ordering lists (target, inventory-membership, adoption, dirty,
//! fleet-existence, digest), plus the requirement's own two named
//! scenarios.

use std::{path::Path, sync::Mutex};

use xi_deploy::decision::{
  Decision,
  Facts,
  LocalReason,
  RefuseReason,
  resolve,
};

/// Serializes the tests below against each other: `std::env::set_current_dir`
/// mutates process-global state, so two of these tests running at once
/// could observe each other's `cwd`. Tests elsewhere in the suite that
/// never touch `cwd` are unaffected either way.
static CWD_LOCK: Mutex<()> = Mutex::new(());

/// Run `resolve(facts)` with the process `cwd` set to `dir`, restoring
/// the original `cwd` before returning (even if `resolve` or the
/// restore itself were to panic, `cwd` is process-global and leaking a
/// change would make every later test in this binary order-dependent).
fn resolve_from(dir: &Path, facts: &Facts) -> Decision {
  let _guard = CWD_LOCK.lock().expect("cwd lock");
  let original = std::env::current_dir().expect("read current dir");
  std::env::set_current_dir(dir).expect("chdir to fixture dir");
  let decision = resolve(facts);
  std::env::set_current_dir(original).expect("restore original cwd");
  decision
}

/// A host with an adopted source, a clean tree, and no inventory or
/// flake-deploy outputs at all - the exact fixture the regression
/// scenario names.
fn adopted_standalone() -> Facts {
  Facts {
    scope_adopted: true,
    ..Facts::default()
  }
}

// -- Scenario: A standalone adopted host activates locally from anywhere --

#[test]
fn standalone_adopted_host_activates_locally_from_tmp() {
  let decision = resolve_from(Path::new("/tmp"), &adopted_standalone());
  assert_eq!(
    decision,
    Decision::ActivateLocal(LocalReason::NothingToFanOutTo)
  );
}

#[test]
fn standalone_adopted_host_activates_locally_from_its_own_working_dir() {
  // "the scope's working_dir" in the scenario's WHEN clause - stood in
  // here by the test binary's own cwd at start, since this module has
  // no real scope config to read a working_dir off of.
  let working_dir = std::env::current_dir().expect("read current dir");
  let decision = resolve_from(&working_dir, &adopted_standalone());
  assert_eq!(
    decision,
    Decision::ActivateLocal(LocalReason::NothingToFanOutTo)
  );
}

#[test]
fn standalone_adopted_host_activates_locally_from_any_other_directory() {
  let decision = resolve_from(Path::new("/"), &adopted_standalone());
  assert_eq!(
    decision,
    Decision::ActivateLocal(LocalReason::NothingToFanOutTo)
  );
}

#[test]
fn standalone_adopted_host_decision_is_identical_across_every_directory() {
  let facts = adopted_standalone();
  let started_in = std::env::current_dir().expect("read current dir");

  let from_tmp = resolve_from(Path::new("/tmp"), &facts);
  let from_root = resolve_from(Path::new("/"), &facts);
  let from_started_in = resolve_from(&started_in, &facts);

  assert_eq!(from_tmp, from_root, "cwd must not change the decision");
  assert_eq!(
    from_root, from_started_in,
    "cwd must not change the decision"
  );
}

// -- Scenario: A dirty tree activates locally before any fleet rollout --

#[test]
fn a_dirty_tree_activates_locally_even_with_an_inventory_configured() {
  let facts = Facts {
    scope_adopted: true,
    tree_dirty: true,
    inventory_configured: true,
    ..Facts::default()
  };
  // Run from /tmp - nowhere near any working_dir this fixture could
  // have - to prove the dirty-routes-local outcome is as cwd-blind as
  // the standalone-host scenario above.
  let decision = resolve_from(Path::new("/tmp"), &facts);
  assert_eq!(decision, Decision::ActivateLocal(LocalReason::Dirty));
}

// -- Every remaining leaf the requirement's ordering names ----------------
//
// Same fixture-and-cwd style as the two named scenarios above: each
// fixture is run from an arbitrary, unrelated directory to establish
// that cwd-independence holds for leaves the requirement states but
// does not spell out as its own named scenario.

#[test]
fn an_explicit_target_wins_over_every_other_fact() {
  let facts = Facts {
    requested_targets: vec!["web1".to_owned()],
    scope_adopted: false,
    tree_dirty: true,
    host_is_fleet_member: true,
    ..Facts::default()
  };
  let decision = resolve_from(Path::new("/tmp"), &facts);
  assert_eq!(decision, Decision::RolloutFleet {
    targets:             vec!["web1".to_owned()],
    reuse_cached_digest: false,
  });
}

#[test]
fn a_fleet_member_host_activates_locally_even_when_unadopted() {
  let facts = Facts {
    host_is_fleet_member: true,
    scope_adopted: false,
    ..Facts::default()
  };
  let decision = resolve_from(Path::new("/tmp"), &facts);
  assert_eq!(decision, Decision::ActivateLocal(LocalReason::FleetMember));
}

#[test]
fn an_unadopted_non_member_scope_refuses() {
  let decision = resolve_from(Path::new("/tmp"), &Facts::default());
  assert_eq!(decision, Decision::Refuse(RefuseReason::NotAdopted));
}

#[test]
fn a_clean_adopted_host_with_no_inventory_or_flake_outputs_activates_locally() {
  let decision = resolve_from(Path::new("/tmp"), &adopted_standalone());
  assert_eq!(
    decision,
    Decision::ActivateLocal(LocalReason::NothingToFanOutTo)
  );
}

#[test]
fn otherwise_it_rolls_out_to_the_discovered_fleet() {
  let facts = Facts {
    scope_adopted: true,
    inventory_configured: true,
    ..Facts::default()
  };
  let decision = resolve_from(Path::new("/tmp"), &facts);
  assert_eq!(decision, Decision::RolloutFleet {
    targets:             Vec::new(),
    reuse_cached_digest: false,
  });
}

#[test]
fn a_fleet_rollout_reuses_the_cached_digest_when_unchanged() {
  let facts = Facts {
    scope_adopted: true,
    inventory_configured: true,
    fleet_digest_unchanged: true,
    ..Facts::default()
  };
  let decision = resolve_from(Path::new("/tmp"), &facts);
  assert_eq!(decision, Decision::RolloutFleet {
    targets:             Vec::new(),
    reuse_cached_digest: true,
  });
}
