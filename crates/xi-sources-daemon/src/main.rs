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
//! 3. Sleep until the next remote becomes due and repeat, unless `--once`
//!    asked for a single pass over every configured remote (used by operators
//!    to drive the daemon by hand, and by integration tests that do not want
//!    to wait out a real interval).

use std::{
  path::PathBuf,
  thread,
  time::{Duration, Instant},
};

use clap::Parser;
use clap_verbosity_flag::{InfoLevel, Verbosity};
use color_eyre::{Result, eyre::eyre};
use tracing::{info, warn};
use xi_core::sources::{
  Scope,
  SourcesId,
  args::resolve_layers,
  config::{ScopePaths, SourcesConfig},
};
use xi_sources_daemon::{
  exec::SystemExec,
  health::TickOutcome,
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

  #[command(flatten)]
  verbosity: Verbosity<InfoLevel>,
}

fn main() -> Result<()> {
  let cli = Cli::parse();
  setup_logging(cli.verbosity)?;

  let scope: Scope = cli.scope.into();
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

  // Each remote polls on its own configured `poll_interval`, not on the
  // shortest one across the set: see [`xi_sources_daemon::schedule`].
  let poll_intervals: Vec<Duration> =
    remotes.iter().map(|remote| remote.poll_interval).collect();
  let mut scheduler = Scheduler::new(Instant::now(), &poll_intervals);

  loop {
    let due = scheduler.due(Instant::now());

    for index in due {
      let remote = &remotes[index];
      let _permit = gate.enter();

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
      };

      let outcome = tick::run(&exec, &inputs);
      let span_outcome = span_outcome_for(&outcome);
      let duration_ms = tick_span.finish(span_outcome);
      tracing::debug!(remote = %remote.url, duration_ms, "tick finished");

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

      // Measured from completion, not from when it became due: a tick
      // slower than its own interval must not read as immediately due
      // again. The permit from this iteration's `gate.enter()` is still
      // held here, so this remote's own next-due bookkeeping is itself
      // serialised with every other remote's tick.
      scheduler.mark_ticked(index, Instant::now());
    }

    if cli.once {
      break;
    }

    let sleep_for = scheduler.sleep_duration(Instant::now());
    thread::sleep(sleep_for.max(Duration::from_secs(1)));
  }

  Ok(())
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
