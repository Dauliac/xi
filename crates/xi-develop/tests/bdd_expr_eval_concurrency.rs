//! BDD: xi-lndy.8.3  -  repl `ExprEval` queries vs. the daemon's own
//! scheduled hook-eval loop, running concurrently against the same flake.
//!
//! Spins up a real daemon (`server::run`, the exact accept loop + eval
//! scheduler production code uses) against a tiny nixpkgs-backed fixture
//! flake. One thread repeatedly flips `current_target` through
//! `DaemonRequest::Prompt`  -  the same `mark_change_pending` →
//! `should_eval`/`spawn_eval` path the real scheduled hook-eval loop runs
//! on  -  while a second thread fires a burst of `Query::ExprEval` calls
//! through the real `client_v3`. A watchdog bounds the whole thing so a
//! genuine deadlock fails this test loudly instead of hanging the suite.
//!
//! Each hook-eval cycle rewrites `flake.nix` with a fresh trailing comment
//! first, which busts `EvalCacheMode::Lock`'s content hash (hash of
//! `flake.lock` + `flake.nix`) so the scheduled loop actually re-runs real
//! `nix print-dev-env` work  -  and therefore real writes to
//! `ServerState::eval_input_hash` / `current_env`  -  on every cycle instead
//! of short-circuiting after the first one. That keeps the exact race the
//! `handle_expr_eval` doc comment calls out (concurrent `RwLock::read`s of
//! those two fields while the scheduled loop writes them) genuinely live
//! for the whole run, rather than only on cycle one.

use std::{
  io::{BufReader, BufWriter},
  os::unix::net::UnixStream,
  path::{Path, PathBuf},
  sync::{Arc, mpsc},
  thread,
  time::{Duration, Instant},
};

use tempfile::TempDir;
use xi_develop::daemon::{
  client_v3::{self, QueryOutcome},
  protocol::{
    DaemonRequest,
    DaemonResponse,
    PromptRequest,
    read_message,
    write_message,
  },
  protocol_v3::{Availability, ExprEvalResult, QueryReply},
  server::{EvalCacheMode, ServerState, run as server_run},
};

/// Overall bound on the concurrent hook-eval / `ExprEval` burst. Generous
/// enough to absorb a cold `nix print-dev-env` substitution on a slow
/// machine; tight enough that a genuine deadlock fails fast instead of
/// hanging the suite.
const WATCHDOG_SECS: u64 = 120;
/// Bound on the one-time `nix flake lock` setup step.
const LOCK_SETUP_SECS: u64 = 90;

const HOOK_EVAL_CYCLES: usize = 8;
const EXPR_EVAL_BURST: usize = 48;

/// `nix eval --impure` is only used here, at test setup, to learn the
/// host's system string  -  the fixture flake itself stays pure-eval
/// compatible (hardcoded system string) so `run_nix_eval`'s real
/// `nix print-dev-env` call (which never passes `--impure`) works
/// unmodified against it, same as production.
fn detect_nix_system() -> String {
  let out = std::process::Command::new("nix")
    .args([
      "eval",
      "--impure",
      "--raw",
      "--expr",
      "builtins.currentSystem",
    ])
    .output()
    .expect("failed to spawn `nix eval` to detect the current system");
  assert!(
    out.status.success(),
    "`nix eval --impure --raw --expr builtins.currentSystem` failed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  String::from_utf8(out.stdout)
    .expect("system string is not valid UTF-8")
    .trim()
    .to_string()
}

/// Fixture flake content: two equivalent `mkShellNoCC` devShells (`default`
/// / `alt`) so toggling `current_target` via `DaemonRequest::Prompt` always
/// counts as a change and retriggers `mark_change_pending`. `marker` is
/// folded into a trailing comment purely to change the file's bytes (and
/// therefore its `EvalCacheMode::Lock` hash) between cycles.
fn fixture_flake_nix(system: &str, marker: usize) -> String {
  format!(
    r#"{{
  description = "xi-repl ExprEval/hook-eval concurrency fixture";
  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";
  outputs = {{ self, nixpkgs }}:
    let
      system = "{system}";
      pkgs = nixpkgs.legacyPackages.${{system}};
    in {{
      devShells.${{system}} = {{
        default = pkgs.mkShellNoCC {{ }};
        alt = pkgs.mkShellNoCC {{ }};
      }};
    }};
}}
# cycle-{marker}
"#
  )
}

/// Write the fixture flake and lock it once. Outside any git repository on
/// purpose: flakes either need git tracking or fall back to a plain
/// `path:` flake, and the latter needs no repo at all  -  simplest way to
/// dodge "file not tracked by Git" errors from `nix`.
fn setup_fixture_flake() -> (TempDir, String) {
  let tmp = tempfile::tempdir().expect("mktemp flake dir");
  let system = detect_nix_system();
  std::fs::write(tmp.path().join("flake.nix"), fixture_flake_nix(&system, 0))
    .expect("write flake.nix");

  let (tx, rx) = mpsc::channel();
  let dir = tmp.path().to_path_buf();
  thread::spawn(move || {
    let out = std::process::Command::new("nix")
      .args(["flake", "lock", "--option", "connect-timeout", "10"])
      .current_dir(&dir)
      .output();
    let _ = tx.send(out);
  });
  let out = rx
    .recv_timeout(Duration::from_secs(LOCK_SETUP_SECS))
    .unwrap_or_else(|_| {
      panic!(
        "`nix flake lock` did not finish within {LOCK_SETUP_SECS}s  -  no \
         network / no nixpkgs substitute available in this environment?"
      )
    })
    .expect("failed to spawn `nix flake lock`");
  assert!(
    out.status.success(),
    "`nix flake lock` failed: {}",
    String::from_utf8_lossy(&out.stderr)
  );

  (tmp, system)
}

/// Poll for the daemon socket to exist (bounded  -  this is setup, not the
/// property under test).
fn wait_for_socket(socket_path: &Path) {
  let deadline = Instant::now() + Duration::from_secs(10);
  while !socket_path.exists() {
    assert!(
      Instant::now() < deadline,
      "daemon socket never appeared at {}",
      socket_path.display()
    );
    thread::sleep(Duration::from_millis(20));
  }
}

/// Send one `DaemonRequest::Prompt` and return the decoded response.
fn send_prompt(socket_path: &Path, req: PromptRequest) -> DaemonResponse {
  let stream = UnixStream::connect(socket_path)
    .unwrap_or_else(|e| panic!("connect failed: {e}"));
  stream
    .set_read_timeout(Some(Duration::from_secs(5)))
    .unwrap();
  stream
    .set_write_timeout(Some(Duration::from_secs(5)))
    .unwrap();
  {
    let mut writer = BufWriter::new(&stream);
    write_message(&mut writer, &DaemonRequest::Prompt(req))
      .unwrap_or_else(|e| panic!("write failed: {e}"));
  }
  let mut reader = BufReader::new(&stream);
  read_message(&mut reader).unwrap_or_else(|e| panic!("read failed: {e}"))
}

/// Drives repeated scheduled hook-eval cycles the same way a real shell
/// prompt hook does: alternate `current_target` so every call is a real
/// change, rewriting `flake.nix` first each time so `EvalCacheMode::Lock`
/// can't skip the real `nix print-dev-env` call.
fn hook_eval_driver(
  socket_path: PathBuf,
  flake_dir: PathBuf,
  system: String,
  cycles: usize,
) {
  for i in 0..cycles {
    std::fs::write(
      flake_dir.join("flake.nix"),
      fixture_flake_nix(&system, i + 1),
    )
    .unwrap_or_else(|e| panic!("hook-eval cycle {i}: rewrite flake.nix: {e}"));

    let target = if i % 2 == 0 { "default" } else { "alt" };
    let resp = send_prompt(&socket_path, PromptRequest {
      consumer_pid: 20_000 + u32::try_from(i).unwrap_or(u32::MAX),
      target:       target.to_string(),
      cwd:          flake_dir.display().to_string(),
      is_subshell:  false,
      parent_pid:   None,
    });
    assert!(
      matches!(resp, DaemonResponse::Prompt(_)),
      "hook-eval cycle {i}: expected a Prompt response, got {resp:?}"
    );

    thread::sleep(Duration::from_millis(150));
  }
}

/// One `(expr, shallow)` probe plus a predicate on the decoded
/// [`ExprEvalResult`] it must produce  -  deterministic regardless of
/// whatever the concurrent hook-eval loop is doing, since none of these
/// expressions touch the flake or the daemon's cached devshell state.
type ExprCase = (&'static str, bool, fn(&ExprEvalResult) -> bool);

const EXPR_CASES: &[ExprCase] = &[
  (
    "1 + 1",
    false,
    |r| matches!(r, ExprEvalResult::Scalar(v) if v.as_i64() == Some(2)),
  ),
  (
    "{ a = 1; b = 2; }",
    true,
    |r| matches!(r, ExprEvalResult::Attrs(names) if names == &["a".to_string(), "b".to_string()]),
  ),
  ("x: x", false, |r| matches!(r, ExprEvalResult::Lambda)),
  ("[ 1 2 3 ]", true, |r| matches!(r, ExprEvalResult::Thunk)),
  ("throw \"xi-lndy.8.3 probe\"", false, |r| {
    matches!(r, ExprEvalResult::Error { .. })
  }),
  // The REPL's "what's in scope" sentinel. Whether this hits
  // `handle_expr_eval`'s fast path (direct `current_env` read) or falls
  // through to a real `nix eval` subprocess depends on whether a
  // scheduled hook-eval has completed yet  -  that race is exactly what
  // this test exercises, so we only assert the response decodes cleanly,
  // not which branch answered it.
  ("", true, |_| true),
];

/// Fires a burst of `Query::ExprEval` calls and returns every decoded
/// result so the caller can assert on well-formedness after the fact.
fn expr_eval_driver(
  socket_path: PathBuf,
  burst: usize,
) -> Vec<(&'static str, bool, ExprEvalResult)> {
  let mut results = Vec::with_capacity(burst);
  for i in 0..burst {
    let (expr, shallow, check) = EXPR_CASES[i % EXPR_CASES.len()];
    let outcome = client_v3::expr_eval(&socket_path, expr, shallow)
      .unwrap_or_else(|e| {
        panic!("expr-eval call {i} ({expr:?}, shallow={shallow}): {e}")
      });
    let QueryOutcome::V3(QueryReply::ExprEval(availability)) = outcome else {
      panic!(
        "expr-eval call {i} ({expr:?}, shallow={shallow}): expected a v3 \
         ExprEval reply, got {outcome:?}"
      );
    };
    let Availability::Ready { value } = availability else {
      panic!(
        "expr-eval call {i} ({expr:?}, shallow={shallow}): handle_expr_eval \
         only ever returns Ready, got {availability:?}"
      );
    };
    assert!(
      check(&value),
      "expr-eval call {i} ({expr:?}, shallow={shallow}): unexpected \
       classification {value:?}"
    );
    results.push((expr, shallow, value));
    thread::sleep(Duration::from_millis(15));
  }
  results
}

/// A scheduled hook-eval loop and a burst of repl `ExprEval` queries,
/// running concurrently against the same flake, neither deadlock nor
/// corrupt either caller's result.
#[test]
fn hook_eval_and_expr_eval_run_concurrently_without_deadlock_or_corruption() {
  let (flake_tmp, system) = setup_fixture_flake();
  let flake_root = flake_tmp.path().to_path_buf();

  let state_tmp = tempfile::tempdir().expect("mktemp state dir");
  let socket_path = state_tmp.path().join("daemon.sock");

  let state = Arc::new(ServerState::new(
    flake_root.clone(),
    state_tmp.path().join("state"),
    0, // eval_interval_secs  -  no rate-limit cooldown between test cycles
    Vec::new(),
    xi_core::cache_queue::QueueConfig::default(),
    EvalCacheMode::Lock,
  ));

  let run_socket = socket_path.clone();
  let run_state = Arc::clone(&state);
  // Detached on purpose: this mirrors the real daemon's lifetime (it runs
  // until idle timeout / shutdown), and the test only cares that the two
  // client-side drivers below complete within the watchdog, not that this
  // loop itself ever returns.
  let _server_thread = thread::spawn(move || {
    let _ = server_run(&run_socket, &run_state);
  });

  wait_for_socket(&socket_path);

  let (done_tx, done_rx) = mpsc::channel();
  let hook_socket = socket_path.clone();
  let hook_flake_dir = flake_root.clone();
  let hook_system = system.clone();
  let expr_socket = socket_path.clone();
  thread::spawn(move || {
    let hook_jh = thread::spawn(move || {
      hook_eval_driver(
        hook_socket,
        hook_flake_dir,
        hook_system,
        HOOK_EVAL_CYCLES,
      );
    });
    let expr_jh =
      thread::spawn(move || expr_eval_driver(expr_socket, EXPR_EVAL_BURST));

    let hook_outcome = hook_jh.join();
    let expr_outcome = expr_jh.join();
    let _ = done_tx.send((hook_outcome, expr_outcome));
  });

  let (hook_outcome, expr_outcome) = done_rx
    .recv_timeout(Duration::from_secs(WATCHDOG_SECS))
    .unwrap_or_else(|_| {
      panic!(
        "hook-eval driver and ExprEval burst did not both finish within \
         {WATCHDOG_SECS}s  -  likely deadlock between the scheduled hook-eval \
         loop and a concurrent ExprEval query"
      )
    });

  hook_outcome.unwrap_or_else(|e| {
    std::panic::resume_unwind(e);
  });
  let results = expr_outcome.unwrap_or_else(|e| {
    std::panic::resume_unwind(e);
  });

  // Every burst call decoded into a well-formed, correctly-classified
  // `ExprEvalResult`  -  already asserted per-call inside the driver, but
  // re-check the count here so a silently-shrunk result vector (e.g. an
  // early return swallowing a panic) would still fail the test.
  assert_eq!(
    results.len(),
    EXPR_EVAL_BURST,
    "expected every ExprEval burst call to produce a decoded result"
  );

  // The daemon survived the storm: one more round-trip proves it is still
  // responsive, not merely that it didn't crash mid-burst.
  assert!(
    client_v3::heartbeat_ok(&socket_path),
    "daemon stopped answering v3 heartbeats after the concurrent burst"
  );
}
