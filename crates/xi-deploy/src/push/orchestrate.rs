//! The `xi push` orchestrator.
//!
//! This is the piece [`super`]'s module doc named as missing: the glue
//! that resolves a push's sources context, discovers and selects its
//! targets, picks a backend, and dispatches each target through
//! [`preflight`], [`engine`](super::engine) (or [`mode_sources`] under
//! `--mode sources`)
//! and, if asked, [`rollback`]'s confirmation window.
//!
//! `deploy-rs` and `colmena` targets are not reimplemented here: those
//! two backends already exist in [`crate::backend`] with their own
//! tested pipelines (and, for deploy-rs, its own magic-rollback and
//! multi-profile semantics), so a push that resolves to either one
//! delegates to the same [`crate::detect_and_deploy`] entry point
//! `xi deploy` uses. Only the `builtin` resolution runs the primitives
//! this module name-checks above - they are, in effect, the "ten gaps"
//! version of the `builtin` backend the `xi-sources-push` proposal
//! describes.

use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
  sync::atomic::{AtomicBool, Ordering},
  time::Duration,
};

use color_eyre::{
  Result,
  eyre::{bail, eyre},
};
use tracing::{info, warn};
use xi_core::sources::{
  Scope,
  SourcesContext,
  conflict::inspect_working_tree,
  secrets::{SecretSpec, UploadAt},
  shadow,
};

use super::{
  args::{BuildStrategy, PushArgs, PushBackend, PushMode},
  config::{self, PushConfig},
  context,
  discover::{self, PushTarget, ResolvedBackend},
  engine::{CliNix, Nix, RemoteCommand, SshTransport, Transport},
  limits::PhaseLimits,
  mode_sources,
  mode_target_pull,
  preflight,
  reattach,
  receipt,
  rollback,
};
use crate::{fleet_digest::FleetDigestInput, target::ProfileKind};

/// The file name a batched build's manifest is written under, inside the
/// push's state dir.
const MANIFEST_FILE_NAME: &str = "push-manifest.json";

/// Where `--build-only` writes its manifest: the same state dir every
/// other `xi push` state file lives in, so a later `--from-manifest`
/// invocation has an obvious default to point at.
#[must_use]
fn default_manifest_path(context: &SourcesContext) -> PathBuf {
  context.state_dir.join(MANIFEST_FILE_NAME)
}

/// Write a batched build's manifest to `path` as JSON, creating the
/// parent directory first if it does not exist yet.
///
/// # Errors
///
/// Returns an error naming `path` if the parent directory cannot be
/// created, the manifest cannot be serialized, or the file cannot be
/// written.
fn write_manifest(
  path: &Path,
  manifest: &BTreeMap<String, String>,
) -> Result<()> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|e| {
      eyre!(
        "{}: creating the manifest's directory: {e}",
        parent.display()
      )
    })?;
  }
  let json = serde_json::to_string_pretty(manifest)
    .map_err(|e| eyre!("serializing the push manifest: {e}"))?;
  std::fs::write(path, json)
    .map_err(|e| eyre!("{}: writing the push manifest: {e}", path.display()))
}

/// Read a batched build's manifest back from `path`.
///
/// # Errors
///
/// Returns an error naming `path` if the file is missing, unreadable, or
/// not the `{ target: store-path }` JSON shape [`write_manifest`] wrote.
fn read_manifest(path: &Path) -> Result<BTreeMap<String, String>> {
  let raw = std::fs::read_to_string(path)
    .map_err(|e| eyre!("{}: reading --from-manifest: {e}", path.display()))?;
  serde_json::from_str(&raw)
    .map_err(|e| eyre!("{}: parsing --from-manifest: {e}", path.display()))
}

/// The subset of `targets` a batched build phase builds, and a
/// manifest's ship phase ships from a recorded store path: every target
/// whose resolved mode is [`PushMode::Closure`].
///
/// Mode is resolved the same way `push_one` resolves it, through the
/// build-location ladder (`config::resolve_remote_build`) rather than
/// `args.mode` alone, so a target the ladder sends to `--mode sources`
/// is excluded even when no `--mode` flag was given.
fn closure_mode_targets<'a>(
  targets: &'a [PushTarget],
  args: &PushArgs,
  inventory: &PushConfig,
  nix_module_remote_build: Option<bool>,
) -> Vec<&'a PushTarget> {
  targets
    .iter()
    .filter(|target| {
      let remote_build = config::resolve_remote_build(
        args.remote_build,
        &target.addr.name,
        inventory,
        nix_module_remote_build,
      );
      let sources_transport = config::resolve_sources_transport(
        args.sources_transport,
        &target.addr.name,
        inventory,
      );
      args.effective_mode(remote_build, sources_transport) == PushMode::Closure
    })
    .collect()
}

/// Refuse early if `manifest` (read from `--from-manifest`) is missing an
/// entry for any closure-mode target this push selected.
///
/// The ship phase must not fall back to `nix.build` silently for a
/// target nobody built, so this runs once, before dispatch, rather than
/// leaving each worker to discover a missing entry on its own.
///
/// # Errors
///
/// Names every missing target in one error.
fn require_manifest_covers(
  targets: &[PushTarget],
  args: &PushArgs,
  inventory: &PushConfig,
  nix_module_remote_build: Option<bool>,
  manifest: &BTreeMap<String, String>,
) -> Result<()> {
  let missing: Vec<&str> =
    closure_mode_targets(targets, args, inventory, nix_module_remote_build)
      .into_iter()
      .map(|target| target.addr.name.as_str())
      .filter(|name| !manifest.contains_key(*name))
      .collect();
  if missing.is_empty() {
    Ok(())
  } else {
    Err(eyre!(
      "--from-manifest is missing an entry for: {}",
      missing.join(", ")
    ))
  }
}

/// How often [`rollback::confirm`] polls the sentinel.
///
/// Fixed rather than configurable: the spec only names the *window*
/// (`--magic-rollback <seconds>`); the poll cadence inside it is an
/// implementation detail no scenario depends on.
const ROLLBACK_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// What happened on one target.
#[derive(Debug)]
pub struct TargetResult {
  /// The target's flake-attribute / inventory name.
  pub name:    String,
  /// `Ok(())` once the target is fully pushed (and, under
  /// `--magic-rollback`, confirmed); `Err` for any failure, including a
  /// magic-rollback that reverted the activation.
  pub outcome: Result<()>,
}

/// The whole run's report: one [`TargetResult`] per selected target,
/// plus the backend and build-location decisions [`run`] caches into
/// `fleet-digest` on success.
#[derive(Debug)]
pub struct PushReport {
  /// Per-target results, in selection order.
  pub results:        Vec<TargetResult>,
  /// The backend this run dispatched to.
  pub backend:        ResolvedBackend,
  /// The resolved build-location decision (`--mode`'s effective value),
  /// as a `fleet-digest` hash input.
  pub build_location: String,
}

impl PushReport {
  /// Whether every target succeeded.
  #[must_use]
  pub fn ok(&self) -> bool {
    self.results.iter().all(|result| result.outcome.is_ok())
  }

  /// The targets that failed, in selection order.
  #[must_use]
  pub fn failures(&self) -> Vec<&TargetResult> {
    self
      .results
      .iter()
      .filter(|result| result.outcome.is_err())
      .collect()
  }
}

/// Run `xi push` end to end: resolve context, discover, select, and
/// dispatch every selected target.
///
/// The thin, non-testable half of this module: it resolves the sources
/// context and builds the real [`CliNix`]/[`SshTransport`] pair, then
/// hands off to [`execute`], which is what every test in this module
/// drives directly against a fake [`Nix`]/[`Transport`].
///
/// # Errors
///
/// With `--reattach` set, propagates [`reattach::run`]'s error instead of
/// anything below - a reattach does not resolve a sources context or
/// discover targets at all. Otherwise refuses before any discovery if the
/// sources context is `Missing` or the working dir is dirty without
/// `--force-dirty`, and otherwise returns an error naming how many
/// targets failed once every target has been attempted (preflight and
/// `--fail-fast` both run to completion first).
pub fn run(args: &PushArgs) -> Result<()> {
  if let Some(id) = &args.reattach {
    // A reattach's receipts live under the scope's state dir regardless
    // of whether that scope currently has sources configured, so this
    // reads `ScopePaths` directly rather than going through
    // `context::resolve_context`, which would refuse on a `Missing`
    // sources status a reattach has no reason to care about.
    let state_dir =
      xi_core::sources::config::ScopePaths::resolve(args.sources_scope())
        .state_dir;
    return reattach::run(id, &state_dir, &SshTransport);
  }

  args.validate()?;

  let scope = args.sources_scope();
  let context = context::resolve_context(scope)?;
  refuse_if_dirty(&context, args.force_dirty)?;

  let flake_ref = context.sources_dir.display().to_string();
  let secrets = context::resolve_secrets(scope)?;
  // The same literal `.xi.toml` the `[sources]` ladder reads
  // (`xi_core::sources::args::resolve_layers`): the inventory describes
  // *other* machines, so it has no per-scope Nix-module rung of its own
  // and is read once, from the operator's own config repo.
  let inventory = PushConfig::from_toml_file(std::path::Path::new(".xi.toml"))?;
  // The build-location and backend ladders' Nix-module rung: the
  // `[push]` table the Nix module wrote into the same per-scope file
  // `[sources]`'s own Nix-module rung reads
  // (`xi_core::sources::config::ScopePaths::resolve(scope).config_file`).
  let nix_module_config = PushConfig::from_toml_file(
    &xi_core::sources::config::ScopePaths::resolve(scope).config_file,
  )?;
  let nix = CliNix;
  let transport = SshTransport;

  let report = execute(
    args,
    &context,
    &flake_ref,
    &secrets,
    &inventory,
    nix_module_config.remote_build,
    nix_module_config.backend,
    Path::new(".xi.toml"),
    &nix,
    &transport,
  )?;

  for result in &report.results {
    match &result.outcome {
      Ok(()) => info!(target = %result.name, "pushed"),
      Err(e) => warn!(target = %result.name, error = %e, "push failed"),
    }
  }

  if report.ok() {
    Ok(())
  } else {
    bail!(
      "push failed on {} of {} target(s): {}",
      report.failures().len(),
      report.results.len(),
      report
        .failures()
        .iter()
        .map(|result| result.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
    );
  }
}

/// Cache `fleet-digest` next to `xi_toml_path`, after a successful
/// rollout.
///
/// Best-effort by design, matching
/// [`xi_core::sources::switch::record_activation`]'s stance: the fleet has
/// already been pushed by the time this runs, so a cache-write failure (an
/// unwritable directory, a read-only repo checkout) must not turn a successful
/// rollout into a failed command - it only costs the next run the re-evaluation
/// skip this cache exists to grant.
fn cache_fleet_digest(xi_toml_path: &Path, report: &PushReport) {
  let input = FleetDigestInput {
    targets:        report.results.iter().map(|r| r.name.clone()).collect(),
    backend:        report.backend.as_str().to_owned(),
    build_location: report.build_location.clone(),
  };
  if let Err(err) = crate::fleet_digest::store(xi_toml_path, &input) {
    warn!("push succeeded but caching fleet-digest failed: {err}");
  }
}

/// Refuse the push when `context.working_dir` is dirty, unless
/// `force_dirty` is set.
///
/// Mirrors [`xi_core::sources::conflict::apply_auto_sync`]'s own dirty
/// check: a scope with no recorded working dir (only a managed sources
/// dir) has nothing an operator could have left uncommitted, so there is
/// nothing to refuse.
///
/// # Errors
///
/// Propagates a git-invocation failure from [`inspect_working_tree`], or
/// refuses naming the dirty paths per
/// [`xi_core::sources::conflict::DirtyTree::refusal`].
fn refuse_if_dirty(context: &SourcesContext, force_dirty: bool) -> Result<()> {
  if force_dirty {
    return Ok(());
  }
  let Some(working_dir) = context.working_dir.as_deref() else {
    return Ok(());
  };
  let tree = inspect_working_tree(working_dir)?;
  if tree.is_dirty() {
    bail!("{}", tree.refusal("xi push"));
  }
  Ok(())
}

/// The testable core of [`run`]: discover, select, pick a backend, and
/// dispatch every selected target against the supplied [`Nix`] and
/// [`Transport`].
///
/// # Errors
///
/// Propagates a discovery, selection or backend-detection error. A
/// per-target failure is *not* an error here - it is recorded in the
/// returned [`PushReport`], so the caller can decide (via
/// `--fail-fast`'s effect on which targets even ran) how to react.
///
/// On a fully successful rollout (every target in the returned
/// [`PushReport`] `Ok`), also caches `fleet-digest` next to
/// `xi_toml_path` - best-effort, per [`cache_fleet_digest`].
#[expect(clippy::too_many_arguments, reason = "every argument is load-bearing")]
pub fn execute(
  args: &PushArgs,
  context: &SourcesContext,
  flake_ref: &str,
  secrets: &BTreeMap<String, SecretSpec>,
  inventory: &PushConfig,
  nix_module_remote_build: Option<bool>,
  nix_module_backend: Option<PushBackend>,
  xi_toml_path: &Path,
  nix: &dyn Nix,
  transport: &dyn Transport,
) -> Result<PushReport> {
  let all_targets = discover::discover(nix, flake_ref)?;
  let selected =
    discover::select(&all_targets, &args.targets, args.on.as_deref())?;
  let targets: Vec<PushTarget> =
    config::apply_inventory(selected.into_iter().cloned().collect(), inventory);

  let colmena_present = discover::has_colmena_hive(nix, flake_ref)?;

  // The fleet-level build-location decision, resolved with no per-host
  // override: a `fleet-digest` hash input describes the whole rollout,
  // not any one target, so it reads the same ladder `push_one` reads
  // per-host but without a host name to look up.
  let fleet_remote_build = config::resolve_remote_build(
    args.remote_build,
    "",
    inventory,
    nix_module_remote_build,
  );
  let fleet_sources_transport =
    config::resolve_sources_transport(args.sources_transport, "", inventory);
  let build_location = format!(
    "{:?}",
    args.effective_mode(fleet_remote_build, fleet_sources_transport)
  );

  // Group by the backend ladder's resolved rung (`config::resolve_backend`,
  // not `args.backend` read directly) before auto-detecting: a fleet with
  // nothing configured resolves every target to the same rung it always
  // has (`args.backend` itself), so this degenerates to one group and one
  // `detect_backend` call, exactly as before this ladder existed. A
  // per-host `backend` override can split the fleet into more than one
  // group, each delegated (or run through the `builtin` pipeline below)
  // independently.
  let mut groups: Vec<(PushBackend, Vec<PushTarget>)> = Vec::new();
  for target in targets {
    let requested = config::resolve_backend(
      args.backend,
      &target.addr.name,
      inventory,
      nix_module_backend,
    );
    match groups.iter_mut().find(|(key, _)| *key == requested) {
      Some((_, group)) => group.push(target),
      None => groups.push((requested, vec![target])),
    }
  }

  let mut results = Vec::new();
  let mut targets = Vec::new();
  // The one precedence table (deploy stanza > colmenaHive output >
  // builtin): `crate::detect_and_deploy`'s own auto-detection, used by
  // the standalone `xi deploy` verb, is a thin wrapper over this same
  // function as of xi-jhh4.5.1, so this and that entrypoint can never
  // disagree on the same flake (SC-UA-005).
  let mut backend = ResolvedBackend::Builtin;
  for (requested, group) in groups {
    let resolved = discover::detect_backend(requested, &group, colmena_present);
    if resolved == ResolvedBackend::Builtin {
      targets.extend(group);
    } else {
      backend = resolved;
      delegate(resolved, flake_ref, args, &group)?;
      results.extend(group.into_iter().map(|target| {
        TargetResult {
          name:    target.addr.name,
          outcome: Ok(()),
        }
      }));
    }
  }

  if targets.is_empty() {
    let report = PushReport {
      results,
      backend,
      build_location,
    };
    if report.ok() {
      cache_fleet_digest(xi_toml_path, &report);
    }
    return Ok(report);
  }

  let limits = PhaseLimits::new(args.parallel);
  let aborted = AtomicBool::new(false);
  // The activation step tags itself with the operator's own hostname,
  // not the target's, so it is resolved once here rather than inside
  // each per-target `xi <scope> switch` invocation.
  let origin_host = xi_core::util::get_hostname(None)?;

  // The build-then-ship manifest: read from `--from-manifest` when
  // given, built fresh under `--build-strategy batched` otherwise (and
  // not at all under the default interleaved strategy, which still
  // builds each target inside `push_one` exactly as it always has).
  let manifest: Option<BTreeMap<String, String>> =
    if let Some(path) = &args.from_manifest {
      let loaded = read_manifest(path)?;
      require_manifest_covers(
        &targets,
        args,
        inventory,
        nix_module_remote_build,
        &loaded,
      )?;
      Some(loaded)
    } else if args.build_strategy == BuildStrategy::Batched {
      let built = build_batch(
        nix,
        &targets,
        args,
        inventory,
        nix_module_remote_build,
        &limits,
      )?;
      if args.build_only {
        write_manifest(&default_manifest_path(context), &built)?;
        results.extend(built.keys().map(|name| {
          TargetResult {
            name:    name.clone(),
            outcome: Ok(()),
          }
        }));
        return Ok(PushReport {
          results,
          backend,
          build_location,
        });
      }
      Some(built)
    } else {
      None
    };

  let dispatched: Vec<TargetResult> = std::thread::scope(|scope| {
    let handles: Vec<_> = targets
      .iter()
      .map(|target| {
        let limits = &limits;
        let aborted = &aborted;
        let origin_host = &origin_host;
        let name = target.addr.name.clone();
        let prebuilt = manifest
          .as_ref()
          .and_then(|manifest| manifest.get(&name))
          .map(String::as_str);
        let handle = scope.spawn(move || {
          push_one(
            nix,
            transport,
            limits,
            target,
            args,
            context,
            secrets,
            inventory,
            nix_module_remote_build,
            prebuilt,
            origin_host,
            aborted,
          )
        });
        (name, handle)
      })
      .collect();

    handles
      .into_iter()
      .map(|(name, handle)| {
        // A panicking worker reports as a failed target rather than
        // propagating the panic to the caller of `execute`: the other
        // targets' threads already ran (or are running) independently,
        // so there is no "whole run" to abort cleanly, and `PushReport`
        // can say "this one target panicked" exactly as it says "this
        // one target's activation failed".
        let outcome = handle
          .join()
          .unwrap_or_else(|_| Err(eyre!("push worker for {name} panicked")));
        TargetResult { name, outcome }
      })
      .collect()
  });

  results.extend(dispatched);
  let report = PushReport {
    results,
    backend,
    build_location,
  };
  if report.ok() {
    cache_fleet_digest(xi_toml_path, &report);
  }
  Ok(report)
}

/// Build every selected `PushMode::Closure` target in an upfront phase,
/// bounded by the same `limits.eval` semaphore `push_one` builds under.
///
/// Targets whose resolved mode is [`PushMode::Sources`] are skipped: a
/// batched build has nothing to build for them, since they ship by
/// syncing a sources dir rather than a store path. Mode is resolved the
/// same way `push_one` resolves it, through the build-location ladder
/// (`config::resolve_remote_build`) rather than `args.mode` alone, so a
/// target the ladder sends to `--mode sources` is excluded even when no
/// `--mode` flag was given.
///
/// # Errors
///
/// Returns the first build failure, naming the target it came from. A
/// batched build is all-or-nothing: a manifest missing even one target's
/// store path cannot be used to ship that target later.
pub fn build_batch(
  nix: &dyn Nix,
  targets: &[PushTarget],
  args: &PushArgs,
  inventory: &PushConfig,
  nix_module_remote_build: Option<bool>,
  limits: &PhaseLimits,
) -> Result<BTreeMap<String, String>> {
  let closure_targets =
    closure_mode_targets(targets, args, inventory, nix_module_remote_build);

  let built: Vec<(String, Result<String>)> = std::thread::scope(|scope| {
    // Collecting into `handles` first is load-bearing, not needless: every
    // `scope.spawn` below must run before the first `.join()` blocks, or
    // the builds serialise one at a time instead of racing concurrently
    // under `limits.eval`.
    #[expect(
      clippy::needless_collect,
      reason = "forces eager spawn before join"
    )]
    let handles: Vec<_> = closure_targets
      .iter()
      .map(|target| {
        let name = target.addr.name.clone();
        let handle = scope.spawn(move || {
          let _permit = limits.eval.acquire();
          nix.build(&target.installable)
        });
        (name, handle)
      })
      .collect();

    handles
      .into_iter()
      .map(|(name, handle)| {
        let outcome = handle
          .join()
          .unwrap_or_else(|_| Err(eyre!("build worker for {name} panicked")));
        (name, outcome)
      })
      .collect()
  });

  let mut manifest = BTreeMap::new();
  for (name, outcome) in built {
    match outcome {
      Ok(store_path) => {
        manifest.insert(name, store_path);
      },
      Err(e) => return Err(eyre!("batched build failed on {name}: {e}")),
    }
  }
  Ok(manifest)
}

/// Delegate to the already-tested `deploy-rs` / `colmena` backend `xi
/// deploy` uses, restricted to the targets this push already selected.
///
/// `backend` is passed through as an explicit [`crate::args::DeployArgs`]
/// override rather than left for [`crate::detect_and_deploy`] to
/// auto-detect: it was already resolved by [`discover::detect_backend`]
/// against this push's own filtered target list, and re-detecting from
/// bare `flake_ref` inside `detect_and_deploy` would discover the
/// flake's *full* target set instead. `push::orchestrate` and
/// `crate::detect_and_deploy` share the one `discover::detect_backend`
/// precedence table either way (xi-jhh4.5.1/5.2) - this is the one place
/// that table is *called* with push's own targets rather than
/// rediscovered ones.
///
/// `on` is left unset on the constructed [`crate::args::DeployArgs`]:
/// [`discover::select`] already applied `--on` against the taxonomy
/// discovery shares with those backends, so re-applying it inside the
/// backend (against *its* idea of tags, which colmena has and deploy-rs
/// does not) would risk filtering a second time against the wrong thing.
///
/// # Errors
///
/// Propagates whatever the delegated backend's own `deploy` returns.
fn delegate(
  backend: ResolvedBackend,
  flake_ref: &str,
  args: &PushArgs,
  targets: &[PushTarget],
) -> Result<()> {
  let deploy_args = crate::args::DeployArgs {
    flake_ref:         Some(flake_ref.to_owned()),
    targets:           targets.iter().map(|t| t.addr.name.clone()).collect(),
    on:                None,
    backend:           Some(backend.as_str().to_owned()),
    dry:               false,
    no_nom:            false,
    skip_checks:       false,
    no_magic_rollback: args.magic_rollback == 0,
    confirm_timeout:   if args.magic_rollback > 0 {
      args.magic_rollback
    } else {
      30
    },
    show_trace:        false,
    extra_args:        Vec::new(),
  };
  crate::detect_and_deploy(&deploy_args)
}

/// Push one target through preflight, build-or-sync, activate, secrets
/// and (optionally) the magic-rollback confirmation window.
///
/// `prebuilt`, when `Some`, is a [`PushMode::Closure`] target's already-
/// built store path from a batched build's manifest - `nix.build` is not
/// called at all in that case, which is the whole point of shipping from
/// a manifest rather than rebuilding on the ship leg.
///
/// # Errors
///
/// Returns an error naming the failing step: a failed [`preflight::run`]
/// check, a build/copy/sync/activation failure, or a magic-rollback that
/// reverted the activation. Also returns an error, without touching
/// `target` at all, when `aborted` is already set and `args.fail_fast`
/// is on.
#[expect(clippy::too_many_arguments, reason = "every argument is load-bearing")]
fn push_one(
  nix: &dyn Nix,
  transport: &dyn Transport,
  limits: &PhaseLimits,
  target: &PushTarget,
  args: &PushArgs,
  context: &SourcesContext,
  secrets: &BTreeMap<String, SecretSpec>,
  inventory: &PushConfig,
  nix_module_remote_build: Option<bool>,
  prebuilt: Option<&str>,
  origin_host: &str,
  aborted: &AtomicBool,
) -> Result<()> {
  // Checked again just after every `apply` permit acquisition below: a
  // thread can reach this first check before an earlier target (holding
  // the only `--parallel 1` apply slot) has failed and set the flag.
  // Re-checking once this thread actually holds the slot a mutation
  // needs is what makes `--fail-fast` deterministic rather than a race
  // between "permit released" and "flag set".
  let check_aborted = || -> Result<()> {
    if args.fail_fast && aborted.load(Ordering::SeqCst) {
      bail!("skipped: an earlier target failed under --fail-fast");
    }
    Ok(())
  };
  check_aborted()?;

  let remote_build = config::resolve_remote_build(
    args.remote_build,
    &target.addr.name,
    inventory,
    nix_module_remote_build,
  );
  let sources_transport = config::resolve_sources_transport(
    args.sources_transport,
    &target.addr.name,
    inventory,
  );
  let retry_attempts =
    config::resolve_retry_attempts(args.retry, &target.addr.name, inventory);
  let mode = args.effective_mode(remote_build, sources_transport);
  if mode == PushMode::Sources && args.copy_via == super::args::CopyVia::Target
  {
    bail!(
      "the build-location ladder resolved {} to --mode sources, which never \
       copies a closure from this machine, so --copy-via target has nothing \
       to choose",
      target.addr.name
    );
  }

  let report = preflight::run(transport, target, args.interactive_sudo)?;
  if !report.ok() {
    if args.fail_fast {
      aborted.store(true, Ordering::SeqCst);
    }
    bail!(
      "preflight failed on {}: {}",
      target.addr.name,
      report.summary()
    );
  }

  upload_secrets(
    transport,
    target,
    context,
    secrets,
    UploadAt::PreActivate,
    args.interactive_sudo,
    retry_attempts,
  )?;

  match mode {
    PushMode::Sources => {
      let _permit = limits.apply.acquire();
      check_aborted()?;
      if let Err(e) = mode_sources::sync(
        transport,
        &target.addr,
        &context.sources_dir,
        args.sources_scope(),
        &[],
        args.interactive_sudo,
        retry_attempts,
      )
      .and_then(|()| {
        switch_with_receipt(
          transport,
          target,
          args,
          &context.state_dir,
          origin_host,
          retry_attempts,
        )
      }) {
        if args.fail_fast {
          aborted.store(true, Ordering::SeqCst);
        }
        return Err(e);
      }
    },
    PushMode::TargetPull => {
      let _permit = limits.apply.acquire();
      check_aborted()?;
      let to_sha = shadow::head_sha(&context.sources_dir).map_err(|e| {
        eyre!(
          "resolving the sources dir HEAD for target-pull on {}: {e}",
          target.addr.name
        )
      })?;
      if let Err(e) = mode_target_pull::advance(
        transport,
        &target.addr,
        args.sources_scope(),
        to_sha,
      ) {
        if args.fail_fast {
          aborted.store(true, Ordering::SeqCst);
        }
        return Err(e);
      }
    },
    PushMode::Closure => {
      let store_path = if let Some(store_path) = prebuilt {
        store_path.to_owned()
      } else {
        let _permit = limits.eval.acquire();
        nix.build(&target.installable)?
      };

      let _permit = limits.apply.acquire();
      check_aborted()?;
      if let Err(e) = transport.copy_closure(
        &target.addr,
        &store_path,
        args.copy_via,
        retry_attempts,
      ) {
        if args.fail_fast {
          aborted.store(true, Ordering::SeqCst);
        }
        return Err(e);
      }

      let script = activation_script(
        target,
        &store_path,
        args.sources_scope(),
        origin_host,
      );
      let outcome = transport.run(
        &target.addr,
        &RemoteCommand::elevated(script, args.interactive_sudo, retry_attempts)
          .non_retryable(),
      )?;
      if !outcome.success {
        if args.fail_fast {
          aborted.store(true, Ordering::SeqCst);
        }
        bail!(
          "activation failed on {}: {}",
          target.addr.name,
          outcome.message()
        );
      }
    },
  }

  upload_secrets(
    transport,
    target,
    context,
    secrets,
    UploadAt::PostActivate,
    args.interactive_sudo,
    retry_attempts,
  )?;

  if args.magic_rollback > 0 {
    let _permit = limits.apply.acquire();
    check_aborted()?;
    let outcome = rollback::confirm(
      transport,
      &target.addr,
      Duration::from_secs(args.magic_rollback),
      ROLLBACK_POLL_INTERVAL,
      args.interactive_sudo,
      retry_attempts,
    )?;
    if outcome == rollback::RollbackOutcome::RolledBack {
      if args.fail_fast {
        aborted.store(true, Ordering::SeqCst);
      }
      bail!(
        "activation on {} did not confirm within {}s; rolled back",
        target.addr.name,
        args.magic_rollback
      );
    }
  }

  Ok(())
}

/// How many times [`switch_with_receipt`] will regenerate a colliding
/// push id before giving up.
///
/// [`mode_sources::push_id`]'s random suffix makes a collision roughly
/// 1-in-16M per draw - this bound exists only to turn a catastrophic
/// (e.g. broken RNG) run of collisions into a clear error instead of an
/// infinite loop, not because a real push is ever expected to need more
/// than one draw.
const MAX_PUSH_ID_ATTEMPTS: u8 = 10;

/// Generate a push id for `target_name` that does not collide with any
/// receipt or notified-marker already on disk for `state_dir`.
///
/// Deferred by bead `xi-enwe.1.1` to "once the receipt store exists" -
/// it now does, so [`switch_with_receipt`] checks here rather than
/// trusting [`mode_sources::push_id`]'s randomness alone: two pushes to
/// the same target in the same second landing on the same suffix would
/// otherwise silently clobber each other's receipt, and if the
/// colliding id's unit was already `--collect`-ed, the second
/// `systemd-run --unit=` could even reuse a still-live name.
///
/// # Errors
///
/// Returns an error if every attempt collided.
fn unique_push_id(state_dir: &std::path::Path, target_name: &str) -> Result<String> {
  unique_push_id_with(state_dir, || mode_sources::push_id(target_name))
}

/// [`unique_push_id`]'s retry loop, with the id generator injected so a
/// test can force a collision - a real collision only happens for one
/// in roughly 16M draws, far too rare to trigger through
/// [`mode_sources::push_id`]'s actual randomness in a test.
fn unique_push_id_with(
  state_dir: &std::path::Path,
  mut generate: impl FnMut() -> String,
) -> Result<String> {
  for _ in 0..MAX_PUSH_ID_ATTEMPTS {
    let candidate = generate();
    if !receipt::exists(state_dir, &candidate) {
      return Ok(candidate);
    }
    warn!(
      push_id = candidate,
      "generated push id collided with an existing receipt or notified \
       marker; regenerating"
    );
  }
  bail!(
    "could not generate a push id that doesn't collide with an existing \
     receipt after {MAX_PUSH_ID_ATTEMPTS} attempts"
  );
}

/// Run [`mode_sources::switch`] with a local receipt bracketing the SSH
/// call that starts its transient `systemd-run` unit.
///
/// A push-id is generated here, before any SSH call runs, so the
/// receipt at `<state_dir>/push-receipts/<push-id>.json` names the
/// exact unit [`mode_sources::switch`] is about to start on `target`.
/// The receipt is removed only once `switch` returns `Ok`: that is the
/// one outcome this process can be sure it observed cleanly end to end.
/// A confirmed `Err` from `switch` - the SSH round-trip completed and
/// the remote rebuild itself failed - still has the receipt's `notify`
/// fired through [`receipt::mark_notified_and_notify`] (the same
/// mechanism `reattach::run`'s failure branches use) before the error
/// propagates, since the spec's "`--notify` fires exactly once per
/// terminal observation" covers this confirmed-failure, stayed-connected
/// path too, not just a disconnect. The receipt itself still stays in
/// place either way: `switch`'s `Result` does not distinguish "the
/// remote told us it failed" from "we lost the connection and don't
/// know", and the signal a later `--reattach` needs (a sibling task, not
/// read here) is exactly a receipt an ambiguous outcome did not remove.
///
/// # Errors
///
/// Propagates a [`receipt::write`] failure before the SSH call, or
/// [`mode_sources::switch`]'s own error after it.
fn switch_with_receipt(
  transport: &dyn Transport,
  target: &PushTarget,
  args: &PushArgs,
  state_dir: &std::path::Path,
  origin_host: &str,
  retry_attempts: u32,
) -> Result<()> {
  let push_id = unique_push_id(state_dir, &target.addr.name)?;
  let unit_name = mode_sources::unit_name(&push_id);
  let receipt = receipt::PushReceipt::new(
    target.addr.name.clone(),
    unit_name,
    args.sources_scope(),
  )
  .with_notify(args.notify.clone());
  receipt::write(state_dir, &push_id, &receipt)?;

  let outcome = mode_sources::switch(
    transport,
    target,
    args.sources_scope(),
    origin_host,
    args.interactive_sudo,
    &push_id,
    retry_attempts,
  );
  match outcome {
    Ok(()) => {
      if let Err(remove_err) = receipt::remove_and_notify(state_dir, &push_id, &receipt) {
        warn!(
          push_id,
          error = %remove_err,
          "failed to remove push receipt after observing a terminal outcome"
        );
      }
      Ok(())
    },
    Err(err) => {
      if let Err(notify_err) = receipt::mark_notified_and_notify(state_dir, &push_id, &receipt) {
        warn!(
          push_id,
          error = %notify_err,
          "failed to record --notify as fired for a confirmed switch failure"
        );
      }
      Err(err)
    },
  }
}

/// Upload every declared secret whose [`UploadAt`] matches `when`.
///
/// `secret.source` is resolved against `context.sources_dir`: the
/// sources dir is the stable root `xi push` always deploys from
/// (unlike the operator's current working directory), so a `source =
/// "./key"` stanza resolves the same way regardless of where `xi push`
/// was invoked from.
///
/// # Errors
///
/// Propagates an [`Transport::upload_secret`] failure, naming the
/// offending secret.
fn upload_secrets(
  transport: &dyn Transport,
  target: &PushTarget,
  context: &SourcesContext,
  secrets: &BTreeMap<String, SecretSpec>,
  when: UploadAt,
  interactive_sudo: bool,
  retry_attempts: u32,
) -> Result<()> {
  for secret in secrets.values().filter(|secret| secret.upload_at == when) {
    let local = secret.source_from(&context.sources_dir);
    transport
      .upload_secret(&target.addr, &local, secret, interactive_sudo, retry_attempts)
      .map_err(|e| {
        eyre!(
          "uploading secret '{}' to {}: {e}",
          secret.name,
          target.addr.name
        )
      })?;
  }
  Ok(())
}

/// The activation command for `--mode closure`: for a NixOS, Darwin or
/// Home Manager target, a remote `xi <scope> switch <store_path>`
/// invocation, tagged with `XI_APPLIED_ORIGIN=push:<origin_host>` so
/// `record_activation_at` can tell this activation apart from a local
/// interactive switch or a gitops-daemon tick.
///
/// `store_path` is already a built, realized store path, so this costs
/// `xi <scope> switch` nothing it was not already going to do - the
/// target's own switch command resolves a store path positional as an
/// installable that needs no further evaluation, then runs the exact
/// register-and-activate sequence it would for an interactive switch.
/// `Scope::sources_verb` picks `system` over naming `nixos`/`darwin`
/// explicitly, which is what lets one push target either kind without
/// this orchestrator needing to know which OS it runs.
///
/// [`ProfileKind::Custom`] and [`ProfileKind::Noop`] have no
/// corresponding `xi` entrypoint and keep the lower-level commands they
/// always have: a user-supplied script, or a bare profile registration
/// with no activation at all. `ProfileKind::Custom`'s `activate_command`
/// is a template fixed at discovery time, before any store path exists,
/// so it carries a [`discover::STORE_PATH_PLACEHOLDER`]
/// (`TargetSource::profile_kind` populates it for `SystemConfigs`
/// targets) that gets substituted here with the real store path.
#[must_use]
fn activation_script(
  target: &PushTarget,
  store_path: &str,
  scope: Scope,
  origin_host: &str,
) -> String {
  match &target.kind {
    ProfileKind::NixOS | ProfileKind::Darwin | ProfileKind::HomeManager => {
      format!(
        "XI_APPLIED_ORIGIN=push:{origin_host} xi {} switch {store_path}",
        scope.sources_verb()
      )
    },
    ProfileKind::Custom { activate_command } => {
      activate_command.replace(discover::STORE_PATH_PLACEHOLDER, store_path)
    },
    ProfileKind::Noop => {
      format!(
        "nix-env --profile {} --set {store_path}",
        target.profile_path
      )
    },
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::sync::Mutex;

  use tempfile::TempDir;
  use xi_core::sources::{Scope, SourcesId};
  use xi_sources_daemon::control::{AdvanceNow, AdvanceReply};

  use super::*;
  use crate::push::{
    args::{CopyVia as PushCopyVia, PushBackend, SourcesTransport},
    engine::{Outcome, TargetAddr, shell_quote},
  };

  /// Answers flake evaluation from a fixture table keyed by attr, the
  /// same shape `bdd_push_builtin.rs` uses.
  struct FixtureNix {
    outputs:     Vec<(&'static str, serde_json::Value)>,
    built:       Mutex<Vec<String>>,
    build_fails: bool,
  }

  impl FixtureNix {
    fn single_nixos_host(name: &'static str) -> Self {
      Self {
        outputs:     vec![(".#nixosConfigurations", serde_json::json!([name]))],
        built:       Mutex::new(Vec::new()),
        build_fails: false,
      }
    }
  }

  impl Nix for FixtureNix {
    fn eval_json(
      &self,
      installable: &str,
      _apply: Option<&str>,
    ) -> Result<Option<serde_json::Value>> {
      Ok(
        self
          .outputs
          .iter()
          .find(|(attr, _)| *attr == installable)
          .map(|(_, value)| value.clone()),
      )
    }

    fn build(&self, installable: &str) -> Result<String> {
      if self.build_fails {
        bail!("build failed for {installable}");
      }
      self
        .built
        .lock()
        .expect("built lock")
        .push(installable.to_owned());
      Ok(format!("/nix/store/fake-{installable}"))
    }
  }

  /// Answers every SSH-family call a single-host push makes, the same
  /// fixture shape `bdd_push_builtin.rs` uses.
  #[derive(Default)]
  struct FixtureTransport {
    run_calls:        Mutex<Vec<RemoteCommand>>,
    copy_calls:       Mutex<Vec<(String, String)>>,
    secret_calls:     Mutex<Vec<String>>,
    sync_calls:       Mutex<Vec<String>>,
    advance_calls:    Mutex<Vec<String>>,
    activation_fails: bool,
  }

  impl Transport for FixtureTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      cmd: &RemoteCommand,
    ) -> Result<Outcome> {
      self
        .run_calls
        .lock()
        .expect("run_calls lock")
        .push(cmd.clone());
      if cmd.script == "true" {
        return Ok(Outcome::ok(""));
      }
      if cmd.script.starts_with("df -kP") {
        return Ok(Outcome::ok(
          "/dev/sda1 100000000 10000000 90000000 11% /nix",
        ));
      }
      if cmd.script == "sudo -n true" {
        return Ok(Outcome::ok(""));
      }
      if cmd.script.starts_with("command -v") {
        return Ok(Outcome::ok(""));
      }
      if cmd.script.contains("xi system switch") && self.activation_fails {
        return Ok(Outcome::failed("switch-to-configuration failed"));
      }
      Ok(Outcome::ok(""))
    }

    fn copy_closure(
      &self,
      target: &TargetAddr,
      store_path: &str,
      _via: PushCopyVia,
      _retry_attempts: u32,
    ) -> Result<()> {
      self
        .copy_calls
        .lock()
        .expect("copy_calls lock")
        .push((target.name.clone(), store_path.to_owned()));
      Ok(())
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      local: &std::path::Path,
      spec: &SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      self
        .secret_calls
        .lock()
        .expect("secret_calls lock")
        .push(format!("{}:{}", spec.name, local.display()));
      Ok(())
    }

    fn sync_dir(
      &self,
      _target: &TargetAddr,
      local: &std::path::Path,
      remote: &str,
      _excludes: &[&str],
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      self
        .sync_calls
        .lock()
        .expect("sync_calls lock")
        .push(format!("{}->{remote}", local.display()));
      Ok(())
    }

    fn advance_now(
      &self,
      target: &TargetAddr,
      _scope: Scope,
      request: &AdvanceNow,
    ) -> Result<AdvanceReply> {
      self
        .advance_calls
        .lock()
        .expect("advance_calls lock")
        .push(target.name.clone());
      Ok(AdvanceReply::Advanced {
        new_head: request.to_sha.clone(),
      })
    }
  }

  /// Wraps a [`FixtureTransport`] so `advance_now` fails the way
  /// [`super::engine::SshTransport::advance_now`] does when nothing is
  /// listening on the target's control socket: the local `ssh` spawns
  /// fine but the daemon side refuses the connection, which
  /// `SshTransport` reports as a [`super::engine::TargetPullUnreachable`]
  /// naming `--sources-transport operator-copy` as the fallback.
  /// Everything else delegates to the wrapped fixture so preflight and
  /// secret upload still succeed, isolating the failure to the
  /// target-pull request itself.
  struct ConnectionRefusedAdvanceNow(FixtureTransport);

  impl Transport for ConnectionRefusedAdvanceNow {
    fn run(&self, target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
      self.0.run(target, cmd)
    }

    fn copy_closure(
      &self,
      target: &TargetAddr,
      store_path: &str,
      via: PushCopyVia,
      retry_attempts: u32,
    ) -> Result<()> {
      self.0.copy_closure(target, store_path, via, retry_attempts)
    }

    fn upload_secret(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      spec: &SecretSpec,
      interactive_sudo: bool,
      retry_attempts: u32,
    ) -> Result<()> {
      self
        .0
        .upload_secret(target, local, spec, interactive_sudo, retry_attempts)
    }

    fn sync_dir(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      remote: &str,
      excludes: &[&str],
      interactive_sudo: bool,
      retry_attempts: u32,
    ) -> Result<()> {
      self.0.sync_dir(
        target,
        local,
        remote,
        excludes,
        interactive_sudo,
        retry_attempts,
      )
    }

    fn advance_now(
      &self,
      target: &TargetAddr,
      _scope: Scope,
      _request: &AdvanceNow,
    ) -> Result<AdvanceReply> {
      Err(eyre!(
        "target-pull on {} is unreachable (ctl relay exited with exit \
         status: 255: ssh: connect to host {} port 22: Connection \
         refused); fall back to --sources-transport operator-copy",
        target.name,
        target.ssh_host,
      ))
    }
  }

  /// Wraps a [`FixtureTransport`] to fail the rebuild-tool preflight
  /// check specifically, for a scenario that needs preflight to fail
  /// without touching the SSH connectivity probe the way
  /// `ssh_unreachable` scenarios elsewhere do.
  struct NoRebuildTool(FixtureTransport);

  impl Transport for NoRebuildTool {
    fn run(&self, target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
      if cmd.script.starts_with("command -v") {
        return Ok(Outcome::failed("not found"));
      }
      self.0.run(target, cmd)
    }

    fn copy_closure(
      &self,
      target: &TargetAddr,
      store_path: &str,
      via: PushCopyVia,
      retry_attempts: u32,
    ) -> Result<()> {
      self.0.copy_closure(target, store_path, via, retry_attempts)
    }

    fn upload_secret(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      spec: &SecretSpec,
      interactive_sudo: bool,
      retry_attempts: u32,
    ) -> Result<()> {
      self
        .0
        .upload_secret(target, local, spec, interactive_sudo, retry_attempts)
    }

    fn sync_dir(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      remote: &str,
      excludes: &[&str],
      interactive_sudo: bool,
      retry_attempts: u32,
    ) -> Result<()> {
      self.0.sync_dir(
        target,
        local,
        remote,
        excludes,
        interactive_sudo,
        retry_attempts,
      )
    }
  }

  /// A scratch [`SourcesContext`] pointing at a real tempdir so
  /// `sources_dir`-relative paths (secrets) resolve to something that
  /// exists.
  fn fixture_context() -> (TempDir, SourcesContext) {
    let root = TempDir::new().expect("tempdir");
    let sources_dir = root.path().join("sources");
    std::fs::create_dir_all(&sources_dir).expect("sources dir");
    let context = SourcesContext::load(
      SourcesId::new(Scope::System, "https://example/repo"),
      sources_dir,
      root.path().join("state"),
    )
    .expect("load context");
    (root, context)
  }

  /// Like [`fixture_context`], but `sources_dir` is a real `git init`'d
  /// repo with one commit, so [`xi_core::sources::shadow::head_sha`]
  /// (what `--mode target-pull` resolves `to_sha` from) has a real `HEAD`
  /// to read instead of erroring out on "not a repository".
  fn fixture_context_with_git_sources() -> (TempDir, SourcesContext) {
    let root = TempDir::new().expect("tempdir");
    let sources_dir = root.path().join("sources");
    std::fs::create_dir_all(&sources_dir).expect("sources dir");
    git_repo(&sources_dir);
    let context = SourcesContext::load(
      SourcesId::new(Scope::System, "https://example/repo"),
      sources_dir,
      root.path().join("state"),
    )
    .expect("load context");
    (root, context)
  }

  fn args(mode: PushMode) -> PushArgs {
    PushArgs {
      mode: Some(mode),
      ..args_with_mode(None)
    }
  }

  fn args_with_mode(mode: Option<PushMode>) -> PushArgs {
    PushArgs {
      targets: vec!["web1".to_owned()],
      on: None,
      backend: PushBackend::Auto,
      phase: "activate".to_owned(),
      mode,
      remote_build: None,
      sources_transport: None,
      retry: None,
      magic_rollback: 0,
      parallel: 1,
      interactive_sudo: false,
      copy_via: PushCopyVia::Local,
      fail_fast: false,
      force_dirty: false,
      build_strategy: BuildStrategy::Interleaved,
      build_only: false,
      from_manifest: None,
      scope: super::super::args::PushScope::System,
      reattach: None,
      notify: None,
    }
  }

  #[test]
  fn a_single_host_closure_push_builds_copies_and_activates() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].name, "web1");
    assert_eq!(transport.copy_calls.lock().expect("lock").len(), 1);
    let last = transport
      .run_calls
      .lock()
      .expect("lock")
      .last()
      .expect("ran something")
      .script
      .clone();
    assert!(last.contains("XI_APPLIED_ORIGIN=push:"), "got: {last}");
    assert!(last.contains("xi system switch"), "got: {last}");
    assert!(
      last.contains("/nix/store/fake-"),
      "activation must point at the copied closure, not a flake ref: {last}"
    );
  }

  #[test]
  fn an_unreachable_target_never_mutates() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = NoRebuildTool(FixtureTransport::default());
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(!report.ok());
    assert_eq!(transport.0.copy_calls.lock().expect("lock").len(), 0);
  }

  #[test]
  fn mode_sources_syncs_and_switches_without_a_local_build() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert!(
      nix.built.lock().expect("lock").is_empty(),
      "closure mode only"
    );
    assert_eq!(transport.sync_calls.lock().expect("lock").len(), 1);
    assert_eq!(transport.copy_calls.lock().expect("lock").len(), 0);
  }

  #[test]
  fn mode_sources_wraps_the_remote_rebuild_in_a_transient_systemd_unit() {
    // xi-a5e.4: the remote-build path's rebuild invocation runs through
    // `systemd-run --wait --pipe --collect`, not as a bare foreground
    // SSH command, so it survives an SSH hiccup and leaves no unit file
    // behind. Driven through the real orchestrator (not
    // `mode_sources::switch` directly) so this is the end-to-end proof
    // the wrap actually reaches `--mode sources` pushes, not just the
    // module that builds the string.
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    let switch_call = transport
      .run_calls
      .lock()
      .expect("lock")
      .last()
      .expect("the remote rebuild ran")
      .script
      .clone();
    assert!(
      switch_call.starts_with(
        "systemd-run --wait --pipe --collect --unit=xi-push-web1-"
      ),
      "got: {switch_call}"
    );
    assert!(switch_call.contains(" -- "), "got: {switch_call}");
    assert!(
      switch_call.contains("XI_APPLIED_ORIGIN=push:"),
      "got: {switch_call}"
    );
    assert!(
      switch_call.ends_with("xi system switch /etc/xi/sources#web1"),
      "got: {switch_call}"
    );
  }

  #[test]
  fn a_successful_mode_sources_push_leaves_no_receipt_behind() {
    // xi-enwe.1.2: this process observed a terminal (successful) outcome
    // for the unit it started, so the receipt that named it before the
    // SSH call must be gone again once `execute` returns.
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    let receipts_dir = context.state_dir.join("push-receipts");
    let leftover: Vec<_> = std::fs::read_dir(&receipts_dir)
      .map(|entries| entries.filter_map(Result::ok).collect())
      .unwrap_or_default();
    assert!(
      leftover.is_empty(),
      "expected no receipts left in {}: {leftover:?}",
      receipts_dir.display()
    );
  }

  #[test]
  fn a_successful_mode_sources_push_runs_notify_exactly_once() {
    // xi-enwe.3.1: --notify fires the moment this process observes (and
    // removes the receipt for) a successful outcome.
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let marker = context.state_dir.join("notified");

    let mut push_args = args(PushMode::Sources);
    push_args.notify =
      Some(format!("touch {}", shell_quote(&marker.display().to_string())));

    let report = execute(
      &push_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert!(
      marker.exists(),
      "expected --notify to have run: {}",
      marker.display()
    );
  }

  /// Wraps a [`FixtureTransport`], watching for the `systemd-run`-wrapped
  /// switch call specifically (not the earlier preflight/sync calls
  /// `run`/`sync_dir` also make), so a test can observe receipt state at
  /// the exact moment that call fires.
  struct SwitchObservingTransport {
    inner:                  FixtureTransport,
    state_dir:              std::path::PathBuf,
    receipt_seen_at_switch: Mutex<Option<bool>>,
    fail_switch_with_error: bool,
    /// When set, the `systemd-run`-wrapped switch call answers with a
    /// confirmed non-zero remote exit (an `Ok(Outcome::failed(..))`)
    /// instead of `fail_switch_with_error`'s `Err` - the SSH round-trip
    /// completed and the remote itself reported failure, as opposed to
    /// an ambiguous dropped connection.
    fail_switch_with_confirmed_failure: bool,
  }

  impl SwitchObservingTransport {
    fn receipts_present(&self) -> bool {
      std::fs::read_dir(self.state_dir.join("push-receipts"))
        .is_ok_and(|mut entries| entries.find_map(Result::ok).is_some())
    }
  }

  impl Transport for SwitchObservingTransport {
    fn run(&self, target: &TargetAddr, cmd: &RemoteCommand) -> Result<Outcome> {
      if cmd.script.starts_with("systemd-run") {
        *self.receipt_seen_at_switch.lock().expect("lock") =
          Some(self.receipts_present());
        if self.fail_switch_with_error {
          bail!("simulated disconnect: ssh connection dropped");
        }
        if self.fail_switch_with_confirmed_failure {
          return Ok(Outcome::failed("switch-to-configuration failed"));
        }
      }
      self.inner.run(target, cmd)
    }

    fn copy_closure(
      &self,
      target: &TargetAddr,
      store_path: &str,
      via: PushCopyVia,
      retry_attempts: u32,
    ) -> Result<()> {
      self.inner.copy_closure(target, store_path, via, retry_attempts)
    }

    fn upload_secret(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      spec: &SecretSpec,
      interactive_sudo: bool,
      retry_attempts: u32,
    ) -> Result<()> {
      self
        .inner
        .upload_secret(target, local, spec, interactive_sudo, retry_attempts)
    }

    fn sync_dir(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      remote: &str,
      excludes: &[&str],
      interactive_sudo: bool,
      retry_attempts: u32,
    ) -> Result<()> {
      self.inner.sync_dir(
        target,
        local,
        remote,
        excludes,
        interactive_sudo,
        retry_attempts,
      )
    }
  }

  #[test]
  fn the_receipt_is_written_before_the_switch_transport_call_fires() {
    // xi-enwe.1.3: the receipt must exist on disk by the time the
    // (mocked) transport call that starts the remote unit runs, not
    // merely by the time `execute` returns.
    let nix = FixtureNix::single_nixos_host("web1");
    let (_root, context) = fixture_context();
    let transport = SwitchObservingTransport {
      inner:                  FixtureTransport::default(),
      state_dir:              context.state_dir.clone(),
      receipt_seen_at_switch: Mutex::new(None),
      fail_switch_with_error: false,
      fail_switch_with_confirmed_failure: false,
    };
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(
      transport
        .receipt_seen_at_switch
        .lock()
        .expect("lock")
        .clone(),
      Some(true),
      "the receipt must already be on disk when the switch transport call \
       fires"
    );
  }

  #[test]
  fn a_simulated_disconnect_during_switch_leaves_the_receipt_in_place() {
    // xi-enwe.1.3: when the transport call that starts the remote unit
    // returns an error (a dropped connection, not an observed remote
    // failure), this process has not seen a terminal outcome for the
    // unit - so the receipt must survive for a later `--reattach`.
    let nix = FixtureNix::single_nixos_host("web1");
    let (_root, context) = fixture_context();
    let transport = SwitchObservingTransport {
      inner:                  FixtureTransport::default(),
      state_dir:              context.state_dir.clone(),
      receipt_seen_at_switch: Mutex::new(None),
      fail_switch_with_error: true,
      fail_switch_with_confirmed_failure: false,
    };
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(!report.ok(), "the simulated disconnect must be reported");
    let leftover = receipt::list(&context.state_dir).expect("list receipts");
    assert_eq!(
      leftover.len(),
      1,
      "a simulated disconnect must leave the receipt behind: {leftover:?}",
    );
  }

  #[test]
  fn a_confirmed_switch_failure_runs_notify_and_still_leaves_the_receipt_in_place()
   {
    // xi-bnow finding 2: `switch_with_receipt`'s Err branch used to
    // propagate a confirmed (not merely ambiguous) remote failure
    // without ever firing `--notify` - an operator who stayed connected
    // through a failing `--mode sources` push got no notification at
    // all, the single most common failure mode.
    let nix = FixtureNix::single_nixos_host("web1");
    let (_root, context) = fixture_context();
    let marker = context.state_dir.join("notified");
    let transport = SwitchObservingTransport {
      inner:                  FixtureTransport::default(),
      state_dir:              context.state_dir.clone(),
      receipt_seen_at_switch: Mutex::new(None),
      fail_switch_with_error: false,
      fail_switch_with_confirmed_failure: true,
    };
    let secrets = BTreeMap::new();
    let mut push_args = args(PushMode::Sources);
    push_args.notify =
      Some(format!("touch {}", shell_quote(&marker.display().to_string())));

    let report = execute(
      &push_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(!report.ok(), "the confirmed remote failure must be reported");
    assert!(
      marker.exists(),
      "expected --notify to fire for a confirmed synchronous switch failure"
    );
    let leftover = receipt::list(&context.state_dir).expect("list receipts");
    assert_eq!(
      leftover.len(),
      1,
      "a confirmed failure must still leave the receipt behind for operator \
       acknowledgment, exactly like reattach's own failure branches: \
       {leftover:?}",
    );
  }

  /// A [`Transport`] that answers any `systemctl show` query with a
  /// terminal "succeeded" reply, for the reattach half of the
  /// disconnect-then-reattach round trip below - the receipt's unit
  /// name is only known once the disconnect has actually written one,
  /// so this does not need to match it by name.
  struct ReattachSucceedsTransport;

  impl Transport for ReattachSucceedsTransport {
    fn run(
      &self,
      _target: &TargetAddr,
      _cmd: &RemoteCommand,
    ) -> Result<Outcome> {
      Ok(Outcome::ok(
        "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\n\
         ExecMainCode=1\n",
      ))
    }

    fn copy_closure(
      &self,
      _target: &TargetAddr,
      _store_path: &str,
      _via: PushCopyVia,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("reattach never copies a closure")
    }

    fn upload_secret(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _spec: &SecretSpec,
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("reattach never uploads a secret")
    }

    fn sync_dir(
      &self,
      _target: &TargetAddr,
      _local: &std::path::Path,
      _remote: &str,
      _excludes: &[&str],
      _interactive_sudo: bool,
      _retry_attempts: u32,
    ) -> Result<()> {
      unreachable!("reattach never syncs a directory")
    }
  }

  #[test]
  fn a_simulated_disconnect_then_reattach_to_success_round_trips_the_receipt()
   {
    // xi-enwe.2.5: the full lifecycle a disconnect is supposed to
    // survive - write the receipt before the switch call, lose the
    // connection so it is left behind, then `--reattach` to a unit that
    // actually succeeded must remove it again.
    let nix = FixtureNix::single_nixos_host("web1");
    let (_root, context) = fixture_context();
    let transport = SwitchObservingTransport {
      inner:                  FixtureTransport::default(),
      state_dir:              context.state_dir.clone(),
      receipt_seen_at_switch: Mutex::new(None),
      fail_switch_with_error: true,
      fail_switch_with_confirmed_failure: false,
    };
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");
    assert!(!report.ok(), "the simulated disconnect must be reported");

    let receipts = receipt::list(&context.state_dir).expect("list receipts");
    assert_eq!(
      receipts.len(),
      1,
      "the disconnect must have left exactly one receipt behind"
    );
    let push_id = receipts[0].push_id.clone();

    reattach::run(&push_id, &context.state_dir, &ReattachSucceedsTransport)
      .expect("reattaching to a unit that actually succeeded is not an error");

    assert!(
      receipt::read(&context.state_dir, &push_id)
        .expect("read")
        .is_none(),
      "reattaching to a succeeded unit must remove the receipt the \
       disconnect left behind"
    );
  }

  #[test]
  fn with_no_mode_flag_a_toml_remote_build_default_picks_sources_mode() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      remote_build: Some(true),
      ..PushConfig::default()
    };

    let report = execute(
      &args_with_mode(None),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert!(
      nix.built.lock().expect("lock").is_empty(),
      "the ladder resolved remote-build=true, so this must behave as --mode \
       sources and never build locally"
    );
    assert_eq!(transport.sync_calls.lock().expect("lock").len(), 1);
  }

  #[test]
  fn a_toml_sources_transport_default_of_target_pull_skips_copy_closure_and_sync_dir()
   {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context_with_git_sources();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      sources_transport: Some(SourcesTransport::TargetPull),
      ..PushConfig::default()
    };

    let report = execute(
      &args_with_mode(None),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(transport.advance_calls.lock().expect("lock").len(), 1);
    assert_eq!(transport.copy_calls.lock().expect("lock").len(), 0);
    assert_eq!(transport.sync_calls.lock().expect("lock").len(), 0);
    assert!(nix.built.lock().expect("lock").is_empty());
  }

  #[test]
  fn a_target_pull_with_no_daemon_listening_refuses_rather_than_switching_to_operator_copy()
   {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = ConnectionRefusedAdvanceNow(FixtureTransport::default());
    let (_root, context) = fixture_context_with_git_sources();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      sources_transport: Some(SourcesTransport::TargetPull),
      ..PushConfig::default()
    };

    let report = execute(
      &args_with_mode(None),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("execute reports a per-target failure rather than erroring itself");

    assert!(!report.ok(), "{:?}", report.results);
    let failures = report.failures();
    assert_eq!(failures.len(), 1);
    let message = failures[0]
      .outcome
      .as_ref()
      .expect_err("target-pull must fail when the control socket refuses the connection")
      .to_string();
    assert!(
      message.contains("target-pull on web1 is unreachable"),
      "got: {message}"
    );
    assert!(
      message.contains("Connection refused"),
      "the connection-refused cause must not be swallowed; got: {message}"
    );
    assert!(
      message.contains("--sources-transport operator-copy"),
      "the error must name the fallback for the operator to retry with; got: {message}"
    );
    assert_eq!(
      transport.0.copy_calls.lock().expect("lock").len(),
      0,
      "an unreachable target-pull must not silently fall back to copy_closure"
    );
    assert_eq!(
      transport.0.sync_calls.lock().expect("lock").len(),
      0,
      "an unreachable target-pull must not silently fall back to sync_dir"
    );
  }

  #[test]
  fn a_per_host_sources_transport_override_outranks_the_table_default() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context_with_git_sources();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      sources_transport: Some(SourcesTransport::OperatorCopy),
      hosts:             vec![super::super::config::PushHost {
        name:              "web1".to_owned(),
        ssh:               "web1".to_owned(),
        remote_build:      None,
        backend:           None,
        retry_attempts:    None,
        sources_transport: Some(SourcesTransport::TargetPull),
      }],
      ..PushConfig::default()
    };

    let report = execute(
      &args_with_mode(None),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(transport.advance_calls.lock().expect("lock").len(), 1);
  }

  #[test]
  fn an_explicit_mode_flag_overrides_the_toml_sources_transport_default() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      sources_transport: Some(SourcesTransport::TargetPull),
      ..PushConfig::default()
    };

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
      None,
      &_root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(
      transport.advance_calls.lock().expect("lock").len(),
      0,
      "--mode closure must win over the ladder's sources-transport=target-pull"
    );
    assert_eq!(nix.built.lock().expect("lock").len(), 1);
  }

  #[test]
  fn an_explicit_mode_flag_overrides_the_toml_remote_build_default() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      remote_build: Some(true),
      ..PushConfig::default()
    };

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(
      transport.sync_calls.lock().expect("lock").len(),
      0,
      "--mode closure must win over the ladder's remote-build=true"
    );
    assert_eq!(nix.built.lock().expect("lock").len(), 1);
  }

  #[test]
  fn with_no_mode_flag_the_nix_module_default_picks_sources_mode() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args_with_mode(None),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      Some(true),
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(transport.sync_calls.lock().expect("lock").len(), 1);
  }

  #[test]
  fn pre_activate_secrets_land_before_the_activation_script_runs() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let mut secrets = BTreeMap::new();
    secrets.insert("ssh-key".to_owned(), SecretSpec {
      name:        "ssh-key".to_owned(),
      source:      std::path::PathBuf::from("./ssh-key"),
      destination: std::path::PathBuf::from("/run/keys/ssh"),
      owner:       "root".to_owned(),
      group:       None,
      mode:        "0600".to_owned(),
      upload_at:   UploadAt::PreActivate,
    });

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(report.ok(), "{:?}", report.results);
    let calls = transport.secret_calls.lock().expect("lock").clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].starts_with("ssh-key:"));
  }

  #[test]
  fn a_failed_activation_is_reported_but_other_targets_still_run() {
    let nix = FixtureNix {
      outputs:     vec![(
        ".#nixosConfigurations",
        serde_json::json!(["web1", "web2"]),
      )],
      built:       Mutex::new(Vec::new()),
      build_fails: false,
    };
    let transport = FixtureTransport {
      activation_fails: true,
      ..FixtureTransport::default()
    };
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let mut push_args = args(PushMode::Closure);
    push_args.targets = Vec::new();

    let report = execute(
      &push_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(!report.ok());
    assert_eq!(report.results.len(), 2);
    assert_eq!(report.failures().len(), 2);
  }

  #[test]
  fn fail_fast_skips_targets_queued_after_the_first_failure() {
    let nix = FixtureNix {
      outputs:     vec![(
        ".#nixosConfigurations",
        serde_json::json!(["web1", "web2"]),
      )],
      built:       Mutex::new(Vec::new()),
      build_fails: false,
    };
    let transport = FixtureTransport {
      activation_fails: true,
      ..FixtureTransport::default()
    };
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let mut push_args = args(PushMode::Closure);
    push_args.targets = Vec::new();
    push_args.fail_fast = true;
    push_args.parallel = 1;

    let report = execute(
      &push_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("executes");

    assert!(!report.ok());
    assert!(
      report.failures().iter().any(|r| {
        format!("{}", r.outcome.as_ref().expect_err("a fail-fast failure"))
          .contains("skipped")
      }),
      "with --parallel 1 the second target must observe the first's failure: \
       {:?}",
      report.results
    );
  }

  /// `git init` a scratch repo with one committed file, so
  /// `inspect_working_tree` has a real `.git` to read instead of
  /// erroring out on "not a repository".
  fn git_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
      std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .status()
        .expect("git invocation")
    };
    assert!(run(&["init", "--quiet"]).success());
    std::fs::write(dir.join("flake.nix"), "{}\n").expect("write");
    assert!(run(&["add", "."]).success());
    assert!(run(&["commit", "--quiet", "-m", "init"]).success());
  }

  fn context_with_working_dir(
    working_dir: std::path::PathBuf,
  ) -> SourcesContext {
    SourcesContext {
      sources_id:          SourcesId::new(
        Scope::System,
        "https://example/repo",
      ),
      sources_dir:         working_dir
        .parent()
        .expect("parent")
        .join("sources"),
      state_dir:           working_dir.parent().expect("parent").join("state"),
      working_dir:         Some(working_dir),
      refs:                xi_core::sources::RefLineage::default(),
      unknown_state_files: Vec::new(),
    }
  }

  #[test]
  fn a_clean_working_dir_does_not_refuse() {
    let root = TempDir::new().expect("tempdir");
    let working_dir = root.path().join("work");
    std::fs::create_dir_all(&working_dir).expect("mkdir");
    git_repo(&working_dir);

    let context = context_with_working_dir(working_dir);
    assert!(refuse_if_dirty(&context, false).is_ok());
  }

  #[test]
  fn a_dirty_working_dir_refuses_unless_forced() {
    let root = TempDir::new().expect("tempdir");
    let working_dir = root.path().join("work");
    std::fs::create_dir_all(&working_dir).expect("mkdir");
    git_repo(&working_dir);
    std::fs::write(working_dir.join("flake.nix"), "{ edited = true; }\n")
      .expect("dirty the tree");

    let context = context_with_working_dir(working_dir);

    let err = refuse_if_dirty(&context, false).expect_err("dirty tree refuses");
    assert!(format!("{err}").contains("xi push"), "got: {err}");

    assert!(
      refuse_if_dirty(&context, true).is_ok(),
      "--force-dirty must override the refusal"
    );
  }

  fn push_target(kind: ProfileKind) -> PushTarget {
    PushTarget {
      addr: TargetAddr {
        name:     "web1".to_owned(),
        ssh_host: "web1".to_owned(),
        ssh_opts: Vec::new(),
      },
      source: crate::push::discover::TargetSource::NixosConfigurations,
      tags: Vec::new(),
      installable: String::new(),
      kind,
      profile_path: "/nix/var/nix/profiles/system".to_owned(),
      rebuild_tool: "nixos-rebuild",
    }
  }

  #[test]
  fn nixos_darwin_and_home_manager_activate_via_the_remote_switch_entrypoint() {
    for kind in [
      ProfileKind::NixOS,
      ProfileKind::Darwin,
      ProfileKind::HomeManager,
    ] {
      let target = push_target(kind);
      let script = activation_script(
        &target,
        "/nix/store/fake-web1",
        Scope::System,
        "operator-host",
      );
      assert_eq!(
        script,
        "XI_APPLIED_ORIGIN=push:operator-host xi system switch \
         /nix/store/fake-web1"
      );
    }
  }

  #[test]
  fn the_user_scope_resolves_to_the_home_switch_verb() {
    let target = push_target(ProfileKind::HomeManager);
    let script = activation_script(
      &target,
      "/nix/store/fake-web1",
      Scope::User,
      "operator-host",
    );
    assert_eq!(
      script,
      "XI_APPLIED_ORIGIN=push:operator-host xi home switch \
       /nix/store/fake-web1"
    );
  }

  #[test]
  fn a_custom_profile_kind_keeps_its_own_activate_command() {
    let target = push_target(ProfileKind::Custom {
      activate_command: "/run/current-system/activate".to_owned(),
    });
    let script = activation_script(
      &target,
      "/nix/store/fake-web1",
      Scope::System,
      "operator-host",
    );
    assert_eq!(script, "/run/current-system/activate");
  }

  #[test]
  fn a_noop_profile_kind_only_registers_the_profile() {
    let target = push_target(ProfileKind::Noop);
    let script = activation_script(
      &target,
      "/nix/store/fake-web1",
      Scope::System,
      "operator-host",
    );
    assert_eq!(
      script,
      "nix-env --profile /nix/var/nix/profiles/system --set \
       /nix/store/fake-web1"
    );
  }

  #[test]
  fn no_recorded_working_dir_never_refuses() {
    let root = TempDir::new().expect("tempdir");
    let context = SourcesContext {
      sources_id:          SourcesId::new(
        Scope::System,
        "https://example/repo",
      ),
      sources_dir:         root.path().join("sources"),
      state_dir:           root.path().join("state"),
      working_dir:         None,
      refs:                xi_core::sources::RefLineage::default(),
      unknown_state_files: Vec::new(),
    };
    assert!(refuse_if_dirty(&context, false).is_ok());
  }

  // -- unique_push_id: collision regeneration (xi-bnow finding 3) --------
  //
  // A real collision only happens for one in roughly 16M draws of
  // `mode_sources::push_id`'s random suffix, far too rare to trigger
  // through its actual randomness in a test - these drive
  // `unique_push_id_with` directly with a scripted generator instead, so
  // the retry/give-up logic itself is exercised deterministically.

  #[test]
  fn unique_push_id_accepts_the_first_candidate_when_nothing_collides() {
    let tmp = TempDir::new().expect("tempdir");
    let mut calls = 0;
    let id = unique_push_id_with(tmp.path(), || {
      calls += 1;
      "web1-1700000000-aaaaaa".to_owned()
    })
    .expect("no collision");
    assert_eq!(id, "web1-1700000000-aaaaaa");
    assert_eq!(calls, 1, "must not regenerate when there is no collision");
  }

  #[test]
  fn unique_push_id_regenerates_past_a_colliding_candidate() {
    let tmp = TempDir::new().expect("tempdir");
    let taken = receipt::PushReceipt::new(
      "web1",
      "xi-push-web1-1700000000-aaaaaa",
      Scope::System,
    );
    receipt::write(tmp.path(), "web1-1700000000-aaaaaa", &taken)
      .expect("seed collision");

    let mut candidates = vec![
      "web1-1700000000-aaaaaa".to_owned(),
      "web1-1700000000-bbbbbb".to_owned(),
    ]
    .into_iter();
    let id =
      unique_push_id_with(tmp.path(), || candidates.next().expect("enough candidates"))
        .expect("regenerates past the collision");
    assert_eq!(
      id, "web1-1700000000-bbbbbb",
      "the first (colliding) candidate must be rejected and the second accepted"
    );
  }

  #[test]
  fn unique_push_id_regenerates_past_a_notified_marker_with_no_receipt() {
    // The collision check must also catch a push id whose receipt was
    // already removed but whose `--notify`-fired marker is still on
    // disk - reusing it would make a brand new push's eventual failure
    // silently skip `--notify` because the old marker already exists.
    let tmp = TempDir::new().expect("tempdir");
    let receipt = receipt::PushReceipt::new(
      "web1",
      "xi-push-web1-1700000000-aaaaaa",
      Scope::System,
    );
    receipt::write(tmp.path(), "web1-1700000000-aaaaaa", &receipt).expect("seed");
    receipt::mark_notified_and_notify(tmp.path(), "web1-1700000000-aaaaaa", &receipt)
      .expect("mark notified");
    assert!(
      receipt::remove(tmp.path(), "web1-1700000000-aaaaaa").expect("remove"),
      "the receipt itself must be gone, leaving only the marker"
    );

    let mut candidates = vec![
      "web1-1700000000-aaaaaa".to_owned(),
      "web1-1700000000-bbbbbb".to_owned(),
    ]
    .into_iter();
    let id =
      unique_push_id_with(tmp.path(), || candidates.next().expect("enough candidates"))
        .expect("regenerates past the stale marker");
    assert_eq!(id, "web1-1700000000-bbbbbb");
  }

  #[test]
  fn unique_push_id_gives_up_after_exhausting_every_attempt() {
    let tmp = TempDir::new().expect("tempdir");
    let taken = receipt::PushReceipt::new(
      "web1",
      "xi-push-web1-1700000000-aaaaaa",
      Scope::System,
    );
    receipt::write(tmp.path(), "web1-1700000000-aaaaaa", &taken)
      .expect("seed collision");

    let mut calls = 0;
    let err = unique_push_id_with(tmp.path(), || {
      calls += 1;
      "web1-1700000000-aaaaaa".to_owned()
    })
    .expect_err("every attempt collides");
    assert!(
      format!("{err}").contains("could not generate a push id"),
      "got: {err}"
    );
    assert_eq!(
      calls,
      u32::from(MAX_PUSH_ID_ATTEMPTS),
      "must try exactly MAX_PUSH_ID_ATTEMPTS times before giving up"
    );
  }

  // -- build_batch: the upfront build phase a batched strategy runs ----

  /// A `PushMode::Closure`-shaped target, built directly rather than
  /// through `discover::discover`, since `build_batch` takes a
  /// `&[PushTarget]` slice rather than a flake to evaluate.
  fn closure_target(name: &str) -> PushTarget {
    PushTarget {
      addr:         TargetAddr {
        name:     name.to_owned(),
        ssh_host: name.to_owned(),
        ssh_opts: Vec::new(),
      },
      source:       discover::TargetSource::NixosConfigurations,
      tags:         Vec::new(),
      installable:  format!(".#nixosConfigurations.{name}"),
      kind:         ProfileKind::NixOS,
      profile_path: "/nix/var/nix/profiles/system".to_owned(),
      rebuild_tool: "nixos-rebuild",
    }
  }

  #[test]
  fn build_batch_builds_every_closure_target_before_returning() {
    let nix = FixtureNix::single_nixos_host("web1");
    let targets = vec![closure_target("web1"), closure_target("web2")];
    let limits = PhaseLimits::new(2);

    let manifest = build_batch(
      &nix,
      &targets,
      &args(PushMode::Closure),
      &PushConfig::default(),
      None,
      &limits,
    )
    .expect("batched build succeeds");

    assert_eq!(manifest.len(), 2);
    assert_eq!(
      manifest["web1"],
      format!("/nix/store/fake-{}", targets[0].installable)
    );
    assert_eq!(
      manifest["web2"],
      format!("/nix/store/fake-{}", targets[1].installable)
    );
    assert_eq!(nix.built.lock().expect("built lock").len(), 2);
  }

  #[test]
  fn build_batch_skips_targets_the_ladder_resolves_to_sources_mode() {
    let nix = FixtureNix::single_nixos_host("web1");
    let targets = vec![closure_target("web1"), closure_target("web2")];
    let limits = PhaseLimits::new(2);
    // No `--mode` flag: each target's mode comes from the build-location
    // ladder. `web2`'s per-host override sends it to `--mode sources`,
    // so only `web1` should end up in the manifest.
    let inventory = PushConfig {
      remote_build:      None,
      backend:           None,
      retry_attempts:    None,
      sources_transport: None,
      hosts:             vec![config::PushHost {
        name:              "web2".to_owned(),
        ssh:               "web2".to_owned(),
        remote_build:      Some(true),
        backend:           None,
        retry_attempts:    None,
        sources_transport: None,
      }],
    };

    let manifest = build_batch(
      &nix,
      &targets,
      &args_with_mode(None),
      &inventory,
      None,
      &limits,
    )
    .expect("batched build succeeds");

    assert_eq!(manifest.len(), 1);
    assert!(manifest.contains_key("web1"));
    assert_eq!(nix.built.lock().expect("built lock").len(), 1);
  }

  #[test]
  fn build_batch_fails_on_the_first_build_failure() {
    let nix = FixtureNix {
      outputs:     Vec::new(),
      built:       Mutex::new(Vec::new()),
      build_fails: true,
    };
    let targets = vec![closure_target("web1")];
    let limits = PhaseLimits::new(1);

    let err = build_batch(
      &nix,
      &targets,
      &args(PushMode::Closure),
      &PushConfig::default(),
      None,
      &limits,
    )
    .expect_err("a failing build must fail the whole batch");
    assert!(format!("{err}").contains("web1"), "got: {err}");
  }

  // -- manifest serialization: write_manifest / read_manifest -----------

  #[test]
  fn write_then_read_manifest_round_trips() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("nested/push-manifest.json");
    let mut manifest = BTreeMap::new();
    manifest.insert("web1".to_owned(), "/nix/store/fake-web1".to_owned());
    manifest.insert("web2".to_owned(), "/nix/store/fake-web2".to_owned());

    write_manifest(&path, &manifest).expect("write");
    let loaded = read_manifest(&path).expect("read");
    assert_eq!(loaded, manifest);
  }

  #[test]
  fn reading_a_missing_manifest_names_the_path() {
    let err = read_manifest(std::path::Path::new(
      "/nonexistent/dir/push-manifest.json",
    ))
    .expect_err("missing file");
    assert!(
      format!("{err}").contains("push-manifest.json"),
      "got: {err}"
    );
  }

  #[test]
  fn an_incomplete_manifest_is_rejected_before_shipping_anything() {
    let targets = vec![closure_target("web1"), closure_target("web2")];
    let mut manifest = BTreeMap::new();
    manifest.insert("web1".to_owned(), "/nix/store/fake-web1".to_owned());

    let err = require_manifest_covers(
      &targets,
      &args(PushMode::Closure),
      &PushConfig::default(),
      None,
      &manifest,
    )
    .expect_err("web2 has no manifest entry");
    assert!(format!("{err}").contains("web2"), "got: {err}");
  }

  #[test]
  fn a_manifest_covering_every_closure_target_is_accepted() {
    let targets = vec![closure_target("web1")];
    let mut manifest = BTreeMap::new();
    manifest.insert("web1".to_owned(), "/nix/store/fake-web1".to_owned());

    require_manifest_covers(
      &targets,
      &args(PushMode::Closure),
      &PushConfig::default(),
      None,
      &manifest,
    )
    .expect("every closure target is covered");
  }

  // -- batched build-then-ship: --build-only, then --from-manifest ------

  #[test]
  fn build_only_builds_and_writes_a_manifest_but_ships_nothing() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let mut push_args = args(PushMode::Closure);
    push_args.build_strategy = BuildStrategy::Batched;
    push_args.build_only = true;

    let report = execute(
      &push_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect("build-only succeeds");

    assert!(report.ok(), "{:?}", report.results);
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].name, "web1");
    assert_eq!(nix.built.lock().expect("built lock").len(), 1);
    assert!(
      transport.copy_calls.lock().expect("lock").is_empty(),
      "--build-only must not copy anything"
    );
    assert!(
      transport.run_calls.lock().expect("lock").is_empty(),
      "--build-only must not activate anything"
    );

    let manifest_path = default_manifest_path(&context);
    let manifest = read_manifest(&manifest_path).expect("manifest written");
    assert_eq!(manifest.len(), 1);
    assert!(
      manifest["web1"].starts_with("/nix/store/fake-"),
      "got: {}",
      manifest["web1"]
    );
  }

  #[test]
  fn a_batched_build_then_ship_round_trip_never_rebuilds_on_the_ship_leg() {
    let nix = FixtureNix::single_nixos_host("web1");
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let manifest_path = default_manifest_path(&context);

    // Leg 1: `--build-strategy batched --build-only` builds web1 and
    // writes the manifest, as a separate invocation that ships nothing.
    let build_transport = FixtureTransport::default();
    let mut build_args = args(PushMode::Closure);
    build_args.build_strategy = BuildStrategy::Batched;
    build_args.build_only = true;

    let build_report = execute(
      &build_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &build_transport,
    )
    .expect("build-only leg succeeds");
    assert!(build_report.ok(), "{:?}", build_report.results);
    assert_eq!(nix.built.lock().expect("built lock").len(), 1);
    assert!(manifest_path.exists());

    // Leg 2: a separate invocation, `--from-manifest <path>`, ships web1
    // from the manifest's recorded store path - against the *same*
    // `FixtureNix`, so a second `nix.build` call here would show up in
    // its shared `built` log.
    let ship_transport = FixtureTransport::default();
    let mut ship_args = args(PushMode::Closure);
    ship_args.build_strategy = BuildStrategy::Batched;
    ship_args.from_manifest = Some(manifest_path);

    let ship_report = execute(
      &ship_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &ship_transport,
    )
    .expect("from-manifest leg succeeds");

    assert!(ship_report.ok(), "{:?}", ship_report.results);
    assert_eq!(
      nix.built.lock().expect("built lock").len(),
      1,
      "the ship leg must not call nix.build again"
    );
    assert_eq!(ship_transport.copy_calls.lock().expect("lock").len(), 1);
    let last = ship_transport
      .run_calls
      .lock()
      .expect("lock")
      .last()
      .expect("ran something")
      .script
      .clone();
    assert!(last.contains("XI_APPLIED_ORIGIN=push:"), "got: {last}");
    assert!(last.contains("xi system switch"), "got: {last}");
  }

  #[test]
  fn from_manifest_missing_a_target_refuses_before_shipping_anything() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let manifest_path = context.state_dir.join("empty-manifest.json");
    write_manifest(&manifest_path, &BTreeMap::new())
      .expect("write empty manifest");

    let mut push_args = args(PushMode::Closure);
    push_args.build_strategy = BuildStrategy::Batched;
    push_args.from_manifest = Some(manifest_path);

    let err = execute(
      &push_args,
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
      None,
      &root.path().join(".xi.toml"),
      &nix,
      &transport,
    )
    .expect_err("manifest missing web1");
    assert!(format!("{err}").contains("web1"), "got: {err}");
    assert!(transport.copy_calls.lock().expect("lock").is_empty());
    assert!(nix.built.lock().expect("built lock").is_empty());
  }

  /// xi-slej.3.2: `ProfileKind::Custom`'s `{store_path}` placeholder
  /// (populated by `TargetSource::profile_kind` for `SystemConfigs`
  /// targets) is substituted with the real store path, the same way
  /// `register` is assembled for every other kind.
  #[test]
  fn custom_activation_substitutes_the_store_path_placeholder() {
    let target = PushTarget {
      addr:         TargetAddr {
        name:     "web3".to_owned(),
        ssh_host: "web3".to_owned(),
        ssh_opts: Vec::new(),
      },
      source:       discover::TargetSource::SystemConfigs,
      tags:         Vec::new(),
      installable:  ".#systemConfigs.web3".to_owned(),
      kind:         ProfileKind::Custom {
        activate_command: "nix build --no-link --profile /p {store_path} && \
                           {store_path}/bin/activate"
          .to_owned(),
      },
      profile_path: "/p".to_owned(),
      rebuild_tool: "nix",
    };

    let script = activation_script(
      &target,
      "/nix/store/abc-web3",
      Scope::System,
      "operator-host",
    );

    assert_eq!(
      script,
      "nix build --no-link --profile /p /nix/store/abc-web3 && \
       /nix/store/abc-web3/bin/activate"
    );
    assert!(!script.contains("nixos-rebuild"));
    assert!(!script.contains("darwin-rebuild"));
  }
}
