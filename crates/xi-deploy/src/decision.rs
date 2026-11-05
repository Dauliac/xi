//! The cwd-independent tree `xi deploy` resolves to pick between a
//! local activation, a fleet rollout, or a refusal.
//!
//! Every input here is a fact some other, already-landed capability
//! resolves on its own: target disambiguation (`classify_positional`
//! and its `--target` override), mechanism/scope detection
//! (`SystemDeploymentKind::detect`), dirty-tree and adoption checks,
//! and inventory/flake-output discovery. This module never reads the
//! filesystem, the network, or `cwd` itself - every leaf in
//! `xi-deploy-unified-verb/spec.md`'s first requirement is a plain
//! unit test over a [`Facts`] value, including the regression that
//! requirement's first scenario exists to guard: a standalone,
//! already-adopted host with no fleet configured resolves to a local
//! activation no matter where `xi deploy` is run from, because `cwd`
//! never enters this function at all.

/// Every already-resolved fact the tree reads, in the order its own
/// requirement lists them: target, inventory-membership, adoption,
/// dirty, fleet-existence, digest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
  /// An explicit target list: a positional already disambiguated as a
  /// fleet name, or `--target`. Non-empty always wins over every
  /// later step.
  pub requested_targets:            Vec<String>,
  /// This host's own machine-id matches an inventory entry: it is
  /// itself a registered fleet member, so it activates locally
  /// regardless of adoption, dirtiness, or inventory size.
  pub host_is_fleet_member:         bool,
  /// The scope being resolved has an adopted source at all.
  pub scope_adopted:                bool,
  /// The scope's relevant tree (working dir if configured, sources
  /// dir otherwise) is dirty.
  pub tree_dirty:                   bool,
  /// Any inventory entries are configured for this scope.
  pub inventory_configured:         bool,
  /// The flake declares any deploy-rs/colmena/nixosConfigurations
  /// outputs at all, independently of the inventory.
  pub flake_deploy_outputs_present: bool,
  /// The cached `fleet-digest` (resolved target set, backend, and
  /// build-location decisions) still matches current state.
  pub fleet_digest_unchanged:       bool,
}

/// The tree's outcome: what `xi deploy` should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
  /// Activate this host directly, dispatching to the same
  /// `switch::prepare`/`record_activation_at` path `switch` already
  /// calls.
  ActivateLocal(LocalReason),
  /// Roll out to the fleet, restricted to `targets` when non-empty
  /// (an explicit target list) or the whole discovered fleet
  /// otherwise.
  RolloutFleet {
    /// Explicit targets to restrict the rollout to; empty means every
    /// discovered target.
    targets:             Vec<String>,
    /// Whether the cached `fleet-digest` matched, so discovery and
    /// selection can be skipped and the cached decisions reused
    /// directly.
    reuse_cached_digest: bool,
  },
  /// Refuse: there is nothing resolvable to activate or roll out to.
  Refuse(RefuseReason),
}

/// Why the tree chose a local activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalReason {
  /// This host's own machine-id is itself an inventory entry.
  FleetMember,
  /// The relevant tree is dirty; untested changes activate locally
  /// before anything fans out, even when a fleet is configured.
  Dirty,
  /// Clean, adopted, not a fleet member, and no inventory or
  /// flake-deploy outputs at all - there is nothing to fan out to.
  NothingToFanOutTo,
  /// `--local`/`--here` forced this outcome, overriding whatever
  /// [`resolve`] would have picked on its own.
  Forced,
}

/// Why the tree refused to resolve a decision at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
  /// The scope has no adopted source.
  NotAdopted,
}

/// Resolve [`Facts`] into a [`Decision`], in the order
/// `xi-deploy-unified-verb/spec.md`'s first requirement lists: target,
/// inventory-membership, adoption, dirty, fleet-existence, digest.
///
/// `cwd` never appears in [`Facts`] and this function never reads it:
/// that is what makes the result identical no matter where `xi
/// deploy` is invoked from.
#[must_use]
pub fn resolve(facts: &Facts) -> Decision {
  if !facts.requested_targets.is_empty() {
    return Decision::RolloutFleet {
      targets:             facts.requested_targets.clone(),
      reuse_cached_digest: false,
    };
  }

  if facts.host_is_fleet_member {
    return Decision::ActivateLocal(LocalReason::FleetMember);
  }

  if !facts.scope_adopted {
    return Decision::Refuse(RefuseReason::NotAdopted);
  }

  if facts.tree_dirty {
    return Decision::ActivateLocal(LocalReason::Dirty);
  }

  if !facts.inventory_configured && !facts.flake_deploy_outputs_present {
    return Decision::ActivateLocal(LocalReason::NothingToFanOutTo);
  }

  Decision::RolloutFleet {
    targets:             Vec::new(),
    reuse_cached_digest: facts.fleet_digest_unchanged,
  }
}

/// Apply `--local`/`--here` and `--fleet`'s explicit overrides to
/// [`resolve`]'s own outcome.
///
/// Layered on top of `resolve` rather than folded into it, matching
/// this change's layering elsewhere (`classify_deploy_positional` over
/// `classify_positional`, the cwd-to-scope step over the tree itself):
/// `resolve`'s own leaf tests stay exactly as they are, cwd-free and
/// override-free.
///
/// A [`Decision::Refuse`] is left untouched by either flag: with no
/// adopted source there is nothing to activate or roll out to, so
/// forcing a destination has no effect. Otherwise `local` wins if
/// both are somehow set true - the CLI's `ArgGroup` is what should
/// actually stop that combination from parsing in the first place.
#[must_use]
pub fn apply_override(decision: Decision, local: bool, fleet: bool) -> Decision {
  if matches!(decision, Decision::Refuse(_)) {
    return decision;
  }

  if local {
    return Decision::ActivateLocal(LocalReason::Forced);
  }

  if fleet && matches!(decision, Decision::ActivateLocal(_)) {
    return Decision::RolloutFleet {
      targets:             Vec::new(),
      reuse_cached_digest: false,
    };
  }

  decision
}

#[cfg(test)]
mod tests {
  use super::*;

  fn adopted_standalone() -> Facts {
    Facts {
      scope_adopted: true,
      ..Facts::default()
    }
  }

  /// Regression test for the bug the corrected tree fixed: this
  /// decision carries no `cwd` input at all, so it is identical
  /// wherever `xi deploy` runs from.
  #[test]
  fn standalone_adopted_host_activates_locally_from_anywhere() {
    let facts = adopted_standalone();
    assert_eq!(
      resolve(&facts),
      Decision::ActivateLocal(LocalReason::NothingToFanOutTo)
    );
  }

  #[test]
  fn dirty_tree_activates_locally_even_with_an_inventory_configured() {
    let facts = Facts {
      scope_adopted: true,
      tree_dirty: true,
      inventory_configured: true,
      ..Facts::default()
    };
    assert_eq!(resolve(&facts), Decision::ActivateLocal(LocalReason::Dirty));
  }

  #[test]
  fn an_explicit_target_wins_over_every_other_fact() {
    let facts = Facts {
      requested_targets: vec!["web1".to_owned()],
      scope_adopted: false,
      tree_dirty: true,
      host_is_fleet_member: true,
      ..Facts::default()
    };
    assert_eq!(resolve(&facts), Decision::RolloutFleet {
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
    assert_eq!(
      resolve(&facts),
      Decision::ActivateLocal(LocalReason::FleetMember)
    );
  }

  #[test]
  fn an_unadopted_non_member_scope_refuses() {
    let facts = Facts::default();
    assert_eq!(resolve(&facts), Decision::Refuse(RefuseReason::NotAdopted));
  }

  #[test]
  fn flake_deploy_outputs_alone_route_to_the_fleet_without_an_inventory() {
    let facts = Facts {
      scope_adopted: true,
      flake_deploy_outputs_present: true,
      ..Facts::default()
    };
    assert_eq!(resolve(&facts), Decision::RolloutFleet {
      targets:             Vec::new(),
      reuse_cached_digest: false,
    });
  }

  #[test]
  fn an_unchanged_fleet_digest_skips_reevaluation() {
    let facts = Facts {
      scope_adopted: true,
      inventory_configured: true,
      fleet_digest_unchanged: true,
      ..Facts::default()
    };
    assert_eq!(resolve(&facts), Decision::RolloutFleet {
      targets:             Vec::new(),
      reuse_cached_digest: true,
    });
  }

  #[test]
  fn a_changed_fleet_digest_forces_reevaluation() {
    let facts = Facts {
      scope_adopted: true,
      inventory_configured: true,
      fleet_digest_unchanged: false,
      ..Facts::default()
    };
    assert_eq!(resolve(&facts), Decision::RolloutFleet {
      targets:             Vec::new(),
      reuse_cached_digest: false,
    });
  }
}
