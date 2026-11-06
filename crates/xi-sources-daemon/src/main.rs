//! `xi-sources-daemon`: the pull-GitOps loop.
//!
//! One scope per process, matching [[xi-sources-model]]'s split: a system
//! daemon watches the system scope's remote(s), a user daemon (run as a
//! `systemd --user` unit under home-manager) watches the user scope's.
//! Launching both, and keeping them alive, is the Nix module's job, not
//! this binary's - this binary only needs to run correctly once started.
//!
//! What one pass through `main` does:
//!
//! 1. Resolve the scope's config through the same precedence ladder the
//!    interactive `xi <scope> switch` uses
//!    ([`xi_core::sources::args::resolve_layers`]), so a daemon started by the
//!    Nix module and a `switch` run by the operator never disagree about what
//!    the remote, the branch, or `require_signed` are.
//! 2. For every configured remote ([`ResolvedSources::effective_remotes`]) that
//!    is due per its own `poll_interval` ([`Scheduler`]), run one
//!    [`xi_sources_daemon::tick::run`], serialised through a [`Singleton`] gate
//!    so a slow fetch cannot stack with the next one.
//! 3. Sleep until the next remote becomes due and repeat, unless `--once` asked
//!    for a single pass over every configured remote (used by operators to
//!    drive the daemon by hand, and by integration tests that do not want to
//!    wait out a real interval).

use std::{
  path::PathBuf,
  thread,
  time::{Duration, Instant},
};

use chrono::Timelike;
use clap::Parser;
use clap_verbosity_flag::{InfoLevel, Verbosity};
use color_eyre::{Result, eyre::eyre};
use tracing::{debug, info, warn};
use xi_core::sources::{
  Scope,
  SourcesId,
  args::resolve_layers,
  config::{ScopePaths, SourcesConfig},
  pause::{DEFAULT_PAUSE_TIMEOUT, TickGate},
  repair,
};
use xi_sources_daemon::{
  activate::{self, ActivateOutcome, SwitchVerb},
  control::{self, ControlServer},
  exec::SystemExec,
  health::TickOutcome,
  machine_id,
  profile,
  schedule::Scheduler,
  singleton::Singleton,
  tick::{self, TickInputs},
};
use xi_sources_observability::{Outcome, TickSpan};

/// Scope spelling accepted on the command line. A thin wrapper around
/// [`Scope`] purely so clap's `ValueEnum` derive has something to attach
/// to - [`Scope`] itself stays free of a CLI dependency.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ScopeArg {
  /// Host-wide configuration.
  System,
  /// Per-user configuration.
  User,
}

impl From<ScopeArg> for Scope {
  fn from(value: ScopeArg) -> Self {
    match value {
      ScopeArg::System => Self::System,
      ScopeArg::User => Self::User,
    }
  }
}

#[derive(Parser)]
#[command(
  name = "xi-sources-daemon",
  about = "Pull-GitOps daemon for xi sources: watches a remote, fast-forwards \
           the sources dir, runs on_advance."
)]
struct Cli {
  /// Which scope's config repo to watch.
  #[arg(long, value_enum)]
  scope: ScopeArg,

  /// Run exactly one tick per configured remote, then exit, instead of
  /// looping forever. For operators driving the daemon by hand and for
  /// tests that should not wait out a real poll interval.
  #[arg(long)]
  once: bool,

  /// Override the sources dir the daemon watches. Mostly for tests; a
  /// real deployment gets this from `ScopePaths::resolve`.
  #[arg(long)]
  sources_dir: Option<PathBuf>,

  /// Skip the `machineId` guard before activating a generation.
  ///
  /// Operator-only, by design ([[xi-sources-gitops]] section 7.2): the
  /// daemon itself never sets this on its own exec of the real switch -
  /// an automated bypass of a safety check is not a safety check. Pass
  /// it when a generation is deliberately meant to run on a different
  /// host than the one it was built against (migrations, cloned VMs).
  #[arg(long)]
  ignore_machine_id: bool,

  #[command(flatten)]
  verbosity: Verbosity<InfoLevel>,
}

fn main() -> Result<()> {
  let cli = Cli::parse();
  setup_logging(cli.verbosity)?;

  let scope: Scope = cli.scope.into();

  // Before anything else, including the first `ScopePaths::resolve`
  // below: a crashed predecessor's leftovers (a dangling shadow fetch,
  // an abandoned transaction sentinel, a stale receipt) have to be
  // cleaned up before this process decides what the pre-lock world
  // looks like. See `xi-sources-crash-recovery`'s "Daemon auto-repair
  // on startup" requirement. `auto_repair` logs its own plan and report
  // (WARN for anything destructive); a repair I/O failure is not fatal
  // to the daemon starting, since the daemon's own first tick will
  // surface the same problem more specifically if repair could not
  // actually fix it.
  if let Err(err) = repair::auto_repair(scope) {
    warn!(%scope, error = %err, "startup repair failed; continuing anyway");
  }

  let paths = ScopePaths::resolve(scope);
  let sources_dir =
    cli.sources_dir.unwrap_or_else(|| paths.sources_dir.clone());

  let resolved = resolve_layers(SourcesConfig::default(), &paths)?;
  let remotes = resolved.effective_remotes();
  if remotes.is_empty() {
    return Err(eyre!(
      "no remote configured for {scope} scope; set `remote` (or \
       `[[sources.remote]]`) before starting the daemon"
    ));
  }

  if !sources_dir.join(".git").is_dir() {
    return Err(eyre!(
      "{} is not a git clone yet; run `xi {scope} switch <remote>` once \
       before starting the daemon",
      sources_dir.display(),
    ));
  }

  // The identity the state dir and the sentinel are keyed on is the
  // *first* configured remote: multi-remote fan-in polls every entry,
  // but they all advance the one sources dir of one logical source.
  let sources_id = SourcesId::new(scope, &remotes[0].url);

  info!(
    scope = %scope,
    remotes = remotes.len(),
    sources_dir = %sources_dir.display(),
    "xi-sources-daemon starting"
  );

  let gate = Singleton::new();
  let exec = SystemExec;
  let control_socket = control::socket_path(&paths.state_dir);
  let control = ControlServer::spawn(control_socket, DEFAULT_PAUSE_TIMEOUT)?;

  // Resolved once: which concrete `xi <verb> switch` this host activates
  // through, the named profile that verb's registration lands on, and
  // this host's `machineId` guard token. All three are host/scope
  // facts, not per-tick ones.
  let switch_verb: SwitchVerb = activate::resolve_switch_verb(scope);
  let daemon_profile = profile::profile_path(scope);
  let host_token = machine_id::host_token(scope);

  // Each remote polls on its own configured `poll_interval`, not on the
  // shortest one across the set: see [`xi_sources_daemon::schedule`].
  let poll_intervals: Vec<Duration> =
    remotes.iter().map(|remote| remote.poll_interval).collect();
  let mut scheduler = Scheduler::new(Instant::now(), &poll_intervals);

  // A sha the daemon advanced the sources dir to but has not yet
  // activated, because the reboot-window gate deferred it. Reloaded on
  // startup ([`xi_core::sources::STATE_FILE_NAMES`]'s `pending-
  // activation`) so a deferral spanning a daemon restart is still
  // honoured on the next due tick rather than forgotten. One per
  // sources dir, not per remote: activation is a property of the
  // sources dir's current head, and a multi-remote config's remotes all
  // advance the same one.
  let mut pending_activation: Option<String> =
    xi_core::sources::read_state_file(&paths.state_dir, "pending-activation")
      .unwrap_or_default();

  loop {
    let due = scheduler.due(Instant::now());

    for index in due {
      let remote = &remotes[index];

      // An inbound `xi push` (or the pause timeout) may have asked the tick
      // loop to stand down. `gate` also polls auto-resume, so a daemon that
      // only ever asks this question before a tick cannot forget to check
      // whether a stale pause has expired - see `xi-sources-race` section 4.
      // A held remote is not marked ticked, so it stays due and is
      // re-checked on the next pass rather than losing its slot.
      if matches!(control.gate(Instant::now()), TickGate::Hold) {
        debug!(remote = %remote.url, "tick held: daemon is paused");
        continue;
      }

      let outcome = {
        let _permit = gate.enter();
        control.begin_tick();

        let tick_span = TickSpan::start(&sources_id.to_string(), &remote.url);
        let inputs = TickInputs {
          sources_id:      &sources_id,
          sources_dir:     &sources_dir,
          state_dir:       &paths.state_dir,
          remote_url:      &remote.url,
          branch:          &remote.branch,
          require_signed:  resolved.require_signed,
          allowed_signers: resolved.allowed_signers.as_deref(),
          gpg_public_keys: resolved.gpg_public_keys.as_deref(),
          on_advance:      resolved.on_advance.as_deref(),
          sync_mode:       resolved.sync_mode,
        };

        let outcome = tick::run(&exec, &inputs);
        let span_outcome = span_outcome_for(&outcome);
        let duration_ms = tick_span.finish(span_outcome);
        tracing::debug!(remote = %remote.url, duration_ms, "tick finished");
        outcome
        // `_permit` drops here, before `control.end_tick()` below -
        // the ordering `xi-sources-race` task 4.2 pins: a `pause` caller
        // must not be told it is safe to acquire until the tick that held
        // the lock has actually released it.
      };
      control.end_tick();

      match &outcome {
        TickOutcome::Degraded(reason) => {
          warn!(remote = %remote.url, reason = %reason, "tick degraded");
        },
        TickOutcome::Advanced { to, hook, .. } => {
          info!(remote = %remote.url, sha = %to, "sources dir advanced");
          if let Some(reason) = hook {
            warn!(remote = %remote.url, reason = %reason, "on_advance hook degraded");
          }
        },
        TickOutcome::NoChange { .. } | TickOutcome::Deferred { .. } => {},
      }

      // Section 6.2: a git-level advance means there is now something to
      // build/activate - record it as pending (persisted, so a reboot-
      // window deferral survives a daemon restart) rather than acting on
      // it inline here. The actual exec happens once below, after every
      // due remote has had its tick, since activation is a property of
      // the sources dir as a whole, not of whichever remote happened to
      // advance it this round.
      if let TickOutcome::Advanced { ref to, .. } = outcome {
        pending_activation = Some(to.clone());
        if let Err(err) = xi_core::sources::write_state_file(
          &paths.state_dir,
          "pending-activation",
          to,
        ) {
          warn!(remote = %remote.url, error = %err, "could not persist pending-activation");
        }
      }

      // Measured from completion, not from when it became due: a tick
      // slower than its own interval must not read as immediately due
      // again. The permit from this iteration's `gate.enter()` is still
      // held here, so this remote's own next-due bookkeeping is itself
      // serialised with every other remote's tick.
      scheduler.mark_ticked(index, Instant::now());
    }

    // A git-level advance is only half the story: exec `xi <verb>
    // switch` to build, activate, and register the generation. Run
    // strictly after every due remote's `control.end_tick()` above and
    // outside `gate`'s permit: the subprocess takes its own fresh
    // `SourcesLock`, and holding anything this crate also holds while it
    // runs would deadlock against it. Runs once per pass regardless of
    // which remote most recently advanced, so a `Deferred` activation
    // from several intervals ago keeps retrying here until the reboot
    // window opens or a newer advance supersedes it.
    if let Some(sha) = pending_activation.clone() {
      let probe_dir = tempfile::Builder::new()
        .prefix("xi-sources-daemon-probe")
        .tempdir();
      match probe_dir {
        Ok(probe_dir) => {
          let probe_out_link = probe_dir.path().join("result");
          let now = chrono::Local::now();
          let minute_of_day = now.hour() * 60 + now.minute();

          let activate_inputs = activate::ActivateInputs {
            host_token: host_token.as_deref(),
            verb: switch_verb,
            state_dir: &paths.state_dir,
            profile_path: &daemon_profile,
            probe_out_link: &probe_out_link,
            ignore_machine_id: cli.ignore_machine_id,
            reboot_window: resolved.reboot_window,
            minute_of_day,
            booted_kernel_path: activate::BOOTED_KERNEL_PATH,
          };

          match activate::run(&exec, &activate_inputs) {
            ActivateOutcome::Activated { hook: None } => {
              info!(sha = %sha, verb = switch_verb.as_str(), "activated and registered generation");
              pending_activation = None;
              clear_pending_activation(&paths.state_dir);
            },
            ActivateOutcome::Activated { hook: Some(reason) } => {
              warn!(sha = %sha, reason = %reason, "activation succeeded but registering the generation degraded");
              pending_activation = None;
              clear_pending_activation(&paths.state_dir);
            },
            ActivateOutcome::Deferred { minutes } => {
              info!(sha = %sha, minutes, "activation deferred: outside the reboot window");
              // `pending_activation` stays set; retried next pass.
            },
            ActivateOutcome::Failed(reason) => {
              warn!(sha = %sha, reason = %reason, verb = switch_verb.as_str(), "activation failed");
              pending_activation = None;
              clear_pending_activation(&paths.state_dir);
            },
          }
        },
        Err(err) => {
          warn!(error = %err, "could not create a probe tempdir; retrying activation next pass");
        },
      }
    }

    if cli.once {
      break;
    }

    let sleep_for = scheduler.sleep_duration(Instant::now());
    thread::sleep(sleep_for.max(Duration::from_secs(1)));
  }

  control.shutdown();

  Ok(())
}

/// Clear the persisted `pending-activation` marker.
///
/// Best-effort: a write failure here just means a resolved activation
/// stays recorded as still-pending, so the next pass attempts it again
/// and finds there is nothing left to do (a fresh `tick::run` on an
/// unchanged sources dir reports `NoChange`) - redundant, not harmful.
fn clear_pending_activation(state_dir: &std::path::Path) {
  if let Err(err) =
    xi_core::sources::write_state_file(state_dir, "pending-activation", "")
  {
    warn!(error = %err, "could not clear pending-activation");
  }
}

/// The observability-layer outcome a [`TickOutcome`] reports as.
const fn span_outcome_for(outcome: &TickOutcome) -> Outcome {
  match outcome {
    TickOutcome::Degraded(_) => Outcome::Failed,
    TickOutcome::Advanced { hook: Some(_), .. } => Outcome::Degraded,
    TickOutcome::Advanced { hook: None, .. }
    | TickOutcome::NoChange { .. }
    | TickOutcome::Deferred { .. } => Outcome::Ok,
  }
}

/// Minimal stderr tracing setup.
///
/// Deliberately not `xi-sources-observability::subscriber` - that module
/// is still unwritten (see the crate's *Implementation status*). This is
/// a placeholder plain `fmt` layer, swapped for the real ndjson stack
/// once it lands.
fn setup_logging(verbosity: Verbosity<InfoLevel>) -> Result<()> {
  use tracing_subscriber::{EnvFilter, fmt, prelude::*};

  color_eyre::install()?;

  let fallback_level = verbosity.tracing_level_filter();

  let env_filter = EnvFilter::try_from_env("XI_LOG")
    .unwrap_or_else(|_| EnvFilter::new(fallback_level.to_string()));

  tracing_subscriber::registry()
    .with(
      fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(env_filter),
    )
    .init();

  Ok(())
}
