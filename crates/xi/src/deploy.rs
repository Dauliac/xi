//! Dispatch for the unified `xi deploy` verb (xi-fxgx.5.1).
//!
//! Resolves the decision tree's own inputs ([`Facts`]) from live host
//! state, then dispatches to the exact same code the verbs it replaces
//! call: `switch::prepare`/`record_activation_at`, via each scope's own
//! `Switch` subcommand, for a local activation; `push::orchestrate::execute`,
//! via [`push::orchestrate::run`], for a fleet rollout. The decision
//! tree itself ([`xi_deploy::decision`]) and the plan it resolves into
//! ([`xi_deploy::plan`]) are xi-fxgx.2's and xi-fxgx.3's own work; this
//! module is the glue between those pure functions and live state.

use std::{collections::BTreeSet, path::Path};

use color_eyre::{Result, eyre::bail};
use xi_core::{
  command::ElevationStrategy,
  installable::InstallableArgs,
  sources::Scope,
};
use xi_deploy::{
  decision::{Decision, RefuseReason},
  fleet_digest,
  plan::{
    resolve_build_mode,
    resolve_plan,
    resolve_requested_targets,
    run_or_preview,
  },
  push,
  scope::disambiguate_scope,
  unified_args::{DeployArgs, DeployMechanism, DeployScope},
};

/// Run `xi deploy`: resolve the scope(s) to govern, then dispatch each
/// one through [`run_for_scope`]. `--scope all` runs both in sequence.
///
/// # Errors
///
/// Propagates a scope-resolution, fact-gathering, or dispatch failure.
pub fn run(args: &DeployArgs, elevation: &ElevationStrategy) -> Result<()> {
  for scope in resolve_scopes(args)? {
    run_for_scope(args, scope, elevation)?;
  }
  Ok(())
}

/// Which scope(s) `xi deploy` governs for this invocation.
///
/// An explicit `--scope` always wins. Otherwise `cwd` disambiguates
/// between two adopted scopes, per `xi-deploy-unified-verb/spec.md`'s
/// "`cwd` disambiguates scope, not destination" requirement; failing
/// that, whichever scope is actually adopted; failing that (neither or
/// both adopted, with no `cwd` match), `System` - the decision tree's
/// own refusal naming `--adopt` is a clearer answer for an unadopted
/// scope than silently guessing which one the operator meant.
fn resolve_scopes(args: &DeployArgs) -> Result<Vec<Scope>> {
  Ok(match args.scope {
    Some(DeployScope::System) => vec![Scope::System],
    Some(DeployScope::User) => vec![Scope::User],
    Some(DeployScope::All) => vec![Scope::System, Scope::User],
    None => {
      let cwd = std::env::current_dir()?;
      disambiguate_scope(&cwd)?.map_or_else(
        || {
          let adopted: Vec<Scope> = [Scope::System, Scope::User]
            .into_iter()
            .filter(|&scope| is_adopted(scope))
            .collect();
          match adopted.as_slice() {
            [one] => vec![*one],
            _ => vec![Scope::System],
          }
        },
        |scope| vec![scope],
      )
    },
  })
}

/// Whether `scope` has a source configured at all, per
/// [`push::context::resolve_context`] - the same check a push already
/// refuses on with no sources configured.
fn is_adopted(scope: Scope) -> bool {
  push::context::resolve_context(scope).is_ok()
}

/// Resolve the decision tree's inputs for `scope` from live host state,
/// resolve the plan, and either dispatch it or - under `--dry` - print
/// it instead.
///
/// # Errors
///
/// Propagates a fact-gathering or dispatch failure.
fn run_for_scope(
  args: &DeployArgs,
  scope: Scope,
  elevation: &ElevationStrategy,
) -> Result<()> {
  let deploy_scope = match scope {
    Scope::System => DeployScope::System,
    Scope::User => DeployScope::User,
  };

  let (facts, known_targets) = gather_facts(args, scope);
  let plan = resolve_plan(args, &facts, &known_targets, deploy_scope);
  let requested = resolve_requested_targets(args, &known_targets);

  run_or_preview(args, &plan, || {
    match &plan.decision {
      Decision::Refuse(RefuseReason::NotAdopted) => {
        bail!(
          "{scope} has no adopted source; run `xi deploy --adopt <remote>` \
           for this scope first"
        )
      },
      Decision::ActivateLocal(_) => {
        dispatch_local(
          args,
          scope,
          plan.mechanism,
          &requested,
          elevation.clone(),
        )
      },
      Decision::RolloutFleet { targets, .. } => {
        push::orchestrate::run(&push_args(args, scope, targets.clone()))
      },
    }
  })
}

/// Gather [`xi_deploy::decision::Facts`] for `scope` from live state:
/// adoption via [`push::context::resolve_context`], dirtiness via
/// [`xi_core::sources::conflict::inspect_working_tree`], the fleet
/// inventory and discovered flake-deploy outputs via the same
/// [`push::config`]/[`push::discover`] modules `xi push` already
/// reads, and `fleet-digest` freshness via [`fleet_digest`]. Also
/// returns the known target name set (inventory hosts plus discovered
/// targets) [`resolve_requested_targets`] checks a bare positional
/// against.
///
/// A scope with no recorded source at all is not fatal: with an
/// explicit `--adopt` and a positional remote, the scope is treated as
/// adopted here so the tree can resolve a first-time bootstrap to a
/// local activation (`switch::prepare`'s own case 1 does the actual
/// adopting once [`dispatch_local`] reaches it); otherwise the tree
/// refuses, naming `--adopt`, exactly as the spec requires. A flake
/// evaluation failure during discovery degrades to "no flake-deploy
/// outputs" rather than failing `xi deploy` outright - discovery here
/// feeds a routing decision, not a build.
fn gather_facts(
  args: &DeployArgs,
  scope: Scope,
) -> (xi_deploy::decision::Facts, BTreeSet<String>) {
  use xi_deploy::decision::Facts;

  let Ok(context) = push::context::resolve_context(scope) else {
    let adopting_now = args.sources.adopt && !args.targets.is_empty();
    return (
      Facts {
        scope_adopted: adopting_now,
        ..Facts::default()
      },
      BTreeSet::new(),
    );
  };

  let flake_ref = context.sources_dir.display().to_string();
  let nix = push::engine::CliNix;

  let tree_dirty = context.working_dir.as_deref().is_some_and(|working_dir| {
    xi_core::sources::conflict::inspect_working_tree(working_dir)
      .is_ok_and(|tree| tree.is_dirty())
  });

  let xi_toml_path = Path::new(".xi.toml");
  let inventory =
    push::config::PushConfig::from_toml_file(xi_toml_path).unwrap_or_default();
  let inventory_configured = !inventory.hosts.is_empty();

  let my_hostname = xi_core::util::get_hostname(None).ok();
  let host_is_fleet_member = my_hostname.is_some_and(|hostname| {
    inventory
      .hosts
      .iter()
      .any(|host| host.name == hostname || host.ssh == hostname)
  });

  let discovered =
    push::discover::discover(&nix, &flake_ref).unwrap_or_default();
  let colmena_hive_present =
    push::discover::has_colmena_hive(&nix, &flake_ref).unwrap_or(false);
  let flake_deploy_outputs_present =
    !discovered.is_empty() || colmena_hive_present;

  let mut known_targets: BTreeSet<String> = inventory
    .hosts
    .iter()
    .map(|host| host.name.clone())
    .collect();
  known_targets
    .extend(discovered.iter().map(|target| target.addr.name.clone()));

  let fleet_digest_unchanged = fleet_digest::resolve_fleet_digest_unchanged(
    xi_toml_path,
    &known_targets.iter().cloned().collect::<Vec<_>>(),
    args.backend.as_str(),
    &format!("{:?}", resolve_build_mode(args)),
  );

  (
    Facts {
      requested_targets: Vec::new(), // overwritten by resolve_plan
      host_is_fleet_member,
      scope_adopted: true,
      tree_dirty,
      inventory_configured,
      flake_deploy_outputs_present,
      fleet_digest_unchanged,
    },
    known_targets,
  )
}

/// Dispatch a local activation for `scope`: resolve the installable
/// positional (when the decision tree left `requested` empty - a
/// non-empty `requested` means `--local` forced a local activation
/// despite a resolved fleet-target list, which has nothing to build
/// locally from, so the recorded source is used instead), then
/// dispatch to the exact same `Switch` subcommand - and therefore the
/// same `switch::prepare`/`record_activation_at` code path - the
/// standalone verb it replaces already calls.
///
/// # Errors
///
/// Propagates whatever the dispatched verb's own `run` returns, or an
/// error naming `--mechanism` when `scope` is `System` and no
/// mechanism could be detected.
fn dispatch_local(
  args: &DeployArgs,
  scope: Scope,
  mechanism: Option<DeployMechanism>,
  requested: &[String],
  elevation: ElevationStrategy,
) -> Result<()> {
  let installable = if requested.is_empty() {
    match args.targets.as_slice() {
      [one] => InstallableArgs::from_positional(one),
      _ => InstallableArgs::Unspecified,
    }
  } else {
    InstallableArgs::Unspecified
  };

  match scope {
    Scope::User => home_args(args, installable).run(),
    Scope::System => {
      match mechanism {
        Some(DeployMechanism::Nixos) => {
          os_args(args, installable).run(elevation)
        },
        Some(DeployMechanism::Darwin) => {
          darwin_args(args, installable).run(elevation)
        },
        Some(DeployMechanism::SystemManager) => {
          system_args(args, installable).run(elevation)
        },
        None => {
          bail!(
            "could not detect a system activation mechanism for this host; \
             pass --mechanism explicitly"
          )
        },
      }
    },
  }
}

const fn update_args_default() -> xi_core::update::UpdateArgs {
  xi_core::update::UpdateArgs {
    update_all:   false,
    update_input: None,
  }
}

fn os_args(
  args: &DeployArgs,
  installable: InstallableArgs,
) -> xi_nixos::args::OsArgs {
  xi_nixos::args::OsArgs {
    subcommand: xi_nixos::args::OsSubcommand::Switch(
      xi_nixos::args::OsRebuildActivateArgs {
        rebuild:              xi_nixos::args::OsRebuildArgs {
          common:             xi_nixos::args::CommonRebuildArgs {
            dry: false,
            ask: args.ask,
            installable,
            sources: args.sources.clone(),
            no_nom: args.no_nom,
            out_link: args.out_link.clone(),
            diff: args.diff.clone(),
            passthrough: args.passthrough.clone(),
            cache: args.cache.clone(),
          },
          update_args:        update_args_default(),
          hostname:           None,
          specialisation:     None,
          no_specialisation:  false,
          install_bootloader: false,
          extra_args:         Vec::new(),
          bypass_root_check:  false,
          target_host:        None,
          build_host:         None,
          no_validate:        false,
        },
        show_activation_logs: false,
      },
    ),
  }
}

fn darwin_args(
  args: &DeployArgs,
  installable: InstallableArgs,
) -> xi_darwin::args::DarwinArgs {
  xi_darwin::args::DarwinArgs {
    subcommand: xi_darwin::args::DarwinSubcommand::Switch(
      xi_darwin::args::DarwinRebuildArgs {
        common:               xi_core::args::CommonRebuildArgs {
          dry: false,
          ask: args.ask,
          installable,
          sources: args.sources.clone(),
          no_nom: args.no_nom,
          out_link: args.out_link.clone(),
          diff: args.diff.clone(),
          passthrough: args.passthrough.clone(),
          cache: args.cache.clone(),
        },
        update_args:          update_args_default(),
        hostname:             None,
        extra_args:           Vec::new(),
        bypass_root_check:    false,
        show_activation_logs: false,
        build_host:           None,
      },
    ),
  }
}

fn home_args(
  args: &DeployArgs,
  installable: InstallableArgs,
) -> xi_home::args::HomeArgs {
  xi_home::args::HomeArgs {
    subcommand: xi_home::args::HomeSubcommand::Switch(
      xi_home::args::HomeRebuildArgs {
        common:               xi_core::args::CommonRebuildArgs {
          dry: false,
          ask: args.ask,
          installable,
          sources: args.sources.clone(),
          no_nom: args.no_nom,
          out_link: args.out_link.clone(),
          diff: args.diff.clone(),
          passthrough: args.passthrough.clone(),
          cache: args.cache.clone(),
        },
        update_args:          update_args_default(),
        configuration:        None,
        specialisation:       None,
        no_specialisation:    false,
        extra_args:           Vec::new(),
        backup_extension:     None,
        show_activation_logs: false,
        build_host:           None,
      },
    ),
  }
}

fn system_args(
  args: &DeployArgs,
  installable: InstallableArgs,
) -> crate::system::args::SystemArgs {
  crate::system::args::SystemArgs {
    subcommand: crate::system::args::SystemSubcommand::Switch(
      crate::system::args::SystemRebuildArgs {
        common:               xi_core::args::CommonRebuildArgs {
          dry: false,
          ask: args.ask,
          installable,
          sources: args.sources.clone(),
          no_nom: args.no_nom,
          out_link: args.out_link.clone(),
          diff: args.diff.clone(),
          passthrough: args.passthrough.clone(),
          cache: args.cache.clone(),
        },
        update_args:          update_args_default(),
        hostname:             None,
        extra_args:           Vec::new(),
        bypass_root_check:    false,
        show_activation_logs: false,
      },
    ),
  }
}

/// Build the [`push::args::PushArgs`] a fleet-rollout decision
/// dispatches to, from the subset of `args`' flags `DeployArgs`
/// unions in from `push`'s own surface.
fn push_args(
  args: &DeployArgs,
  scope: Scope,
  targets: Vec<String>,
) -> push::args::PushArgs {
  push::args::PushArgs {
    targets,
    on: args.on.clone(),
    backend: args.backend,
    phase: args.phase.clone(),
    mode: args.mode,
    remote_build: args.remote_build,
    sources_transport: None,
    retry: None,
    magic_rollback: args.magic_rollback,
    parallel: args.parallel,
    interactive_sudo: args.interactive_sudo,
    copy_via: args.copy_via,
    fail_fast: args.fail_fast,
    force_dirty: args.sources.force_dirty,
    build_strategy: push::args::BuildStrategy::Interleaved,
    build_only: false,
    from_manifest: None,
    scope: match scope {
      Scope::System => push::args::PushScope::System,
      Scope::User => push::args::PushScope::User,
    },
    reattach: None,
    notify: None,
  }
}
