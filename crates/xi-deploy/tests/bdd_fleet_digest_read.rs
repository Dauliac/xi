//! BDD: the decision tree's fleet-existence/digest step reads
//! `fleet-digest` back (xi-fxgx.4.3), on top of 4.1's cache shape and
//! 4.2's write.
//!
//! `decision::resolve` itself stays filesystem-free and only sees the
//! already-resolved `Facts::fleet_digest_unchanged` bool  -  these
//! tests drive the real seam a future rollout caller uses to produce
//! that bool: [`fleet_digest::resolve_fleet_digest_unchanged`] against
//! a real cache file on disk, then [`decision::resolve`] against the
//! `Facts` it feeds into.

use xi_deploy::{
  decision::{Decision, Facts, resolve},
  fleet_digest::{FleetDigestInput, resolve_fleet_digest_unchanged, store},
};

fn adopted_with_inventory(fleet_digest_unchanged: bool) -> Facts {
  Facts {
    scope_adopted: true,
    inventory_configured: true,
    fleet_digest_unchanged,
    ..Facts::default()
  }
}

#[test]
fn an_unchanged_inventory_reuses_the_cached_digest_and_skips_reevaluation() {
  let dir = tempfile::tempdir().expect("tempdir");
  let xi_toml = dir.path().join(".xi.toml");
  store(&xi_toml, &FleetDigestInput {
    targets:        vec!["web1".to_owned(), "web2".to_owned()],
    backend:        "deploy-rs".to_owned(),
    build_location: "remote".to_owned(),
  })
  .expect("store");

  // The same rollout re-run against the same, unchanged inventory.
  let unchanged = resolve_fleet_digest_unchanged(
    &xi_toml,
    &["web1".to_owned(), "web2".to_owned()],
    "deploy-rs",
    "remote",
  );
  assert!(unchanged, "an unchanged inventory must be a cache hit");

  let decision = resolve(&adopted_with_inventory(unchanged));
  assert_eq!(decision, Decision::RolloutFleet {
    targets:             Vec::new(),
    reuse_cached_digest: true,
  });
}

#[test]
fn a_changed_inventory_invalidates_the_cached_digest_and_forces_reevaluation() {
  let dir = tempfile::tempdir().expect("tempdir");
  let xi_toml = dir.path().join(".xi.toml");
  store(&xi_toml, &FleetDigestInput {
    targets:        vec!["web1".to_owned()],
    backend:        "deploy-rs".to_owned(),
    build_location: "remote".to_owned(),
  })
  .expect("store");

  // A target was added to the inventory since the last cached rollout.
  let unchanged = resolve_fleet_digest_unchanged(
    &xi_toml,
    &["web1".to_owned(), "web2".to_owned()],
    "deploy-rs",
    "remote",
  );
  assert!(!unchanged, "a changed inventory must be a cache miss");

  let decision = resolve(&adopted_with_inventory(unchanged));
  assert_eq!(decision, Decision::RolloutFleet {
    targets:             Vec::new(),
    reuse_cached_digest: false,
  });
}

#[test]
fn a_changed_backend_also_invalidates_the_cached_digest() {
  let dir = tempfile::tempdir().expect("tempdir");
  let xi_toml = dir.path().join(".xi.toml");
  store(&xi_toml, &FleetDigestInput {
    targets:        vec!["web1".to_owned()],
    backend:        "deploy-rs".to_owned(),
    build_location: "remote".to_owned(),
  })
  .expect("store");

  let unchanged = resolve_fleet_digest_unchanged(
    &xi_toml,
    &["web1".to_owned()],
    "colmena",
    "remote",
  );
  assert!(!unchanged, "a changed backend must be a cache miss");

  let decision = resolve(&adopted_with_inventory(unchanged));
  assert_eq!(decision, Decision::RolloutFleet {
    targets:             Vec::new(),
    reuse_cached_digest: false,
  });
}

#[test]
fn no_prior_rollout_at_all_is_a_cache_miss() {
  let dir = tempfile::tempdir().expect("tempdir");
  let xi_toml = dir.path().join(".xi.toml");

  let unchanged = resolve_fleet_digest_unchanged(
    &xi_toml,
    &["web1".to_owned()],
    "deploy-rs",
    "remote",
  );
  assert!(!unchanged, "no cache file at all must be a miss, not a hit");

  let decision = resolve(&adopted_with_inventory(unchanged));
  assert_eq!(decision, Decision::RolloutFleet {
    targets:             Vec::new(),
    reuse_cached_digest: false,
  });
}
