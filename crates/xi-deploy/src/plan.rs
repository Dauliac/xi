//! The resolved plan `xi deploy --dry` prints instead of dispatching.
//!
//! Mirrors `decision`'s own design: everything here is a pure function
//! over already-resolved facts and the parsed [`DeployArgs`], so `--dry`
//! is unit-testable without touching the filesystem, the network, or a
//! real dispatcher. [`run_or_preview`] is the one seam `--dry` actually
//! changes: it is the sole place a caller's dispatch closure - the
//! build/copy/activate call `xi-deploy-unified-verb/spec.md`'s "`--dry`
//! previews without dispatching" scenario forbids - can run at all.

use std::collections::BTreeSet;

use color_eyre::Result;
use xi_core::sources::{
  config::{DeployPositionalKind, classify_deploy_positional},
  deployment::SystemDeploymentKind,
};

use crate::{
  decision::{Decision, Facts, apply_override, resolve},
  push::args::{PushBackend, PushMode},
  unified_args::{DeployArgs, DeployMechanism, DeployScope},
};

/// The fully resolved plan `--dry` prints: scope, mechanism, the
/// decision tree's own outcome (which already carries the resolved
/// target set), backend, and build strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlan {
  /// Which half of the machine this plan governs.
  pub scope:      DeployScope,
  /// The activation mechanism, or `None` when host detection found none
  /// of the known markers.
  pub mechanism:  Option<DeployMechanism>,
  /// Local activation, fleet rollout (with its own target set), or
  /// refusal - the decision tree's own outcome.
  pub decision:   Decision,
  /// Which deployment backend a fleet rollout would use.
  pub backend:    PushBackend,
  /// Evaluate-and-copy, or sync-and-build-remotely.
  pub build_mode: PushMode,
}

/// Which positionals and `--target` occurrences resolve to explicit
/// fleet targets, per `xi-deploy-unified-verb/spec.md`'s
/// positional-disambiguation requirement: `--target` always wins; two
/// or more positionals are always a target list; a single positional
/// is a target only when it matches `known_targets`, falling through
/// to "no explicit target" (so the decision tree's own default route
/// applies) otherwise.
#[must_use]
pub fn resolve_requested_targets(
  args: &DeployArgs,
  known_targets: &BTreeSet<String>,
) -> Vec<String> {
  if !args.target.is_empty() {
    return args.target.clone();
  }
  if args.targets.len() >= 2 {
    return args.targets.clone();
  }
  if let [one] = args.targets.as_slice()
    && matches!(
      classify_deploy_positional(one, known_targets),
      DeployPositionalKind::Target
    )
  {
    return vec![one.clone()];
  }
  Vec::new()
}

/// The build strategy `--mode`/`--remote-build` resolve to, matching
/// [`crate::push::args::PushArgs::effective_mode`]'s own ladder rung:
/// an explicit `--mode` wins outright, otherwise `--remote-build`
/// decides.
#[must_use]
pub const fn resolve_build_mode(args: &DeployArgs) -> PushMode {
  match args.mode {
    Some(mode) => mode,
    None if matches!(args.remote_build, Some(true)) => PushMode::Sources,
    None => PushMode::Closure,
  }
}

/// `--mechanism`, or a best-effort default off host markers when unset.
#[must_use]
pub fn resolve_mechanism(args: &DeployArgs) -> Option<DeployMechanism> {
  if let Some(mechanism) = args.mechanism {
    return Some(mechanism);
  }
  match SystemDeploymentKind::detect() {
    SystemDeploymentKind::NixOs => Some(DeployMechanism::Nixos),
    SystemDeploymentKind::Darwin => Some(DeployMechanism::Darwin),
    SystemDeploymentKind::SystemManager => Some(DeployMechanism::SystemManager),
    SystemDeploymentKind::None => None,
  }
}

/// Resolve `args` and `facts` into the full plan `--dry` prints.
///
/// `facts.requested_targets` is overwritten here from `args` and
/// `known_targets`; callers only need to fill in the host-state facts
/// the CLI itself cannot answer (fleet membership, adoption,
/// dirtiness, inventory/flake-output presence, cached-digest
/// freshness - see [`Facts`]).
#[must_use]
pub fn resolve_plan(
  args: &DeployArgs,
  facts: &Facts,
  known_targets: &BTreeSet<String>,
  scope: DeployScope,
) -> DeployPlan {
  let facts = Facts {
    requested_targets: resolve_requested_targets(args, known_targets),
    ..facts.clone()
  };
  let decision = apply_override(resolve(&facts), args.local, args.fleet);
  DeployPlan {
    scope,
    mechanism: resolve_mechanism(args),
    decision,
    backend: args.backend,
    build_mode: resolve_build_mode(args),
  }
}

/// Render `plan` as `--dry`'s human-readable preview.
#[must_use]
pub fn format_plan(plan: &DeployPlan) -> String {
  let mechanism = plan
    .mechanism
    .map_or_else(|| "unknown".to_owned(), |m| format!("{m:?}"));
  let destination = match &plan.decision {
    Decision::ActivateLocal(reason) => format!("activate locally ({reason:?})"),
    Decision::RolloutFleet {
      targets,
      reuse_cached_digest,
    } if targets.is_empty() => {
      format!(
        "roll out to the discovered fleet (cached digest reused: \
         {reuse_cached_digest})"
      )
    },
    Decision::RolloutFleet {
      targets,
      reuse_cached_digest,
    } => {
      format!(
        "roll out to {} (cached digest reused: {reuse_cached_digest})",
        targets.join(", ")
      )
    },
    Decision::Refuse(reason) => format!("refuse ({reason:?})"),
  };
  format!(
    "scope: {:?}\nmechanism: {mechanism}\ndestination: \
     {destination}\nbackend: {}\nbuild strategy: {:?}",
    plan.scope,
    plan.backend.as_str(),
    plan.build_mode,
  )
}

/// Print `plan` to stdout - `--dry`'s whole visible effect.
pub fn print_plan(plan: &DeployPlan) {
  println!("{}", format_plan(plan));
}

/// Run `dispatch` unless `args.dry`, in which case `plan` is printed
/// and `dispatch` never runs at all.
///
/// This is the one seam `--dry` touches: plan resolution and printing
/// happen unconditionally so the preview is always accurate; `dispatch`
/// is the only side-effecting step it must skip.
///
/// # Errors
///
/// Propagates whatever error `dispatch` returns; `--dry` itself never
/// fails.
pub fn run_or_preview(
  args: &DeployArgs,
  plan: &DeployPlan,
  dispatch: impl FnOnce() -> Result<()>,
) -> Result<()> {
  if args.dry {
    print_plan(plan);
    return Ok(());
  }
  dispatch()
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;

  use clap::Parser;

  use super::*;

  #[derive(Debug, clap::Parser)]
  struct TestCli {
    #[command(flatten)]
    args: DeployArgs,
  }

  fn parse(args: &[&str]) -> DeployArgs {
    TestCli::try_parse_from(std::iter::once("xi").chain(args.iter().copied()))
      .expect("parses")
      .args
  }

  fn adopted_with_inventory() -> Facts {
    Facts {
      scope_adopted: true,
      inventory_configured: true,
      ..Facts::default()
    }
  }

  // -- resolve_requested_targets -----------------------------------------

  #[test]
  fn explicit_target_flag_wins_over_positionals() {
    let args = parse(&["--target", "web1", "ignored-positional"]);
    assert_eq!(resolve_requested_targets(&args, &BTreeSet::new()), vec![
      "web1".to_owned()
    ]);
  }

  #[test]
  fn two_or_more_positionals_are_always_a_target_list() {
    let args = parse(&["web1", "web2"]);
    assert_eq!(resolve_requested_targets(&args, &BTreeSet::new()), vec![
      "web1".to_owned(),
      "web2".to_owned()
    ]);
  }

  #[test]
  fn a_single_positional_matching_a_known_target_resolves_as_one() {
    let args = parse(&["web1"]);
    let known = BTreeSet::from(["web1".to_owned()]);
    assert_eq!(resolve_requested_targets(&args, &known), vec![
      "web1".to_owned()
    ]);
  }

  #[test]
  fn a_single_positional_matching_nothing_resolves_no_explicit_target() {
    let args = parse(&["./dotfiles"]);
    assert_eq!(
      resolve_requested_targets(&args, &BTreeSet::new()),
      Vec::<String>::new()
    );
  }

  // -- resolve_build_mode --------------------------------------------------

  #[test]
  fn build_mode_defaults_to_closure() {
    assert_eq!(resolve_build_mode(&parse(&[])), PushMode::Closure);
  }

  #[test]
  fn explicit_mode_wins_over_remote_build() {
    let args = parse(&["--mode", "closure", "--remote-build", "true"]);
    assert_eq!(resolve_build_mode(&args), PushMode::Closure);
  }

  #[test]
  fn remote_build_true_resolves_sources_with_no_explicit_mode() {
    let args = parse(&["--remote-build", "true"]);
    assert_eq!(resolve_build_mode(&args), PushMode::Sources);
  }

  // -- resolve_mechanism ----------------------------------------------------

  #[test]
  fn explicit_mechanism_flag_is_never_overridden_by_detection() {
    let args = parse(&["--mechanism", "darwin"]);
    assert_eq!(resolve_mechanism(&args), Some(DeployMechanism::Darwin));
  }

  // -- run_or_preview: the actual --dry short-circuit ----------------------

  #[test]
  fn dry_prints_the_plan_and_never_calls_dispatch() {
    let args = parse(&["--dry", "--mechanism", "nixos"]);
    let plan = resolve_plan(
      &args,
      &adopted_with_inventory(),
      &BTreeSet::new(),
      DeployScope::System,
    );
    assert_eq!(plan.decision, Decision::RolloutFleet {
      targets:             Vec::new(),
      reuse_cached_digest: false,
    });

    let dispatched = Cell::new(false);
    let result = run_or_preview(&args, &plan, || {
      dispatched.set(true);
      Ok(())
    });

    assert!(result.is_ok());
    assert!(
      !dispatched.get(),
      "--dry must never run the build/copy/activate dispatch closure"
    );
  }

  #[test]
  fn without_dry_the_dispatch_closure_runs() {
    let args = parse(&["--mechanism", "nixos"]);
    let plan = resolve_plan(
      &args,
      &adopted_with_inventory(),
      &BTreeSet::new(),
      DeployScope::System,
    );

    let dispatched = Cell::new(false);
    let result = run_or_preview(&args, &plan, || {
      dispatched.set(true);
      Ok(())
    });

    assert!(result.is_ok());
    assert!(dispatched.get(), "dispatch must run when --dry is not set");
  }

  // -- format_plan: the plan actually gets printed ------------------------

  #[test]
  fn format_plan_names_every_resolved_field() {
    let args =
      parse(&["--dry", "--mechanism", "nixos", "--backend", "colmena"]);
    let plan = resolve_plan(
      &args,
      &adopted_with_inventory(),
      &BTreeSet::new(),
      DeployScope::User,
    );
    let rendered = format_plan(&plan);
    assert!(rendered.contains("scope: User"), "got: {rendered}");
    assert!(rendered.contains("mechanism: Nixos"), "got: {rendered}");
    assert!(rendered.contains("destination:"), "got: {rendered}");
    assert!(rendered.contains("backend: colmena"), "got: {rendered}");
    assert!(rendered.contains("build strategy:"), "got: {rendered}");
  }
}
