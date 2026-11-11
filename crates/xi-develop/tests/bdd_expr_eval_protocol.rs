//! BDD (xi-repl task 8.2): a shallow `ExprEval` never forces  -  and therefore
//! never builds/realises  -  an unselected candidate's value.
//!
//! Unlike `bdd_client_v3.rs`'s `MiniDaemon` (which answers with a canned
//! reply from a closure), [`RealDaemon`] here routes the request through the
//! *real* [`v3_handlers::handle_envelope`] dispatcher, which shells out to a
//! genuine `nix eval` subprocess (see `v3_handlers::run_expr_eval`). That is
//! the only way to prove something about the daemon's actual nix-eval
//! behaviour rather than about a test double.
//!
//! # The poison technique
//!
//! There is no cheap way to observe "was this nix thunk forced?" from
//! outside the `nix eval` subprocess (its stderr is discarded by the
//! handler on success). Instead the fixture's `expensive` attribute is
//! wired so that forcing it is *guaranteed to fail fast*: its `name` is
//! `import ./<a file that does not exist>`. Nix's `import` is exactly the
//! mechanism that forces an import-from-derivation (IFD) build/realisation
//! in real daemon expressions, so "this import never fires" stands in for
//! "this derivation was never built/realised". Forcing it is cheap to
//! fail (an immediate `ENOENT`-shaped eval error, no sandboxed builder, no
//! network, no `system`/platform assumptions) rather than cheap to succeed,
//! which makes a false negative (the test passing despite a regression)
//! far less likely than a timing-based assertion would allow.
//!
//! `forcing_the_poisoned_candidate_directly_fails` is the control: it proves
//! the fixture is not vacuous by showing that forcing `expensive.name`
//! head-on *does* blow up. Only then does
//! `shallow_expr_eval_never_forces_unselected_candidate` mean anything by
//! showing the shallow query over the same expression does not.

use std::{
  io::{BufReader, BufWriter},
  os::unix::net::UnixListener,
  path::PathBuf,
  sync::{Arc, mpsc::channel},
  thread::{self, JoinHandle},
  time::{Duration, Instant},
};

use tempfile::TempDir;
use xi_develop::daemon::{
  client_v3::{self, QueryOutcome},
  protocol::{read_message, write_message},
  protocol_v3::{Availability, Envelope, ExprEvalResult, QueryReply},
  server::{EvalCacheMode, ServerState},
  v3_handlers,
};

/// A file name that is referenced but deliberately never created in the
/// fixture flake root. Its absence is load-bearing: forcing the attribute
/// that `import`s it must fail.
const POISON_FILE: &str = "xi-repl-8-2-poison-should-never-be-imported.nix";

/// Build the fixture expression: a plain attrset with a cheap `"ok"` string
/// and an `expensive` attribute shaped like a derivation (`type =
/// "derivation"`) whose `name` requires importing [`POISON_FILE`] to
/// resolve. A shallow `ExprEval` must list both attribute names without
/// ever resolving `expensive.name`.
fn fixture_expr() -> String {
  format!(
    r#"let poisoned = {{ type = "derivation"; name = import ./{POISON_FILE}; }}; in {{ cheap = "ok"; expensive = poisoned; }}"#
  )
}

/// A tiny in-process v3 daemon that serves exactly one connection through
/// the real [`v3_handlers::handle_envelope`] dispatcher  -  not a canned
/// reply  -  so the subprocess path (`nix eval`) actually runs.
struct RealDaemon {
  socket:  PathBuf,
  _handle: JoinHandle<()>,
  _tmp:    TempDir,
}

impl RealDaemon {
  /// Spawn a listener backed by a `ServerState` rooted at `flake_root`.
  /// Terminates once a single connection is served.
  fn spawn_one(flake_root: PathBuf) -> Self {
    let tmp = tempfile::tempdir().expect("mktemp");
    let socket = tmp.path().join("v3.sock");
    let listener = UnixListener::bind(&socket).expect("bind");

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state_dir");

    let state = Arc::new(ServerState::new(
      flake_root,
      state_dir,
      5,
      Vec::new(),
      xi_core::cache_queue::QueueConfig::default(),
      EvalCacheMode::Lock,
    ));

    let (ready_tx, ready_rx) = channel::<()>();
    let handle = thread::spawn(move || {
      ready_tx.send(()).ok();
      if let Ok((stream, _)) = listener.accept() {
        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);
        if let Ok(env) = read_message::<_, Envelope>(&mut reader) {
          let reply = v3_handlers::handle_envelope(env, &state);
          let _ = write_message(&mut writer, &reply);
        }
      }
    });
    ready_rx.recv().expect("listener ready");
    Self {
      socket,
      _handle: handle,
      _tmp: tmp,
    }
  }
}

/// A shallow `ExprEval` over an attrset containing an unselected,
/// IFD-poisoned candidate returns `Attrs(["cheap", "expensive"])` instead of
/// forcing (and thereby attempting to build/realise) that candidate.
#[test]
fn shallow_expr_eval_never_forces_unselected_candidate() {
  let flake_root = tempfile::tempdir().expect("mktemp flake root");
  // The poison file must be absent for this test to mean anything: forcing
  // it would otherwise silently succeed instead of failing loudly.
  assert!(
    !flake_root.path().join(POISON_FILE).exists(),
    "poison file must not exist"
  );

  let daemon = RealDaemon::spawn_one(flake_root.path().to_path_buf());

  let start = Instant::now();
  let outcome = client_v3::expr_eval(&daemon.socket, &fixture_expr(), true)
    .expect("expr_eval query should round-trip");
  let elapsed = start.elapsed();

  let QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready { value })) =
    outcome
  else {
    unreachable!("expected ExprEval Ready reply, got {outcome:?}")
  };

  let ExprEvalResult::Attrs(names) = value else {
    unreachable!(
      "shallow query on an attrset must classify as Attrs, got {value:?} \
       instead -- a Scalar/Derivation/Error result here would mean the daemon \
       forced (and attempted to build/realise) the unselected 'expensive' \
       candidate"
    )
  };

  assert_eq!(names, vec!["cheap".to_string(), "expensive".to_string()]);

  // Defense in depth: forcing the poisoned candidate fails fast (see the
  // control test below), so a shallow query that *did* force it would
  // return an Error, not hang. This timing bound is generous on purpose  -
  // it is a secondary signal, not the primary assertion above.
  assert!(
    elapsed < Duration::from_secs(10),
    "shallow eval took {elapsed:?}, suspiciously slow for an attrNames-only \
     classification"
  );
}

/// Control: proves the fixture is not vacuous. Forcing the poisoned
/// candidate's `name` directly (a non-shallow `ExprEval` on
/// `(fixture).expensive.name`) must fail, naming the missing poison file.
/// Without this, the test above could pass for the wrong reason (e.g. a
/// typo that makes `expensive` unreachable rather than merely unforced).
#[test]
fn forcing_the_poisoned_candidate_directly_fails() {
  let flake_root = tempfile::tempdir().expect("mktemp flake root");
  let daemon = RealDaemon::spawn_one(flake_root.path().to_path_buf());

  let expr = format!("({}).expensive.name", fixture_expr());
  let outcome = client_v3::expr_eval(&daemon.socket, &expr, false)
    .expect("expr_eval query should round-trip");

  let QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready { value })) =
    outcome
  else {
    unreachable!("expected ExprEval Ready reply, got {outcome:?}")
  };

  let ExprEvalResult::Error { message } = value else {
    unreachable!(
      "forcing the poisoned candidate directly must error, got {value:?}"
    )
  };
  assert!(
    message.contains(POISON_FILE),
    "expected the import failure to name the missing poison file, got: \
     {message}"
  );
}
