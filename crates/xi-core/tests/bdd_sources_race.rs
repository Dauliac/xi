//! BDD: contract for the `xi-sources-race` capability
//! (openspec/changes/xi-sources-race/specs/xi-sources-race/spec.md).
//!
//! One test per SC-E-* criterion in
//! `openspec/changes/xi-sources-race/proposal.md`, named after it, so a
//! failure here names exactly one broken requirement.
//!
//! Three actors can mutate one sources dir on one host at once: a
//! gitops tick, an interactive `switch`, and an inbound `push`. The
//! claims below are therefore about real `flock(2)` contention and real
//! state dirs on disk, not about a model of them. What *is* injected is
//! the pid-liveness predicate: proving dead-holder recovery by killing a
//! process would make the suite depend on scheduler timing, so the
//! liveness answer is supplied directly and the record is written by
//! hand.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{
  fs,
  path::Path,
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

use tempfile::TempDir;
use xi_core::sources::{
  lock::{
    ALIVE_FILE,
    ALIVE_STALE_AFTER,
    ActorName,
    DEAD_PID_GRACE,
    DEFAULT_LEASE,
    LOCK_FILE,
    LOCK_WAIT_ENV,
    Liveness,
    LockError,
    LockOptions,
    LockRecord,
    SourcesLock,
    WAITERS_DIR,
    holder_liveness,
    next_waiter,
    status,
    wait_override_from,
  },
  pause::{
    AutoResume,
    DEFAULT_PAUSE_TIMEOUT,
    PauseController,
    PauseReply,
    PauseRequest,
    PauseState,
    TickGate,
  },
};

/// A pid no process can hold: above every `pid_max` Linux accepts.
const DEAD_PID: u32 = 0x7FFF_FFFF;

/// One host's state dir, per scope.
struct Host {
  _tmp:   TempDir,
  system: std::path::PathBuf,
  user:   std::path::PathBuf,
}

impl Host {
  fn new() -> Self {
    let tmp = TempDir::new().expect("tempdir");
    let system = tmp.path().join("system-state");
    let user = tmp.path().join("user-state");
    for dir in [&system, &user] {
      fs::create_dir_all(dir).expect("mkdir state");
    }
    Self {
      _tmp: tmp,
      system,
      user,
    }
  }
}

/// `LockOptions` that give up rather than block the test for a lease.
fn impatient(actor: ActorName, intent: &str) -> LockOptions {
  LockOptions::new(actor, intent)
    .wait_for(Duration::from_millis(50))
    // The alive-file refresher is a background thread; nothing here runs
    // long enough to need one, and not spawning it keeps the test's
    // thread count honest.
    .without_refresh()
}

/// Write a holder record by hand, as a crashed actor would have left it.
///
/// The kernel lock is deliberately *not* taken: that is the dead-holder
/// state exactly, a record naming a pid with no `flock` behind it.
fn plant_stale_record(state_dir: &Path, record: &LockRecord) {
  fs::create_dir_all(state_dir.join(WAITERS_DIR)).expect("mkdir waiters");
  fs::write(
    state_dir.join(LOCK_FILE),
    record.to_line().expect("serialise record"),
  )
  .expect("write lock record");
}

// ---------------------------------------------------------------------------
// SC-E-001 / SC-E-007: a held lock blocks, and says who holds it
// ---------------------------------------------------------------------------

/// SC-E-001: a daemon tick holding the lock blocks a parallel switch,
/// and the refusal is machine-readable as `lock-held-by-daemon`.
#[test]
fn sc_e_001_a_daemon_tick_blocks_a_parallel_switch_and_names_it() {
  let host = Host::new();

  let tick = SourcesLock::acquire(
    &host.system,
    &impatient(ActorName::Daemon, "fetch and activate"),
  )
  .expect("the daemon takes a free lock");

  let err = SourcesLock::acquire(
    &host.system,
    &impatient(ActorName::Switch, "xi system switch"),
  )
  .expect_err("a switch must not take a held lock");

  let LockError::Held { holder, .. } = &err else {
    panic!("expected Held, got {err:?}");
  };
  assert_eq!(holder.actor_name, ActorName::Daemon);
  assert_eq!(
    err.reason(),
    "lock-held-by-daemon",
    "SC-E-001 asks for the actor in the reason, not a bare `busy`"
  );

  // SC-E-007: the message carries every field the operator needs.
  let message = err.to_string();
  for needle in [
    "daemon",
    "fetch and activate",
    &holder.pid.to_string(),
    &holder.started_at,
    "estimated completion",
  ] {
    assert!(
      message.contains(needle.as_ref() as &str),
      "the refusal must name `{needle}`: {message}"
    );
  }

  // And once the tick finishes, the switch gets through.
  tick.release().expect("release");
  SourcesLock::acquire(
    &host.system,
    &impatient(ActorName::Switch, "xi system switch"),
  )
  .expect("a released lock is free")
  .release()
  .expect("release");
}

/// Regression for xi-wao6: the lock file's inode must survive a whole
/// acquire-release cycle.
///
/// `flock(2)` locks an inode, not a path, so writing the holder record
/// by renaming a temp file over `state_dir/lock` leaves the holder's
/// lock attached to an unlinked inode and the next acquirer is granted
/// the lock immediately. That is not a subtle degradation: it removes
/// mutual exclusion entirely, which is the one thing this module
/// exists to provide. Pinned on the inode rather than on the symptom
/// so the cause is named if it ever returns.
#[test]
fn writing_the_record_must_not_replace_the_locked_inode() {
  use std::os::unix::fs::MetadataExt as _;

  let host = Host::new();
  let lock_path = host.system.join(LOCK_FILE);

  // Created up front so there is an inode to name *before* acquisition:
  // sampling only afterwards would be measuring whatever the record
  // write left behind, which is the very thing under test. `try_flock`
  // opens with `create(true)` and no truncation, so it reuses this file.
  fs::write(&lock_path, b"").expect("create the lock file");
  let before = fs::metadata(&lock_path).expect("stat before").ino();

  let held =
    SourcesLock::acquire(&host.system, &impatient(ActorName::Daemon, "tick"))
      .expect("acquire");

  assert!(
    !fs::read_to_string(&lock_path)
      .expect("read record")
      .trim()
      .is_empty(),
    "the holder record really was written, so this test is not vacuous"
  );
  assert_eq!(
    fs::metadata(&lock_path).expect("stat while held").ino(),
    before,
    "writing the holder record replaced the lock file, so the holder's flock \
     is attached to an unlinked inode and the lock excludes nobody"
  );

  held.release().expect("release");
  assert_eq!(
    fs::metadata(&lock_path).expect("stat after release").ino(),
    before,
    "releasing the lock replaced the lock file"
  );
}

/// SC-E-001, serialisation half: two threads contending for one lock
/// never hold it at the same time.
#[test]
fn sc_e_001_contending_actors_serialise_rather_than_overlap() {
  let host = Host::new();

  let held =
    SourcesLock::acquire(&host.system, &impatient(ActorName::Daemon, "tick"))
      .expect("first acquisition");

  // A second acquisition from another thread, patient enough to outlast
  // the release below. The flag is the point: without it this test
  // passes even when the lock excludes nobody, because every assertion
  // about the acquired lock holds just as well for one taken too early.
  let released = Arc::new(AtomicBool::new(false));
  let state_dir = host.system.clone();
  let seen = Arc::clone(&released);
  let waiter = thread::spawn(move || {
    let lock = SourcesLock::acquire(
      &state_dir,
      &LockOptions::new(ActorName::Switch, "xi system switch")
        .wait_for(Duration::from_secs(10))
        .without_refresh(),
    );
    (lock, seen.load(Ordering::SeqCst))
  });

  // The holder is still recorded while the waiter spins.
  let state = status(&host.system)
    .expect("read status")
    .expect("the lock is held");
  assert_eq!(state.held_by, ActorName::Daemon);

  // Long enough that a lock providing no exclusion would have been taken
  // by now, so the flag below distinguishes the two outcomes.
  thread::sleep(Duration::from_millis(300));
  released.store(true, Ordering::SeqCst);
  held.release().expect("release");

  let (acquired, after_release) = waiter.join().expect("waiter thread");
  let acquired =
    acquired.expect("the waiter acquires once the holder releases");
  assert!(
    after_release,
    "the waiter took the lock while the daemon still held it, so the lock \
     excludes nobody"
  );
  assert_eq!(acquired.record().actor_name, ActorName::Switch);
  assert!(
    acquired.stolen_from().is_none(),
    "an orderly handover is not a steal"
  );
  acquired.release().expect("release");

  assert!(
    status(&host.system).expect("read status").is_none(),
    "a fully released lock reports free"
  );
}

// ---------------------------------------------------------------------------
// Task 1.2: ActorName::Push and ActorName::Daemon contend for one lock
// ---------------------------------------------------------------------------

/// Task 1.2: a gitops tick holding the lock and an inbound push waiting on
/// it serialise rather than both succeeding, mirroring the module doc's
/// "a push and a daemon tick on the same target serialise" scenario.
#[test]
fn push_and_daemon_contending_for_one_lock_serialise_rather_than_both_succeeding()
 {
  let host = Host::new();

  let tick =
    SourcesLock::acquire(&host.system, &impatient(ActorName::Daemon, "tick"))
      .expect("the daemon takes a free lock");

  // A push arriving mid-tick, patient enough to outlast the release below.
  // The flag is the point: without it this test passes even when the lock
  // excludes nobody, because every assertion about the acquired lock holds
  // just as well for one taken too early.
  let released = Arc::new(AtomicBool::new(false));
  let state_dir = host.system.clone();
  let seen = Arc::clone(&released);
  let waiter = thread::spawn(move || {
    let lock = SourcesLock::acquire(
      &state_dir,
      &LockOptions::new(ActorName::Push, "inbound push")
        .wait_for(Duration::from_secs(10))
        .without_refresh(),
    );
    (lock, seen.load(Ordering::SeqCst))
  });

  // The daemon's tick is still recorded as holder while the push spins.
  let state = status(&host.system)
    .expect("read status")
    .expect("the lock is held");
  assert_eq!(state.held_by, ActorName::Daemon);

  // Long enough that a lock providing no exclusion would have been taken
  // by now, so the flag below distinguishes the two outcomes.
  thread::sleep(Duration::from_millis(300));
  released.store(true, Ordering::SeqCst);
  tick.release().expect("release");

  let (acquired, after_release) = waiter.join().expect("waiter thread");
  let acquired =
    acquired.expect("the push acquires once the daemon's tick releases");
  assert!(
    after_release,
    "the push took the lock while the daemon's tick still held it, so the \
     lock excludes nobody"
  );
  assert_eq!(acquired.record().actor_name, ActorName::Push);
  assert!(
    acquired.stolen_from().is_none(),
    "an orderly handover is not a steal"
  );
  acquired.release().expect("release");

  assert!(
    status(&host.system).expect("read status").is_none(),
    "a fully released lock reports free"
  );
}

// ---------------------------------------------------------------------------
// SC-E-002: pause / resume around an inbound push
// ---------------------------------------------------------------------------

/// SC-E-002: `pause` mid-tick is answered `Pausing`, the real `Paused`
/// is owed until the tick drains, and `resume` restarts the loop.
#[test]
fn sc_e_002_push_pauses_the_daemon_then_resumes_it() {
  let mut daemon = PauseController::new(DEFAULT_PAUSE_TIMEOUT);
  let now = Instant::now();
  // This process stands in for the `xi push` client, because it has to
  // be genuinely alive: `gate` polls auto-resume, and a pause whose
  // requester cannot be found is correctly abandoned (see
  // `sc_e_002_a_pause_outlives_neither_its_requester_nor_its_timeout`).
  let push_pid = std::process::id();

  assert_eq!(daemon.state(), PauseState::Running);
  assert_eq!(daemon.gate(now), TickGate::Run);

  // The push arrives while a tick is in flight, so the honest answer is
  // `Pausing`: the daemon still holds the lock.
  let reply = daemon.on_request(
    &PauseRequest::Pause {
      requester_pid: push_pid,
      timeout_secs:  None,
    },
    true,
    now,
  );
  assert_eq!(reply, PauseReply::Pausing);
  assert_eq!(daemon.state(), PauseState::Pausing {
    requester_pid: push_pid,
  });
  assert_eq!(
    daemon.gate(now),
    TickGate::Hold,
    "no new tick may start once a pause is in flight"
  );

  // The tick finishes and the lock is dropped; now `Paused` is true.
  assert_eq!(daemon.on_tick_complete(), Some(PauseReply::Paused));
  assert_eq!(daemon.state(), PauseState::Paused {
    requester_pid: push_pid,
  });
  assert_eq!(
    daemon.on_tick_complete(),
    None,
    "the deferred reply is owed once, not on every tick boundary"
  );

  // The push is done.
  assert_eq!(
    daemon.on_request(
      &PauseRequest::Resume {
        requester_pid: push_pid,
      },
      false,
      now,
    ),
    PauseReply::Resumed
  );
  assert_eq!(daemon.state(), PauseState::Running);
  assert_eq!(daemon.gate(now), TickGate::Run);
}

/// SC-E-002, no-tick-in-flight half: `pause` on an idle daemon is
/// `Paused` straight away, and a stray `resume` is not an error.
#[test]
fn sc_e_002_pausing_an_idle_daemon_is_immediate_and_resume_is_idempotent() {
  let mut daemon = PauseController::new(DEFAULT_PAUSE_TIMEOUT);
  let now = Instant::now();

  assert_eq!(
    daemon.on_request(
      &PauseRequest::Pause {
        requester_pid: 7,
        timeout_secs:  None,
      },
      false,
      now,
    ),
    PauseReply::Paused
  );

  daemon.on_request(&PauseRequest::Resume { requester_pid: 7 }, false, now);
  assert_eq!(
    daemon.on_request(&PauseRequest::Resume { requester_pid: 7 }, false, now),
    PauseReply::NotPaused,
    "a push that timed out client-side still sends resume; that must not be \
     an error"
  );
}

/// SC-E-002, orphan half: a pause whose requester died ends on its own,
/// and so does one that simply ran out of time.
#[test]
fn sc_e_002_a_pause_outlives_neither_its_requester_nor_its_timeout() {
  let timeout = Duration::from_secs(1800);

  // Requester gone.
  let mut daemon = PauseController::new(timeout);
  let start = Instant::now();
  daemon.on_request(
    &PauseRequest::Pause {
      requester_pid: DEAD_PID,
      timeout_secs:  None,
    },
    false,
    start,
  );
  assert_eq!(
    daemon.poll_auto_resume_with(start, |_| false),
    Some(AutoResume::RequesterGone { pid: DEAD_PID })
  );
  assert_eq!(daemon.state(), PauseState::Running);

  // Timed out, with the requester still alive.
  let mut daemon = PauseController::new(timeout);
  let start = Instant::now();
  daemon.on_request(
    &PauseRequest::Pause {
      requester_pid: 9,
      timeout_secs:  None,
    },
    false,
    start,
  );
  let later = start + timeout + Duration::from_secs(1);
  assert_eq!(
    daemon.poll_auto_resume_with(later, |_| true),
    Some(AutoResume::Timeout {
      after_secs: timeout.as_secs(),
    })
  );
  assert_eq!(daemon.state(), PauseState::Running);
}

// ---------------------------------------------------------------------------
// SC-E-003 / SC-E-006: dead and hung holders
// ---------------------------------------------------------------------------

/// SC-E-003: a record naming a pid that no longer exists is stolen, and
/// the new holder can say whose record it took.
#[test]
fn sc_e_003_a_dead_holders_record_is_stolen_after_the_grace_period() {
  let host = Host::new();
  let ghost = LockRecord {
    pid: DEAD_PID,
    ..LockRecord::now(ActorName::Daemon, "tick that crashed")
  };
  plant_stale_record(&host.system, &ghost);

  assert_eq!(
    holder_liveness(&host.system, &ghost),
    Liveness::DeadPid,
    "a record with no process behind it is a ghost"
  );

  let taken = SourcesLock::acquire(
    &host.system,
    &LockOptions::new(ActorName::Switch, "xi system switch")
      .wait_for(Duration::from_secs(5))
      // The real grace is 10s (SC-E-003); the protocol is what is under
      // test, not the constant, which `DEAD_PID_GRACE` pins separately.
      .dead_pid_grace(Duration::from_millis(50))
      .without_refresh(),
  )
  .expect("a ghost record does not hold the kernel lock");

  let stolen = taken.stolen_from().expect("the steal must be reported");
  assert_eq!(stolen.pid, DEAD_PID);
  assert_eq!(
    stolen.actor_name,
    ActorName::Daemon,
    "the log line names both actors, so both must be available"
  );
  assert_eq!(taken.record().actor_name, ActorName::Switch);
  taken.release().expect("release");
}

/// SC-E-003, constants half: the spec's grace and staleness windows.
#[test]
fn sc_e_003_the_grace_and_staleness_windows_are_the_spec_ones() {
  assert_eq!(DEAD_PID_GRACE, Duration::from_secs(10));
  assert_eq!(ALIVE_STALE_AFTER, Duration::from_secs(90));
}

/// SC-E-006: the lease defaults to 15 minutes, and a holder that stops
/// refreshing its liveness file reads as hung rather than as healthy.
#[test]
fn sc_e_006_a_holder_that_stops_refreshing_is_hung_not_healthy() {
  assert_eq!(DEFAULT_LEASE, Duration::from_mins(15));

  let host = Host::new();
  // This process really is running, so the pid half of liveness passes
  // and the only thing left to judge is the refresh. A record older than
  // the staleness window with no alive file beside it is the holder that
  // wedged before its first ping.
  let wedged = LockRecord {
    started_at: "2020-01-01T00:00:00Z".to_owned(),
    ..LockRecord::now(ActorName::Daemon, "a tick that wedged")
  };
  plant_stale_record(&host.system, &wedged);

  let Liveness::Hung { alive_age } = holder_liveness(&host.system, &wedged)
  else {
    panic!("a holder past the staleness window must not read Alive");
  };
  assert!(
    alive_age > ALIVE_STALE_AFTER,
    "the reported age is what justifies the verdict"
  );

  // A ping makes it credible again, which is exactly what refreshing
  // buys a holder that legitimately needs longer than one lease.
  fs::write(host.system.join(ALIVE_FILE), b"").expect("touch alive");
  assert_eq!(
    holder_liveness(&host.system, &wedged),
    Liveness::Alive,
    "a fresh liveness file outranks an old record, or no holder could ever \
     outlive its lease"
  );

  // A freshly acquired holder has not pinged yet either, and must not be
  // mistaken for a wedged one.
  let just_now = LockRecord::now(ActorName::Daemon, "a tick that just began");
  let fresh = Host::new();
  plant_stale_record(&fresh.system, &just_now);
  assert_eq!(holder_liveness(&fresh.system, &just_now), Liveness::Alive);
}

// ---------------------------------------------------------------------------
// SC-E-004: the priority queue
// ---------------------------------------------------------------------------

/// SC-E-004: with a tick in flight and both a switch and a push queued,
/// the switch is granted first.
#[test]
fn sc_e_004_the_queue_grants_switch_before_push() {
  let queued = [
    LockRecord::now(ActorName::Push, "inbound push"),
    LockRecord::now(ActorName::Switch, "xi system switch"),
    LockRecord::now(ActorName::Daemon, "tick"),
  ];

  let first = next_waiter(&queued).expect("a non-empty queue has a next");
  assert_eq!(first.actor_name, ActorName::Switch);

  // Then the push, once the switch is no longer waiting.
  let rest: Vec<LockRecord> = queued
    .iter()
    .filter(|r| r.actor_name != ActorName::Switch)
    .cloned()
    .collect();
  assert_eq!(
    next_waiter(&rest).expect("still non-empty").actor_name,
    ActorName::Push
  );

  assert!(next_waiter(&[]).is_none(), "an empty queue grants nobody");
}

/// SC-E-004, full order: the spec's priority list, with a total order so
/// two waiters that registered in the same second still queue
/// deterministically.
#[test]
fn sc_e_004_the_priority_order_is_the_spec_one_and_is_total() {
  let spelled = [
    ActorName::Switch,
    ActorName::Sync,
    ActorName::Push,
    ActorName::Bootstrap,
    ActorName::Daemon,
    ActorName::CachePrestage,
  ];
  for pair in spelled.windows(2) {
    assert!(
      pair[0].priority() < pair[1].priority(),
      "{:?} must outrank {:?}",
      pair[0],
      pair[1]
    );
  }

  // Same actor, same timestamp: pid breaks the tie, so the grant does
  // not depend on whichever order `read_dir` happened to yield.
  let base = LockRecord::now(ActorName::Daemon, "tick");
  let low = LockRecord {
    pid: 10,
    ..base.clone()
  };
  let high = LockRecord { pid: 20, ..base };
  assert_eq!(
    next_waiter(&[high, low]).expect("non-empty").pid,
    10,
    "ties resolve by pid, not by directory order"
  );
}

// ---------------------------------------------------------------------------
// SC-E-005: the two scopes do not interact
// ---------------------------------------------------------------------------

/// SC-E-005: system scope and user scope lock independently, because
/// their locks live in different state dirs.
#[test]
fn sc_e_005_system_and_user_scope_locks_do_not_interact() {
  let host = Host::new();

  let system = SourcesLock::acquire(
    &host.system,
    &impatient(ActorName::Switch, "xi system switch"),
  )
  .expect("system scope is free");

  let user = SourcesLock::acquire(
    &host.user,
    &impatient(ActorName::Switch, "xi home switch"),
  )
  .expect("a held system lock must not block the user scope");

  // Each scope reports only its own holder.
  for dir in [&host.system, &host.user] {
    assert!(
      status(dir).expect("read status").is_some(),
      "each scope holds its own lock"
    );
  }

  system.release().expect("release system");
  assert!(
    status(&host.user).expect("read status").is_some(),
    "releasing one scope must not release the other"
  );
  user.release().expect("release user");
}

// ---------------------------------------------------------------------------
// Task 6.1: the status surface
// ---------------------------------------------------------------------------

/// Task 6.1: `status` reports the holder, and reports nothing at all
/// when the lock is free, so the envelope shape `xi-sources-model`
/// pinned is unchanged on the common path.
#[test]
fn the_status_surface_names_the_holder_and_is_absent_when_free() {
  let host = Host::new();
  assert!(
    status(&host.system).expect("read status").is_none(),
    "a free lock contributes nothing to the envelope"
  );

  let held = SourcesLock::acquire(
    &host.system,
    &impatient(ActorName::Push, "inbound push from workstation"),
  )
  .expect("acquire");

  let state = status(&host.system)
    .expect("read status")
    .expect("the lock is held");
  assert_eq!(state.held_by, ActorName::Push);
  assert_eq!(state.intent, "inbound push from workstation");
  assert_eq!(state.pid, std::process::id());
  assert!(
    !state.held_since.is_empty(),
    "held_since is part of the reported record"
  );
  assert!(
    state.estimated_wait_secs.is_some(),
    "a holder that reports a lease gives the waiter an estimate"
  );

  held.release().expect("release");
}

/// A record round-trips through its single JSON line (task 2.2), and
/// carries exactly the four fields the spec names.
#[test]
fn a_lock_record_round_trips_through_one_json_line() {
  let record = LockRecord::now(ActorName::Bootstrap, "first-run adoption");
  let line = record.to_line().expect("serialise");
  assert!(
    !line.trim_end().contains('\n'),
    "the record is one line, so a reader can tail it: {line}"
  );

  let parsed = LockRecord::parse(Path::new("lock"), &line)
    .expect("parse")
    .expect("a non-empty line is a record");
  assert_eq!(parsed, record);
  assert_eq!(parsed.actor_name, ActorName::Bootstrap);

  // An empty file is a free lock, not a malformed one: that is the state
  // between creating the file and writing the record.
  assert!(
    LockRecord::parse(Path::new("lock"), "  \n")
      .expect("blank is not an error")
      .is_none()
  );
}

// ---------------------------------------------------------------------------
// xi-aiw7: a bounded, operator-configurable wait budget
// ---------------------------------------------------------------------------

/// `XI_SOURCES_LOCK_WAIT` is unset: the override is absent, not an error,
/// so [`LockOptions::from_env`] falls back to the spec default untouched.
#[test]
fn an_unset_lock_wait_env_leaves_the_default_untouched() {
  let lookup = |_: &str| None;
  assert_eq!(
    wait_override_from(&lookup).expect("unset is not an error"),
    None
  );
}

/// A valid `XI_SOURCES_LOCK_WAIT` overrides the wait budget.
#[test]
fn a_valid_lock_wait_env_overrides_the_wait_budget() {
  let lookup = |key: &str| (key == LOCK_WAIT_ENV).then(|| "5s".to_owned());
  assert_eq!(
    wait_override_from(&lookup).expect("5s parses"),
    Some(Duration::from_secs(5))
  );
}

/// An empty `XI_SOURCES_LOCK_WAIT` is treated the same as unset, matching
/// every other `XI_SOURCES_*` variable in this capability
/// ([`xi_core::sources::config::SourcesConfig::from_env_with`]).
#[test]
fn an_empty_lock_wait_env_is_treated_as_unset() {
  let lookup = |key: &str| (key == LOCK_WAIT_ENV).then(String::new);
  assert_eq!(
    wait_override_from(&lookup).expect("empty is not an error"),
    None
  );
}

/// A malformed `XI_SOURCES_LOCK_WAIT` is a reported error naming the
/// variable and the bad value, not a silent fallback: a typo should not
/// quietly keep the 15-minute default the operator was trying to shorten.
#[test]
fn a_malformed_lock_wait_env_is_a_named_error() {
  let lookup = |key: &str| (key == LOCK_WAIT_ENV).then(|| "soon".to_owned());
  let err = wait_override_from(&lookup).expect_err("not a duration");
  assert!(err.contains(LOCK_WAIT_ENV), "got: {err}");
  assert!(err.contains("soon"), "got: {err}");
}

/// `LockOptions::new` opts out of progress output by default - a daemon
/// tick or another background actor has no terminal to print status lines
/// to. `with_progress` is how a foreground command like `switch` opts in.
#[test]
fn progress_output_is_off_by_default_and_on_once_requested() {
  let quiet = LockOptions::new(ActorName::Daemon, "tick");
  assert!(!quiet.progress);

  let loud =
    LockOptions::new(ActorName::Switch, "xi home switch").with_progress();
  assert!(loud.progress);
}

/// A progress-enabled acquisition against a lock already held still
/// reports `Held` and still names the holder once it gives up: the status
/// line is a courtesy printed to stderr along the way, not a substitute
/// for the error [`SourcesLock::acquire`] already returns. See
/// `crates/xi/tests/bdd_sources_entrypoint.rs` for the end-to-end proof
/// that the status line is what actually reaches the operator's
/// terminal (xi-fcfy): this is the library-level half, that requesting it
/// does not change the outcome.
#[test]
fn progress_output_does_not_change_the_acquisition_outcome() {
  let host = Host::new();
  let holder =
    SourcesLock::acquire(&host.system, &impatient(ActorName::Daemon, "tick"))
      .expect("the daemon takes a free lock");

  let err = SourcesLock::acquire(
    &host.system,
    &impatient(ActorName::Switch, "xi system switch").with_progress(),
  )
  .expect_err("still held");

  let LockError::Held { holder: named, .. } = &err else {
    panic!("expected Held, got {err:?}");
  };
  assert_eq!(named.actor_name, ActorName::Daemon);

  holder.release().expect("release");
}
