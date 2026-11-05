//! One tick: fetch, verify, resolve.
//!
//! This is where sections 3 (commit selection + conflict enforcement) and
//! 4 (signing) of the `xi-sources-gitops` change meet: [`run`] is the
//! function a scheduler ([`crate::singleton`]) calls once per remote, per
//! interval, and it is the thing [`crate::health::TickOutcome`] describes
//! the result of.
//!
//! The phase order matches
//! [`xi_sources_observability::Phase::FULL_TICK`]'s first three phases -
//! `Fetch`, `Verify`, `Resolve` - exactly, because that enum is the
//! authoritative sequencing contract the observability layer's spans are
//! built from. `Build` and `Activate` are not reached here: realising a
//! generation and activating it are the daemon's build step
//! ([[xi-sources-gitops]] section 6.2), not yet written, so a tick today
//! ends once the sources dir has advanced (or been refused) and
//! `on_advance` has run.

use std::path::Path;

use xi_core::sources::{
  Sha,
  SourcesId,
  config::SyncMode,
  conflict::{PostAdvance, RefRelation, post_advance, relate},
};

use crate::{
  exec::Exec,
  fetch::{checkout, fetch_tip},
  health::{DegradedReason, TickOutcome},
  hooks,
  sentinel::assert_writable,
  signing::verify_tip,
};

/// Everything one tick needs, gathered so [`run`]'s signature stays
/// readable as the pipeline grows.
#[derive(Debug, Clone, Copy)]
pub struct TickInputs<'a> {
  /// Identity of the source this tick is for.
  pub sources_id:      &'a SourcesId,
  /// The managed clone the tick writes to.
  pub sources_dir:     &'a Path,
  /// The flat metadata dir `sources-head` is recorded in.
  pub state_dir:       &'a Path,
  /// Git URL to fetch from.
  pub remote_url:      &'a str,
  /// Branch to track.
  pub branch:          &'a str,
  /// Whether the tip must carry a valid signature to be accepted.
  pub require_signed:  bool,
  /// SSH trust root, when configured.
  pub allowed_signers: Option<&'a Path>,
  /// GPG trust root, when configured.
  pub gpg_public_keys: Option<&'a Path>,
  /// Shell command to run after a successful advance, if any.
  pub on_advance:      Option<&'a str>,
  /// The working-dir reconciliation policy, resolved the same way
  /// `xi <scope> switch` resolves it. Governs what [`TickOutcome::Advanced`]
  /// reports via [`xi_core::sources::conflict::post_advance`]; see that
  /// function for the mapping. Defaults to [`SyncMode::Prompt`] in every
  /// `TickInputs` built here so a caller that does not care still gets the
  /// spec's default behaviour.
  pub sync_mode:       SyncMode,
}

/// Run one tick against `inputs`.
///
/// The pipeline, in order:
///
/// 1. **Sentinel** - refuse to touch `sources_dir` unless it is xi's.
/// 2. **Fetch** - bring the remote's current tip into the object database,
///    without moving anything yet.
/// 3. **Verify** - when `require_signed`, the fetched tip must carry a
///    signature that verifies against a configured trust root.
/// 4. **Resolve** - fast-forward only: the fetched sha must be a descendant of
///    the sources dir's current `HEAD`, or the tick refuses and nothing moves.
/// 5. **Hook** - `on_advance`, if the resolve step actually moved `HEAD`.
/// 6. **Post-advance** - look up `sync_mode`'s
///    [`xi_core::sources::conflict::post_advance`] and report it on the
///    outcome. The tick itself never copies anything into a working dir -
///    `QueueSync` is carried out by the next interactive command via
///    `conflict::apply_auto_sync`, and `Announce` is carried out by `xi sources
///    status` computing `sync_available` live. This step only makes the
///    decision observable (so `never` can be logged) and leaves acting on it to
///    the right owner.
#[must_use]
pub fn run(exec: &dyn Exec, inputs: &TickInputs<'_>) -> TickOutcome {
  if let Err(reason) = assert_writable(inputs.sources_dir, inputs.sources_id) {
    return TickOutcome::Degraded(reason);
  }

  let fetched =
    match fetch_tip(inputs.sources_dir, inputs.remote_url, inputs.branch) {
      Ok(sha) => sha,
      Err(err) => {
        return TickOutcome::Degraded(DegradedReason::FetchFailed {
          remote: inputs.remote_url.to_owned(),
          detail: err.to_string(),
        });
      },
    };

  if inputs.require_signed
    && let Err(reason) = verify_tip(
      inputs.sources_dir,
      &fetched,
      inputs.allowed_signers,
      inputs.gpg_public_keys,
    )
  {
    return TickOutcome::Degraded(reason);
  }

  let current = current_head(inputs.sources_dir);

  let Some(current) = current else {
    // No prior HEAD: the sources dir has just been cloned and never
    // checked out. There is nothing to be a non-descendant *of*, so the
    // first tick always accepts.
    return advance(exec, inputs, None, fetched);
  };

  match relate(inputs.sources_dir, &fetched, &current) {
    RefRelation::Ahead => advance(exec, inputs, Some(current), fetched),
    // `Identical`: nothing moved. `Behind`: the fetched tip is behind the
    // sources dir - another remote (in a multi-remote config) already
    // advanced past it this window. Neither is a refusal, just nothing
    // for this remote to do.
    RefRelation::Identical | RefRelation::Behind => {
      TickOutcome::NoChange { sha: current }
    },
    // `Diverged`: a genuine force push. `Unknown`: ancestry could not be
    // established at all, and refusing is the safe default - advancing
    // on an undecidable relationship is exactly the force-push case this
    // tick exists to catch, with extra steps.
    RefRelation::Diverged | RefRelation::Unknown => {
      TickOutcome::Degraded(DegradedReason::RemoteForcePush {
        rejected: fetched,
        held:     current,
      })
    },
  }
}

/// Checkout the fetched sha, record it, and run `on_advance`.
fn advance(
  exec: &dyn Exec,
  inputs: &TickInputs<'_>,
  from: Option<Sha>,
  to: Sha,
) -> TickOutcome {
  if let Err(err) = checkout(inputs.sources_dir, &to) {
    return TickOutcome::Degraded(DegradedReason::FetchFailed {
      remote: inputs.remote_url.to_owned(),
      detail: err.to_string(),
    });
  }

  if let Err(err) =
    xi_core::sources::write_state_file(inputs.state_dir, "sources-head", &to)
  {
    tracing::warn!(error = %err, "could not record sources-head after advance");
  }

  let hook_reason = inputs.on_advance.and_then(|command| {
    hooks::run(exec, inputs.state_dir, "on_advance", command).degraded_reason()
  });

  let decision = post_advance(inputs.sync_mode);
  if matches!(decision, PostAdvance::LogOnly) {
    // `never`: nothing else in this binary will ever mention this sync.
    // `prompt` is picked up by `xi sources status` computing
    // `sync_available` live, and `auto-copy` is picked up by the next
    // interactive command, so this is the only place `never`'s advance is
    // ever recorded.
    tracing::info!(
      event = "sync-available-not-applied",
      sources_id = %inputs.sources_id,
      from = from.as_deref().unwrap_or("<none>"),
      to = %to,
      "sync_mode = never; a sync is available but the daemon will not apply \
       it"
    );
  }

  TickOutcome::Advanced {
    from,
    to,
    hook: hook_reason,
    post_advance: decision,
  }
}

/// `HEAD` of `sources_dir`, or `None` for an unborn/empty repo.
fn current_head(sources_dir: &Path) -> Option<Sha> {
  xi_core::sources::conflict::head_of(sources_dir)
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]
mod tests {
  use std::{fs, process::Command};

  use tempfile::TempDir;
  use xi_core::sources::Scope;

  use super::*;
  use crate::exec::recorder::RecordingExec;

  fn git(at: &Path, args: &[&str]) {
    let status = Command::new("git")
      .env("GIT_TERMINAL_PROMPT", "0")
      .arg("-C")
      .arg(at)
      .args(args)
      .status()
      .expect("git should run");
    assert!(status.success(), "git {args:?} failed");
  }

  fn commit(repo: &Path, name: &str, body: &str) {
    fs::write(repo.join(name), body).expect("write");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", body]);
  }

  struct World {
    _tmp:        TempDir,
    remote:      std::path::PathBuf,
    sources_dir: std::path::PathBuf,
    state_dir:   std::path::PathBuf,
  }

  impl World {
    fn new() -> Self {
      let tmp = TempDir::new().expect("tempdir");
      let remote = tmp.path().join("remote");
      let sources_dir = tmp.path().join("sources");
      let state_dir = tmp.path().join("state");
      fs::create_dir_all(&remote).expect("mkdir");

      git(&remote, &["init", "-q", "-b", "main"]);
      git(&remote, &["config", "user.email", "test@example.com"]);
      git(&remote, &["config", "user.name", "Test"]);
      commit(&remote, "flake.nix", "base");

      fs::create_dir_all(&sources_dir).expect("mkdir");
      git(&sources_dir, &["init", "-q", "-b", "main"]);

      Self {
        _tmp: tmp,
        remote,
        sources_dir,
        state_dir,
      }
    }

    fn sources_id(&self) -> SourcesId {
      SourcesId::new(Scope::System, self.remote.to_str().expect("utf8"))
    }

    fn inputs<'a>(&'a self, id: &'a SourcesId) -> TickInputs<'a> {
      TickInputs {
        sources_id:      id,
        sources_dir:     &self.sources_dir,
        state_dir:       &self.state_dir,
        remote_url:      self.remote.to_str().expect("utf8"),
        branch:          "main",
        require_signed:  false,
        allowed_signers: None,
        gpg_public_keys: None,
        on_advance:      None,
        sync_mode:       SyncMode::Prompt,
      }
    }
  }

  fn write_sentinel(world: &World, id: &SourcesId) {
    crate::sentinel::Sentinel::new(id.clone())
      .write(&world.sources_dir)
      .expect("write sentinel");
  }

  #[test]
  fn first_tick_on_an_empty_clone_adopts_the_remote_tip() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &world.inputs(&id));
    assert!(outcome.advanced(), "got {outcome:?}");
    if let TickOutcome::Advanced { from, .. } = outcome {
      assert_eq!(from, None);
    }
  }

  /// SC-G-002 / "Linear advance accepted".
  #[test]
  fn a_linear_remote_advance_fast_forwards_the_sources_dir() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    // Adopt the base commit first, as a real daemon would on its first
    // tick.
    assert!(run(&exec, &world.inputs(&id)).advanced());

    commit(&world.remote, "flake.nix", "advanced");
    let outcome = run(&exec, &world.inputs(&id));

    let expected = {
      let output = Command::new("git")
        .arg("-C")
        .arg(&world.remote)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse");
      String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    match outcome {
      TickOutcome::Advanced { to, .. } => assert_eq!(to, expected),
      other => panic!("expected an advance, got {other:?}"),
    }
    assert_eq!(
      fs::read_to_string(world.sources_dir.join("flake.nix")).expect("read"),
      "advanced"
    );
  }

  #[test]
  fn a_second_tick_with_nothing_new_reports_no_change() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    assert!(run(&exec, &world.inputs(&id)).advanced());
    let outcome = run(&exec, &world.inputs(&id));
    assert!(
      matches!(outcome, TickOutcome::NoChange { .. }),
      "got {outcome:?}"
    );
  }

  /// SC-G-003 / "Non-descendant rejected" (force push).
  #[test]
  fn a_force_pushed_remote_is_rejected_and_sources_head_stays_put() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    assert!(run(&exec, &world.inputs(&id)).advanced());
    let held =
      fs::read_to_string(world.sources_dir.join(".git/refs/heads/main")).ok();

    // Force-push: rewrite history instead of extending it.
    git(&world.remote, &["checkout", "-q", "--orphan", "rewritten"]);
    commit(&world.remote, "flake.nix", "rewritten-history");
    git(&world.remote, &["branch", "-M", "rewritten", "main"]);

    let outcome = run(&exec, &world.inputs(&id));
    match &outcome {
      TickOutcome::Degraded(reason) => {
        assert_eq!(reason.token(), "remote-force-push");
      },
      other => panic!("expected Degraded(remote-force-push), got {other:?}"),
    }

    let now =
      fs::read_to_string(world.sources_dir.join(".git/refs/heads/main")).ok();
    assert_eq!(
      held, now,
      "sources_head must not move on a rejected force push"
    );
  }

  #[test]
  fn a_missing_sentinel_aborts_the_tick_before_any_write() {
    let world = World::new();
    let id = world.sources_id();
    // No sentinel written.
    let exec = RecordingExec::always_ok();

    let outcome = run(&exec, &world.inputs(&id));
    match outcome {
      TickOutcome::Degraded(reason) => {
        assert_eq!(reason.token(), "missing-sentinel");
      },
      other => panic!("expected Degraded(missing-sentinel), got {other:?}"),
    }
    assert!(
      !world.sources_dir.join("flake.nix").exists(),
      "a tick with no sentinel must not write into the sources dir"
    );
  }

  #[test]
  fn a_successful_advance_runs_the_on_advance_hook() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    let mut inputs = world.inputs(&id);
    inputs.on_advance = Some("true");
    let outcome = run(&exec, &inputs);
    assert!(outcome.advanced());
    assert_eq!(
      exec.argv_log(),
      vec!["sh -c true".to_owned()],
      "on_advance should run exactly once, via a shell"
    );
  }

  #[test]
  fn a_failing_on_advance_hook_does_not_undo_the_advance() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok().with_reply(1, "", "boom");

    let mut inputs = world.inputs(&id);
    inputs.on_advance = Some("exit 1");
    let outcome = run(&exec, &inputs);

    match outcome {
      TickOutcome::Advanced {
        hook: Some(reason), ..
      } => {
        assert_eq!(reason.token(), "hook-on_advance-failed");
      },
      other => {
        panic!("expected an advance with a degraded hook, got {other:?}")
      },
    }
    assert!(
      world.sources_dir.join("flake.nix").exists(),
      "the advance itself must stand even though the hook failed"
    );
  }

  /// 5b.2 / default config: `sync_mode = prompt` reports `Announce` so
  /// `xi sources status` is the thing that surfaces the sync, not the
  /// daemon writing anything.
  #[test]
  fn sync_mode_prompt_reports_announce_on_advance() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    let mut inputs = world.inputs(&id);
    inputs.sync_mode = SyncMode::Prompt;
    let outcome = run(&exec, &inputs);

    match outcome {
      TickOutcome::Advanced { post_advance, .. } => {
        assert_eq!(post_advance, PostAdvance::Announce);
      },
      other => panic!("expected an advance, got {other:?}"),
    }
  }

  /// 5b.2: `sync_mode = auto-copy` reports `QueueSync`. The daemon itself
  /// does not touch the working dir - running the sync is the next
  /// interactive command's job via `conflict::apply_auto_sync`.
  #[test]
  fn sync_mode_auto_copy_reports_queue_sync_on_advance() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    let mut inputs = world.inputs(&id);
    inputs.sync_mode = SyncMode::AutoCopy;
    let outcome = run(&exec, &inputs);

    match outcome {
      TickOutcome::Advanced { post_advance, .. } => {
        assert_eq!(post_advance, PostAdvance::QueueSync);
      },
      other => panic!("expected an advance, got {other:?}"),
    }
  }

  /// 5b.2: `sync_mode = never` reports `LogOnly` and the tick logs a
  /// `sync-available-not-applied` event - this is the only place `never`'s
  /// advance is ever recorded, since neither `xi sources status`'s
  /// `sync_available` computation nor any interactive command is told
  /// about it otherwise.
  #[test]
  fn sync_mode_never_reports_log_only_on_advance() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    let mut inputs = world.inputs(&id);
    inputs.sync_mode = SyncMode::Never;
    let outcome = run(&exec, &inputs);

    match outcome {
      TickOutcome::Advanced { post_advance, .. } => {
        assert_eq!(post_advance, PostAdvance::LogOnly);
      },
      other => panic!("expected an advance, got {other:?}"),
    }
  }

  /// A `NoChange` tick (nothing to advance to) must not report a
  /// post-advance decision for a mode that never fired - there is no
  /// `TickOutcome::Advanced` to carry one.
  #[test]
  fn a_no_change_tick_carries_no_post_advance_decision() {
    let world = World::new();
    let id = world.sources_id();
    write_sentinel(&world, &id);
    let exec = RecordingExec::always_ok();

    let mut inputs = world.inputs(&id);
    inputs.sync_mode = SyncMode::Never;
    assert!(run(&exec, &inputs).advanced());

    let outcome = run(&exec, &inputs);
    assert!(
      matches!(outcome, TickOutcome::NoChange { .. }),
      "got {outcome:?}"
    );
  }
}
