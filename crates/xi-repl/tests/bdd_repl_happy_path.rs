//! BDD: `xi repl`'s end-to-end happy path (`openspec/changes/xi-repl/tasks.md`
//! task 8.5).
//!
//! Opens against a fixture flake, completes a partial attribute path with a
//! preview, then evaluates the completed derivation-valued expression to a
//! structured, colored card.
//!
//! No real `nix eval` runs here. `MiniDaemon` below answers `ExprEval`
//! queries with canned replies over a real Unix socket, the same
//! accept-and-dispatch shape `xi-develop`'s own
//! `crates/xi-develop/tests/bdd_client_v3.rs` fixture uses (that one serves
//! exactly one connection per daemon; this scenario drives three round
//! trips  -  the attribute listing, the completion preview, and the final
//! eval  -  so the loop here keeps accepting instead of exiting after the
//! first). This keeps the test hermetic and fast while still exercising the
//! real wire protocol and the real `xi_repl::session`/`xi_repl::render`
//! code `xi repl`'s own read-eval-print loop calls.
//!
//! `xi_repl::completion::AttrCompleter` (the actual `reedline::Completer`
//! backing Tab/`.`-triggered completion) is a private module, so it is not
//! reachable from this integration test. The completion step below instead
//! drives the exact same public round trip `AttrCompleter` performs
//! internally  -  a shallow `ExprEval` to list sibling attributes, filtered
//! locally by the typed partial, then one more shallow `ExprEval` per
//! surviving candidate rendered through `xi_repl::render::render` for the
//! preview text (see `crates/xi-repl/src/completion.rs`'s module doc
//! comment for the real completer's description of this flow, and
//! `crates/xi-repl/tests/LATENCY_NOTES.md` for this crate's own precedent
//! of calling `xi_develop::daemon::client_v3::expr_eval` directly from a
//! test).

#![expect(clippy::expect_used, clippy::unreachable, reason = "Fine in tests")]

use std::{
  io::{BufReader, BufWriter},
  os::unix::net::UnixListener,
  path::PathBuf,
  sync::mpsc::channel,
  thread::{self, JoinHandle},
};

use tempfile::TempDir;
use xi_develop::daemon::{
  client_v3::{self, QueryOutcome},
  protocol::{read_message, write_message},
  protocol_v3::{
    Availability,
    Envelope,
    ExprEvalResult,
    Payload,
    Query,
    QueryReply,
    Reply,
  },
};

/// A tiny in-process v3 daemon that answers every `ExprEval` query it
/// receives via `responder`, looping over connections for as long as its
/// `TempDir` (and thus its socket) is alive.
///
/// Mirrors `bdd_client_v3.rs`'s `MiniDaemon::spawn_one`, except it keeps
/// accepting rather than exiting after one connection  -  this scenario's
/// three sequential `xi_develop::daemon::client_v3::expr_eval` calls (one
/// per `session::eval`/`client_v3::expr_eval` call site below) each open
/// their own socket, matching `client_v3`'s own "stateless per call" doc
/// comment.
struct MiniDaemon {
  socket:  PathBuf,
  _handle: JoinHandle<()>,
  _tmp:    TempDir,
}

impl MiniDaemon {
  fn spawn<F>(responder: F) -> Self
  where
    F: Fn(&str, bool) -> ExprEvalResult + Send + 'static,
  {
    let tmp = tempfile::tempdir().expect("mktemp");
    let socket = tmp.path().join("v3.sock");
    let listener = UnixListener::bind(&socket).expect("bind");

    let (ready_tx, ready_rx) = channel::<()>();
    let handle = thread::spawn(move || {
      ready_tx.send(()).ok();
      for stream in listener.incoming() {
        let Ok(stream) = stream else { break };
        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);
        let Ok(env) = read_message::<_, Envelope>(&mut reader) else {
          continue;
        };
        let Payload::Query(Query::ExprEval { expr, shallow }) = env.payload
        else {
          continue;
        };
        let value = responder(&expr, shallow);
        let reply = Envelope::new(
          env.id,
          Payload::Reply(Reply::Query(QueryReply::ExprEval(
            Availability::Ready { value },
          ))),
        );
        let _ = write_message(&mut writer, &reply);
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

/// Canned `ExprEval` answers for the fixture flake's `packages.x86_64-linux`
/// attrset: `pkgs` lists two siblings (`hello`, `world`, task 8.5's "at
/// least two sibling attributes so completion has something to list"),
/// `pkgs.hello` is a cheap derivation, everything else is an explicit
/// `Error` rather than a silent default so an unexpected expression fails
/// loudly instead of coincidentally passing.
fn fixture_reply(expr: &str, _shallow: bool) -> ExprEvalResult {
  match expr {
    "pkgs" => {
      ExprEvalResult::Attrs(vec!["hello".to_owned(), "world".to_owned()])
    },
    "pkgs.hello" => {
      ExprEvalResult::Derivation {
        name:        Some("hello-2.12".to_owned()),
        version:     Some("2.12".to_owned()),
        description: Some("A friendly test program".to_owned()),
      }
    },
    other => {
      ExprEvalResult::Error {
        message: format!("fixture daemon has no canned reply for {other:?}"),
      }
    },
  }
}

/// Fixture flake root used for `session::eval`'s `flake_root` argument (it
/// is only used for error messages on the `ReplError::DaemonUnreachable`
/// path, not sent over the wire  -  see `xi_repl::session::eval`), and so
/// the "opening against the fixture flake" step canonicalizes a real flake
/// directory on disk rather than an arbitrary path.
fn fixture_flake_root() -> PathBuf {
  let manifest_dir = env!("CARGO_MANIFEST_DIR");
  std::fs::canonicalize(
    PathBuf::from(manifest_dir).join("tests/fixtures/mini-flake"),
  )
  .expect("fixture flake root exists and canonicalizes")
}

#[test]
fn opens_completes_and_evaluates_a_derivation_end_to_end() {
  // --- 1. Open against the fixture flake. ------------------------------
  //
  // `resolve_socket(.., no_daemon: true)` never spawns  -  it only computes
  // where a daemon for this flake *would* live, exactly the branch
  // `xi_repl::session`'s own doc comment describes for "a daemon happens
  // to already be reachable". This fixture's actual daemon is the
  // `MiniDaemon` below rather than a real `xi-develop` daemon, so the two
  // sockets are deliberately different paths; what matters for "opening"
  // is that resolution against a real, canonicalized flake root succeeds
  // without needing Nix at all.
  let flake_root = fixture_flake_root();
  let computed_socket = xi_repl::session::resolve_socket(&flake_root, true)
    .expect("resolve_socket(no_daemon: true) never spawns, never fails");
  assert_eq!(
    computed_socket.file_name().and_then(|n| n.to_str()),
    Some("daemon.sock"),
    "resolve_socket should compute the same daemon-socket filename \
     xi-develop's own lifecycle derives, got {computed_socket:?}"
  );
  assert_eq!(
    computed_socket,
    xi_repl::session::resolve_socket(&flake_root, true)
      .expect("resolve_socket is pure and deterministic"),
    "the same flake root must resolve to the same socket path every time"
  );

  let daemon = MiniDaemon::spawn(fixture_reply);

  // --- 2. Complete a partial attribute path; a preview appears. --------
  //
  // Typing `pkgs.he<Tab>` splits (on the last `.`, exactly like
  // `completion::attr_prefix`) into base expression `pkgs` and partial
  // `he`.
  let typed = "pkgs.he";
  let dot = typed.rfind('.').expect("typed token contains a dot");
  let base_expr = &typed[..dot];
  let partial = &typed[dot + 1..];
  assert_eq!(base_expr, "pkgs");
  assert_eq!(partial, "he");

  let attrs_outcome = client_v3::expr_eval(&daemon.socket, base_expr, true)
    .expect("shallow attrs query reaches the fixture daemon");
  let names = match attrs_outcome {
    QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready {
      value: ExprEvalResult::Attrs(names),
    })) => names,
    other => unreachable!("expected a shallow Attrs reply, got {other:?}"),
  };

  let mut candidates: Vec<&String> =
    names.iter().filter(|n| n.starts_with(partial)).collect();
  candidates.sort();
  assert_eq!(
    candidates,
    vec!["hello"],
    "completing 'pkgs.he' should offer only the sibling that matches the \
     typed partial, not the unrelated 'world' sibling"
  );

  // Enrich the one surviving candidate with a preview  -  the same shallow
  // round trip plus shared renderer `completion::AttrCompleter`'s
  // background enrichment thread performs per candidate.
  let full_path = format!("{base_expr}.{}", candidates[0]);
  let preview_outcome = client_v3::expr_eval(&daemon.socket, &full_path, true)
    .expect("preview query reaches the fixture daemon");
  let preview_value = match preview_outcome {
    QueryOutcome::V3(QueryReply::ExprEval(Availability::Ready { value })) => {
      value
    },
    other => unreachable!("expected a Ready reply, got {other:?}"),
  };
  let preview = xi_repl::render::render(&preview_value);
  assert!(
    preview.contains("hello") && preview.contains("2.12"),
    "preview should show the completed candidate's name/version, got: \
     {preview:?}"
  );

  // --- 3. Evaluate the completed expression to a structured card. ------
  let result = xi_repl::session::eval(&daemon.socket, &flake_root, &full_path)
    .expect("eval of the completed derivation expression succeeds");
  assert!(
    matches!(result, ExprEvalResult::Derivation { .. }),
    "pkgs.hello should classify as a derivation, got {result:?}"
  );

  let card = xi_repl::render::render(&result);
  assert_eq!(
    card.lines().count(),
    1,
    "a derivation renders as a one-line card, not a multi-line structure: \
     {card:?}"
  );
  assert!(
    card.contains("hello"),
    "card should name the package: {card:?}"
  );
  assert!(
    card.contains("2.12"),
    "card should show the version: {card:?}"
  );
  assert!(
    card.contains("A friendly test program"),
    "card should show the description: {card:?}"
  );
  assert!(
    !card.contains("/nix/store"),
    "the card must be the structured name/version/description shape, not a \
     raw store-path string: {card:?}"
  );
}
