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
  SourcesContext,
  conflict::inspect_working_tree,
  secrets::{SecretSpec, UploadAt},
};

use super::{
  args::{BuildStrategy, PushArgs, PushMode},
  config::{self, PushConfig},
  context,
  discover::{self, PushTarget, ResolvedBackend},
  engine::{CliNix, Nix, RemoteCommand, SshTransport, Transport},
  limits::PhaseLimits,
  mode_sources, preflight, rollback,
};
use crate::target::ProfileKind;

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
      args.effective_mode(remote_build) == PushMode::Closure
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
  pub name: String,
  /// `Ok(())` once the target is fully pushed (and, under
  /// `--magic-rollback`, confirmed); `Err` for any failure, including a
  /// magic-rollback that reverted the activation.
  pub outcome: Result<()>,
}

/// The whole run's report: one [`TargetResult`] per selected target.
#[derive(Debug, Default)]
pub struct PushReport {
  /// Per-target results, in selection order.
  pub results: Vec<TargetResult>,
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
/// Refuses before any discovery if the sources context is `Missing` or
/// the working dir is dirty without `--force-dirty`. Otherwise returns an
/// error naming how many targets failed once every target has been
/// attempted (preflight and `--fail-fast` both run to completion first).
pub fn run(args: &PushArgs) -> Result<()> {
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
  // The build-location ladder's third rung: a `[push] remote-build`
  // table the Nix module wrote into the same per-scope file
  // `[sources]`'s own Nix-module rung reads
  // (`xi_core::sources::config::ScopePaths::resolve(scope).config_file`).
  let nix_module_remote_build = PushConfig::from_toml_file(
    &xi_core::sources::config::ScopePaths::resolve(scope).config_file,
  )?
  .remote_build;
  let nix = CliNix;
  let transport = SshTransport;

  let report = execute(
    args,
    &context,
    &flake_ref,
    &secrets,
    &inventory,
    nix_module_remote_build,
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
#[expect(clippy::too_many_arguments, reason = "every argument is load-bearing")]
pub fn execute(
  args: &PushArgs,
  context: &SourcesContext,
  flake_ref: &str,
  secrets: &BTreeMap<String, SecretSpec>,
  inventory: &PushConfig,
  nix_module_remote_build: Option<bool>,
  nix: &dyn Nix,
  transport: &dyn Transport,
) -> Result<PushReport> {
  let all_targets = discover::discover(nix, flake_ref)?;
  let selected =
    discover::select(&all_targets, &args.targets, args.on.as_deref())?;
  let targets: Vec<PushTarget> =
    config::apply_inventory(selected.into_iter().cloned().collect(), inventory);

  let colmena_present = discover::has_colmena_hive(nix, flake_ref)?;
  let backend =
    discover::detect_backend(args.backend, &targets, colmena_present);

  if backend != ResolvedBackend::Builtin {
    delegate(backend, flake_ref, args, &targets)?;
    return Ok(PushReport {
      results: targets
        .iter()
        .map(|target| TargetResult {
          name: target.addr.name.clone(),
          outcome: Ok(()),
        })
        .collect(),
    });
  }

  let limits = PhaseLimits::new(args.parallel);
  let aborted = AtomicBool::new(false);

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
        return Ok(PushReport {
          results: built
            .keys()
            .map(|name| TargetResult {
              name: name.clone(),
              outcome: Ok(()),
            })
            .collect(),
        });
      }
      Some(built)
    } else {
      None
    };

  let results: Vec<TargetResult> = std::thread::scope(|scope| {
    let handles: Vec<_> = targets
      .iter()
      .map(|target| {
        let limits = &limits;
        let aborted = &aborted;
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

  Ok(PushReport { results })
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
    flake_ref: Some(flake_ref.to_owned()),
    targets: targets.iter().map(|t| t.addr.name.clone()).collect(),
    on: None,
    backend: Some(backend.as_str().to_owned()),
    dry: false,
    no_nom: false,
    skip_checks: false,
    no_magic_rollback: args.magic_rollback == 0,
    confirm_timeout: if args.magic_rollback > 0 {
      args.magic_rollback
    } else {
      30
    },
    show_trace: false,
    extra_args: Vec::new(),
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
  let mode = args.effective_mode(remote_build);
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
      )
      .and_then(|()| {
        mode_sources::switch(
          transport,
          target,
          args.sources_scope(),
          args.interactive_sudo,
        )
      }) {
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
      if let Err(e) =
        transport.copy_closure(&target.addr, &store_path, args.copy_via)
      {
        if args.fail_fast {
          aborted.store(true, Ordering::SeqCst);
        }
        return Err(e);
      }

      let script = activation_script(target, &store_path);
      let outcome = transport.run(
        &target.addr,
        &RemoteCommand::elevated(script, args.interactive_sudo),
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
) -> Result<()> {
  for secret in secrets.values().filter(|secret| secret.upload_at == when) {
    let local = secret.source_from(&context.sources_dir);
    transport
      .upload_secret(&target.addr, &local, secret, interactive_sudo)
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

/// The activation command for `--mode closure`: register `store_path` in
/// the target's profile, then run the activation the target's kind
/// needs.
///
/// There is no flake on the target under this mode, so this cannot shell
/// out to `nixos-rebuild`/`darwin-rebuild switch` the way
/// [`mode_sources::switch_script`] does - it runs the lower-level
/// commands those tools themselves run, which is also exactly what
/// deploy-rs does for the same reason.
#[must_use]
fn activation_script(target: &PushTarget, store_path: &str) -> String {
  let register = format!(
    "nix-env --profile {} --set {store_path}",
    target.profile_path
  );
  match &target.kind {
    ProfileKind::NixOS => {
      format!("{register} && {store_path}/bin/switch-to-configuration switch")
    },
    ProfileKind::Darwin | ProfileKind::HomeManager => {
      format!("{register} && {store_path}/activate")
    },
    ProfileKind::Custom { activate_command } => activate_command.clone(),
    ProfileKind::Noop => register,
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::sync::Mutex;

  use tempfile::TempDir;
  use xi_core::sources::{Scope, SourcesId};

  use super::*;
  use crate::push::{
    args::{CopyVia as PushCopyVia, PushBackend},
    engine::{Outcome, TargetAddr},
  };

  /// Answers flake evaluation from a fixture table keyed by attr, the
  /// same shape `bdd_push_builtin.rs` uses.
  struct FixtureNix {
    outputs: Vec<(&'static str, serde_json::Value)>,
    built: Mutex<Vec<String>>,
    build_fails: bool,
  }

  impl FixtureNix {
    fn single_nixos_host(name: &'static str) -> Self {
      Self {
        outputs: vec![(".#nixosConfigurations", serde_json::json!([name]))],
        built: Mutex::new(Vec::new()),
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
    run_calls: Mutex<Vec<RemoteCommand>>,
    copy_calls: Mutex<Vec<(String, String)>>,
    secret_calls: Mutex<Vec<String>>,
    sync_calls: Mutex<Vec<String>>,
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
      if cmd.script.contains("switch-to-configuration switch")
        && self.activation_fails
      {
        return Ok(Outcome::failed("switch-to-configuration failed"));
      }
      Ok(Outcome::ok(""))
    }

    fn copy_closure(
      &self,
      target: &TargetAddr,
      store_path: &str,
      _via: PushCopyVia,
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
    ) -> Result<()> {
      self
        .sync_calls
        .lock()
        .expect("sync_calls lock")
        .push(format!("{}->{remote}", local.display()));
      Ok(())
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
    ) -> Result<()> {
      self.0.copy_closure(target, store_path, via)
    }

    fn upload_secret(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      spec: &SecretSpec,
      interactive_sudo: bool,
    ) -> Result<()> {
      self.0.upload_secret(target, local, spec, interactive_sudo)
    }

    fn sync_dir(
      &self,
      target: &TargetAddr,
      local: &std::path::Path,
      remote: &str,
      excludes: &[&str],
      interactive_sudo: bool,
    ) -> Result<()> {
      self
        .0
        .sync_dir(target, local, remote, excludes, interactive_sudo)
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
    }
  }

  #[test]
  fn a_single_host_closure_push_builds_copies_and_activates() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
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
    assert!(
      last.contains("switch-to-configuration switch"),
      "got: {last}"
    );
  }

  #[test]
  fn an_unreachable_target_never_mutates() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = NoRebuildTool(FixtureTransport::default());
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
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
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
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
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args(PushMode::Sources),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
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
      switch_call.starts_with("systemd-run --wait --pipe --collect -- "),
      "got: {switch_call}"
    );
    assert!(
      switch_call
        .ends_with("nixos-rebuild switch --flake /etc/xi/sources#web1"),
      "got: {switch_call}"
    );
  }

  #[test]
  fn with_no_mode_flag_a_toml_remote_build_default_picks_sources_mode() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      remote_build: Some(true),
      backend: None,
      hosts: Vec::new(),
    };

    let report = execute(
      &args_with_mode(None),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
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
  fn an_explicit_mode_flag_overrides_the_toml_remote_build_default() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();
    let inventory = PushConfig {
      remote_build: Some(true),
      backend: None,
      hosts: Vec::new(),
    };

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &inventory,
      None,
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
    let (_root, context) = fixture_context();
    let secrets = BTreeMap::new();

    let report = execute(
      &args_with_mode(None),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      Some(true),
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
    let (_root, context) = fixture_context();
    let mut secrets = BTreeMap::new();
    secrets.insert(
      "ssh-key".to_owned(),
      SecretSpec {
        name: "ssh-key".to_owned(),
        source: std::path::PathBuf::from("./ssh-key"),
        destination: std::path::PathBuf::from("/run/keys/ssh"),
        owner: "root".to_owned(),
        group: None,
        mode: "0600".to_owned(),
        upload_at: UploadAt::PreActivate,
      },
    );

    let report = execute(
      &args(PushMode::Closure),
      &context,
      ".",
      &secrets,
      &PushConfig::default(),
      None,
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
      outputs: vec![(
        ".#nixosConfigurations",
        serde_json::json!(["web1", "web2"]),
      )],
      built: Mutex::new(Vec::new()),
      build_fails: false,
    };
    let transport = FixtureTransport {
      activation_fails: true,
      ..FixtureTransport::default()
    };
    let (_root, context) = fixture_context();
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
      outputs: vec![(
        ".#nixosConfigurations",
        serde_json::json!(["web1", "web2"]),
      )],
      built: Mutex::new(Vec::new()),
      build_fails: false,
    };
    let transport = FixtureTransport {
      activation_fails: true,
      ..FixtureTransport::default()
    };
    let (_root, context) = fixture_context();
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
      sources_id: SourcesId::new(Scope::System, "https://example/repo"),
      sources_dir: working_dir.parent().expect("parent").join("sources"),
      state_dir: working_dir.parent().expect("parent").join("state"),
      working_dir: Some(working_dir),
      refs: xi_core::sources::RefLineage::default(),
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

  #[test]
  fn no_recorded_working_dir_never_refuses() {
    let root = TempDir::new().expect("tempdir");
    let context = SourcesContext {
      sources_id: SourcesId::new(Scope::System, "https://example/repo"),
      sources_dir: root.path().join("sources"),
      state_dir: root.path().join("state"),
      working_dir: None,
      refs: xi_core::sources::RefLineage::default(),
      unknown_state_files: Vec::new(),
    };
    assert!(refuse_if_dirty(&context, false).is_ok());
  }

  // -- build_batch: the upfront build phase a batched strategy runs ----

  /// A `PushMode::Closure`-shaped target, built directly rather than
  /// through `discover::discover`, since `build_batch` takes a
  /// `&[PushTarget]` slice rather than a flake to evaluate.
  fn closure_target(name: &str) -> PushTarget {
    PushTarget {
      addr: TargetAddr {
        name: name.to_owned(),
        ssh_host: name.to_owned(),
        ssh_opts: Vec::new(),
      },
      source: discover::TargetSource::NixosConfigurations,
      tags: Vec::new(),
      installable: format!(".#nixosConfigurations.{name}"),
      kind: ProfileKind::NixOS,
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
      remote_build: None,
      backend: None,
      hosts: vec![config::PushHost {
        name: "web2".to_owned(),
        ssh: "web2".to_owned(),
        remote_build: Some(true),
        backend: None,
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
      outputs: Vec::new(),
      built: Mutex::new(Vec::new()),
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
    let (_root, context) = fixture_context();
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
    let (_root, context) = fixture_context();
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
    assert!(
      last.contains("switch-to-configuration switch"),
      "got: {last}"
    );
  }

  #[test]
  fn from_manifest_missing_a_target_refuses_before_shipping_anything() {
    let nix = FixtureNix::single_nixos_host("web1");
    let transport = FixtureTransport::default();
    let (_root, context) = fixture_context();
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
      &nix,
      &transport,
    )
    .expect_err("manifest missing web1");
    assert!(format!("{err}").contains("web1"), "got: {err}");
    assert!(transport.copy_calls.lock().expect("lock").is_empty());
    assert!(nix.built.lock().expect("built lock").is_empty());
  }
}
