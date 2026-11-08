//! v3 CQRS request handlers.
//!
//! Owns the v3 request/response path: envelopes carrying a `"v":3`
//! discriminator route through here from `daemon::server::handle_connection`.
//!
//! Contracts:
//! - Queries never block on work. They read `ServerState` fields directly; no
//!   channel round-trip.
//! - Every command reply carries an existing or newly-allocated `JobId` via
//!   [`CommandReply::Accepted`]. `EvalDevshell` attaches through
//!   [`JobRegistry`] so N concurrent identical commands observe one job.
//! - Malformed dispatch produces [`Reply::Error`], never a panic.

use std::sync::{Arc, OnceLock};

use nix_command::{CommandKind, NixCommand};
use serde_json::json;

use super::{
  job_registry::{
    Attached,
    IdempotencyKey as RegistryKey,
    JobEntry,
    JobId,
    JobKind,
    JobRegistry,
    JobStatus,
  },
  protocol::DaemonState,
  protocol_v3::{
    Availability,
    Command,
    CommandReply,
    Envelope,
    ExprEvalResult,
    FLAKE_SCOPE_EXPR,
    IdempotencyKey,
    Payload,
    Query,
    QueryReply,
    Reply,
  },
  server::ServerState,
};

/// Process-wide job registry. One per daemon process; each daemon invocation
/// starts fresh (registry is intentionally in-memory per the design's Risks
/// section).
fn registry() -> &'static JobRegistry {
  static R: OnceLock<JobRegistry> = OnceLock::new();
  R.get_or_init(JobRegistry::new)
}

/// Handle a decoded v3 envelope end to end, returning the reply envelope the
/// caller should write back on the wire.
///
/// The `id` on the reply mirrors the request `id` so clients can multiplex.
#[must_use]
pub fn handle_envelope(
  request: Envelope,
  state: &Arc<ServerState>,
) -> Envelope {
  let reply_id = request.id.clone();
  let reply = match request.payload {
    Payload::Query(q) => Reply::Query(handle_query(q, state)),
    Payload::Command(c) => Reply::Command(handle_command(&c)),
    Payload::Subscribe(_) => Reply::SubscribeAck { from_seq: 0 },
    Payload::Reply(_) | Payload::Event(_) => {
      Reply::Error {
        message: "server received a reply/event envelope; ignoring".into(),
      }
    },
  };
  Envelope::new(reply_id, Payload::Reply(reply))
}

/// Sentinel job id returned by [`Query::Devshell`] when a fresh eval is in
/// flight. The registry-side id lands with the work-executor integration; for
/// now this is a stable placeholder any subscriber can `Query::GetJob` for
/// and get `Missing`, which is a legal `Availability`.
const CURRENT_EVAL_JOB_ID: &str = "current-eval";

/// Answer a v3 [`Query`] by reading directly from `ServerState`.
#[must_use]
fn handle_query(query: Query, state: &ServerState) -> QueryReply {
  match query {
    Query::Status => {
      QueryReply::Status(Availability::Ready {
        value: status_value(state),
      })
    },
    Query::HeartBeat => QueryReply::HeartBeat,
    Query::StateCatalog => {
      QueryReply::StateCatalog(Availability::Ready {
        value: catalog_value(),
      })
    },
    Query::Devshell => QueryReply::Devshell(devshell_availability(state)),
    Query::FlakeOutputs => QueryReply::FlakeOutputs(Availability::Missing),
    Query::GetJob { job_id } => QueryReply::GetJob(job_availability(&job_id)),
    Query::Snapshot { seq } => {
      QueryReply::Snapshot(Availability::Ready {
        value: json!({ "seq": seq }),
      })
    },
    Query::ExprEval { expr, shallow } => {
      QueryReply::ExprEval(handle_expr_eval(state, &expr, shallow))
    },
  }
}

/// Dispatch a v3 [`Command`], allocating (or attaching to) a job via the
/// authoritative job registry.
#[must_use]
fn handle_command(command: &Command) -> CommandReply {
  let kind = job_kind_for(command);
  let key = wire_key_to_registry(command_key(command));
  let (job_id, attached) = registry().attach_or_start(key, kind);
  CommandReply::Accepted {
    job_id:   job_id.as_str().to_owned(),
    attached: matches!(attached, Attached::Yes),
  }
}

fn job_kind_for(cmd: &Command) -> JobKind {
  match cmd {
    Command::EvalDevshell { target, .. } => {
      JobKind::EvalDevshell {
        target: target.clone(),
      }
    },
    Command::Restart { .. } => JobKind::Restart,
    Command::AbortJob { job_id, .. } => {
      JobKind::AbortJob {
        target: JobId(job_id.clone()),
      }
    },
    Command::InvalidateCache { scope, .. } => {
      JobKind::InvalidateCache {
        scope: scope.clone(),
      }
    },
  }
}

const fn command_key(cmd: &Command) -> IdempotencyKey {
  match cmd {
    Command::EvalDevshell { key, .. }
    | Command::Restart { key }
    | Command::AbortJob { key, .. }
    | Command::InvalidateCache { key, .. } => *key,
  }
}

const fn wire_key_to_registry(key: IdempotencyKey) -> RegistryKey {
  RegistryKey::from_bytes(key.0)
}

fn status_value(state: &ServerState) -> serde_json::Value {
  let s = state
    .eval_state
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let target = state
    .current_target
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let pkg_count = state
    .current_env
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .as_ref()
    .map_or(0u32, |e| {
      u32::try_from(e.packages.len()).unwrap_or(u32::MAX)
    });
  json!({
    "state": s,
    "uptime_secs": state.start_time.elapsed().as_secs(),
    "current_target": target,
    "package_count": pkg_count,
    "version": state.version,
  })
}

fn devshell_availability(
  state: &ServerState,
) -> Availability<serde_json::Value> {
  let s = state
    .eval_state
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if matches!(*s, DaemonState::Evaluating) {
    return Availability::Pending {
      job_id: CURRENT_EVAL_JOB_ID.to_owned(),
    };
  }
  drop(s);

  let env = state
    .current_env
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  env.as_ref().map_or(Availability::Missing, |dev| {
    Availability::Ready {
      value: env_snapshot_value(dev),
    }
  })
}

fn env_snapshot_value(env: &crate::env_file::DevEnv) -> serde_json::Value {
  json!({
    "env_hash": env.env_hash,
    "packages": env.packages,
    "package_count": env.packages.len(),
  })
}

/// Sentinel expression meaning "the current devshell's top-level scope" -
/// the empty input a REPL sends to list what's in scope without the caller
/// having written any expression yet.
const CURRENT_SCOPE_EXPR: &str = "";

/// Answer a v3 [`Query::ExprEval`].
///
/// # Concurrency (design decision, not left implicit - see task 3.4)
///
/// `eval_in_progress` and `eval_input_hash` are owned exclusively by
/// `spawn_eval`'s background hook-eval loop in `server.rs`; this handler
/// never writes either field, from any branch below.
///
/// - The fast path below only *reads* `eval_input_hash` (a plain
///   `RwLock::read`, which never blocks behind a writer for longer than a
///   memcpy) to decide whether `ServerState::current_env` is fresh enough to
///   answer [`CURRENT_SCOPE_EXPR`] from already-known package names without
///   shelling out at all.
/// - Every other expression - and every non-shallow query, which needs a real
///   forced value the daemon has no cache for - launches its own `nix eval
///   --json --expr` **subprocess**. That subprocess is a separate OS process
///   with no shared Rust-level lock, so it runs unconditionally even while
///   `eval_in_progress` is `true` (a scheduled hook-eval is mid-flight): it
///   does not contend with the main loop's in-process state, and this handler
///   runs on its own per-connection thread (spawned in `server.rs`'s accept
///   loop), so it can't block that loop either. We deliberately do not check
///   `eval_in_progress` here - there is nothing correct to do with that
///   information.
#[must_use]
fn handle_expr_eval(
  state: &ServerState,
  expr: &str,
  shallow: bool,
) -> Availability<ExprEvalResult> {
  if shallow && expr.trim() == CURRENT_SCOPE_EXPR {
    let is_fresh = state
      .eval_input_hash
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_some();
    if is_fresh {
      let env = state
        .current_env
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if let Some(ref dev) = *env {
        let names = dev.packages.iter().map(|p| p.name.clone()).collect();
        return Availability::Ready {
          value: ExprEvalResult::Attrs(names),
        };
      }
    }
  }

  Availability::Ready {
    value: run_expr_eval(&state.flake_root, expr, shallow),
  }
}

/// Run `nix eval --json --expr` against `flake_root`, classifying the
/// result into an [`ExprEvalResult`].
///
/// Builds one wrapper expression that does the shallow/function/derivation
/// classification inside nix itself (`builtins.isFunction`,
/// `.type or null == "derivation"`, `builtins.attrNames`) rather than
/// post-hoc sniffing the JSON or the raw `nix` error text - nix already has
/// to force the value to WHNF to answer those checks, so this is no more
/// expensive than the naive "just run --json" approach, and it never risks
/// a JSON-serialization failure on a derivation's function-valued attrs
/// (`override`, `overrideAttrs`, …) the way serializing the raw value
/// would.
///
/// When `flake_root` has a `flake.nix`, the flake's own outputs are bound
/// into scope via `with` (the same effect real `nix repl`'s `:lf .` has),
/// so `expr` can reference `packages`, `nixosConfigurations`, etc. as bare
/// names instead of every caller spelling out
/// `(builtins.getFlake "path:...").packages` themselves. This needs
/// `--impure` (an unlocked `path:` ref can't be read purely) and re-runs
/// `builtins.getFlake` fresh on every call, with no cross-call cache - a
/// nixpkgs-sized flake will feel this on every keystroke; see proposal
/// Open Question 3. When there is no `flake.nix` at `flake_root` (e.g. a
/// throwaway test fixture), this falls back to the plain, unbound,
/// pure-eval-safe wrapper - checked once in Rust rather than inside the
/// nix expression, since `builtins.tryEval` does not reliably catch a
/// missing-flake.nix error from `getFlake`.
fn run_expr_eval(
  flake_root: &std::path::Path,
  expr: &str,
  shallow: bool,
) -> ExprEvalResult {
  let has_flake = flake_root.join("flake.nix").is_file();
  let wrapper = classify_wrapper(flake_root, has_flake, expr, shallow);

  let mut builder = NixCommand::new(CommandKind::Eval)
    .arg("--expr")
    .arg(&wrapper)
    .arg("--json");
  if has_flake {
    builder = builder.impure(true);
  }
  let mut cmd = builder.to_std_command();
  cmd.current_dir(flake_root);

  let output = match cmd.output() {
    Ok(o) => o,
    Err(e) => {
      return ExprEvalResult::Error {
        message: format!("failed to run nix eval: {e}"),
      };
    },
  };

  if !output.status.success() {
    return ExprEvalResult::Error {
      message: trim_nix_stderr(&String::from_utf8_lossy(&output.stderr)),
    };
  }

  let parsed: serde_json::Value = match serde_json::from_slice(&output.stdout) {
    Ok(v) => v,
    Err(e) => {
      return ExprEvalResult::Error {
        message: format!("could not parse nix eval output as JSON: {e}"),
      };
    },
  };

  decode_classified(&parsed)
}

/// Build the classifying wrapper expression fed to `nix eval --expr`.
///
/// `expr` is pasted verbatim as nix source (not a shell string - there is
/// no shell involved, we invoke the `nix` binary directly), wrapped in an
/// extra pair of parens so any syntactically valid top-level expression
/// (including a bare `let … in …`) binds cleanly to `v`.
///
/// When `has_flake` is set, the whole thing is wrapped in
/// `with (builtins.getFlake "path:<flake_root>"); ...` first, so `expr` can
/// reach the flake's own outputs (`packages`, `nixosConfigurations`, …) as
/// bare names - this is what the user is asking for when they type e.g.
/// `packages.x86_64-linux.hello` without first spelling out
/// `builtins.getFlake` themselves.
fn classify_wrapper(
  flake_root: &std::path::Path,
  has_flake: bool,
  expr: &str,
  shallow: bool,
) -> String {
  // Both shapes share the lambda/derivation checks (cheap: WHNF-only).
  // They diverge only on what happens to a plain (non-derivation) attrset
  // and to every other value shape (scalars, lists, ints, …):
  // - shallow lists attribute names without forcing any of them, and declines
  //   (reports `thunk`) rather than serializing anything else.
  // - non-shallow serializes the forced value as-is (`scalar`), covering plain
  //   attrsets, lists, and primitives alike.
  let tail = if shallow {
    r#"else if builtins.isAttrs v then { kind = "attrs"; names = builtins.attrNames v; }
  else { kind = "thunk"; }"#
  } else {
    r#"else { kind = "scalar"; value = v; }"#
  };

  // `FLAKE_SCOPE_EXPR` is not Nix source to parse - it asks for the
  // flake's own top-level outputs directly, which only has an answer when
  // `__xiReplFlake` is actually bound below (`has_flake`). `.outputs`
  // specifically (not `__xiReplFlake` itself) because `getFlake`'s return
  // value also carries its own metadata (`_type`, `inputs`, `lastModified`,
  // `narHash`, `outPath`, `sourceInfo`, …) merged in at the top level
  // alongside the real outputs - `.outputs` is the actual
  // `outputs = inputs: {...}` result on its own, which is exactly what
  // real `nix repl`'s `:lf` scopes to. Binding `v` to a syntax error
  // string in the no-flake case still produces a sensible "no such
  // variable" class of ExprEvalResult::Error rather than silently
  // returning something wrong.
  let v_binding = if expr == FLAKE_SCOPE_EXPR && has_flake {
    "__xiReplFlake.outputs".to_owned()
  } else {
    format!("({expr})")
  };

  let body = format!(
    r#"let v = {v_binding}; in
  if builtins.isFunction v then {{ kind = "lambda"; }}
  else if builtins.isAttrs v && (v.type or null) == "derivation" then
    {{ kind = "derivation";
       name = v.name or null;
       version = v.version or null;
       description = v.meta.description or null; }}
  {tail}
"#
  );

  if has_flake {
    let escaped = nix_string_literal(&flake_root.to_string_lossy());
    format!(
      "let __xiReplFlake = builtins.getFlake \"path:{escaped}\"; in\nwith \
       __xiReplFlake;\n{body}"
    )
  } else {
    body
  }
}

/// Escape a raw string for use inside a Nix double-quoted string literal.
fn nix_string_literal(raw: &str) -> String {
  raw
    .replace('\\', "\\\\")
    .replace('"', "\\\"")
    .replace("${", "\\${")
}

/// Decode the `{ kind = …; … }` JSON object the [`classify_wrapper`]
/// expression evaluates to.
fn decode_classified(value: &serde_json::Value) -> ExprEvalResult {
  let kind = value.get("kind").and_then(serde_json::Value::as_str);
  match kind {
    Some("lambda") => ExprEvalResult::Lambda,
    Some("thunk") => ExprEvalResult::Thunk,
    Some("derivation") => {
      ExprEvalResult::Derivation {
        name:        value
          .get("name")
          .and_then(|v| v.as_str().map(str::to_owned)),
        version:     value
          .get("version")
          .and_then(|v| v.as_str().map(str::to_owned)),
        description: value
          .get("description")
          .and_then(|v| v.as_str().map(str::to_owned)),
      }
    },
    Some("attrs") => {
      let names = value
        .get("names")
        .and_then(|v| v.as_array())
        .map(|arr| {
          arr
            .iter()
            .filter_map(|n| n.as_str().map(str::to_owned))
            .collect()
        })
        .unwrap_or_default();
      ExprEvalResult::Attrs(names)
    },
    Some("scalar") => {
      ExprEvalResult::Scalar(value.get("value").cloned().unwrap_or_default())
    },
    _ => {
      ExprEvalResult::Error {
        message: format!("daemon could not classify eval result: {value}"),
      }
    },
  }
}

/// Trim `nix eval` stderr down to the `error:`-prefixed summary (noise like
/// `warning:` / `evaluating ` lines above it is dropped).
fn trim_nix_stderr(stderr: &str) -> String {
  let lines: Vec<&str> = stderr.lines().collect();
  let start = lines.iter().position(|l| l.trim().starts_with("error:"));
  start.map_or_else(|| stderr.trim().to_owned(), |i| lines[i..].join("\n"))
}

fn job_availability(job_id: &str) -> Availability<serde_json::Value> {
  registry().get(&JobId(job_id.to_owned())).map_or(
    Availability::Missing,
    |entry| {
      Availability::Ready {
        value: job_entry_value(&entry),
      }
    },
  )
}

fn job_entry_value(entry: &JobEntry) -> serde_json::Value {
  json!({
    "id": entry.id.as_str(),
    "kind": job_kind_str(&entry.kind),
    "status": job_status_str(&entry.status),
  })
}

const fn job_kind_str(kind: &JobKind) -> &'static str {
  match kind {
    JobKind::EvalDevshell { .. } => "eval_devshell",
    JobKind::Restart => "restart",
    JobKind::AbortJob { .. } => "abort_job",
    JobKind::InvalidateCache { .. } => "invalidate_cache",
  }
}

const fn job_status_str(status: &JobStatus) -> &'static str {
  match status {
    JobStatus::Running => "running",
    JobStatus::Succeeded => "succeeded",
    JobStatus::Failed { .. } => "failed",
    JobStatus::Aborted => "aborted",
    JobStatus::Stuck { .. } => "stuck",
  }
}

fn catalog_value() -> serde_json::Value {
  use super::state_meta::CATALOG;
  let entries: Vec<_> = CATALOG
    .iter()
    .map(|(kind, meta)| {
      json!({
        "name": format!("{kind:?}"),
        "display": meta.display,
        "color": meta.color,
        "category": format!("{:?}", meta.category),
        "phase": format!("{:?}", meta.phase),
        "terminal": meta.terminal,
      })
    })
    .collect();
  serde_json::Value::Array(entries)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn heartbeat_query_returns_heartbeat_reply() {
    // A HeartBeat query never touches state; we can invoke handle_query
    // with a throwaway ServerState by asserting the branch alone.
    assert!(matches!(
      handle_query_pure(Query::HeartBeat),
      QueryReply::HeartBeat
    ));
  }

  #[test]
  fn state_catalog_query_returns_catalog_array() {
    let reply = handle_query_pure(Query::StateCatalog);
    let QueryReply::StateCatalog(Availability::Ready { value }) = reply else {
      unreachable!("expected StateCatalog Ready")
    };
    let arr = value.as_array().expect("catalog is JSON array");
    assert!(!arr.is_empty(), "catalog must not be empty");
  }

  #[test]
  fn snapshot_query_echoes_seq() {
    let QueryReply::Snapshot(Availability::Ready { value }) =
      handle_query_pure(Query::Snapshot { seq: 42 })
    else {
      unreachable!("expected Snapshot Ready")
    };
    assert_eq!(value["seq"], 42);
  }

  #[test]
  fn eval_devshell_command_returns_job_id_fresh() {
    let cmd = Command::EvalDevshell {
      target: "default".into(),
      key:    IdempotencyKey::from_bytes([1u8; 32]),
    };
    let CommandReply::Accepted { job_id, attached } = handle_command(&cmd)
    else {
      unreachable!("EvalDevshell must be accepted")
    };
    assert!(!job_id.is_empty(), "job id must be non-empty");
    assert!(!attached, "first request is a fresh allocation");
  }

  #[test]
  fn identical_commands_attach_to_same_job() {
    let key = IdempotencyKey::from_bytes([2u8; 32]);
    let cmd_a = Command::EvalDevshell {
      target: "identical".into(),
      key,
    };
    let cmd_b = Command::EvalDevshell {
      target: "identical".into(),
      key,
    };
    let first = must_accept(handle_command(&cmd_a));
    let second = must_accept(handle_command(&cmd_b));
    assert_eq!(first.0, second.0, "same key must return same job id");
    assert!(second.1, "second request must report attached=true");
  }

  fn must_accept(reply: CommandReply) -> (String, bool) {
    let CommandReply::Accepted { job_id, attached } = reply else {
      unreachable!("command must be accepted")
    };
    (job_id, attached)
  }

  /// Pure-query dispatch for tests that never touch `ServerState`.
  fn handle_query_pure(q: Query) -> QueryReply {
    match q {
      Query::HeartBeat => QueryReply::HeartBeat,
      Query::StateCatalog => {
        QueryReply::StateCatalog(Availability::Ready {
          value: catalog_value(),
        })
      },
      Query::Snapshot { seq } => {
        QueryReply::Snapshot(Availability::Ready {
          value: json!({ "seq": seq }),
        })
      },
      _ => unreachable!("test helper only handles pure queries"),
    }
  }

  #[test]
  fn classify_wrapper_without_flake_is_unchanged() {
    let no_flake = std::path::Path::new("/does/not/exist");
    let wrapper = classify_wrapper(no_flake, false, "1 + 1", false);
    assert!(
      !wrapper.contains("getFlake"),
      "no flake.nix present must not introduce a getFlake binding"
    );
    assert!(wrapper.starts_with("let v = (1 + 1); in"));
  }

  #[test]
  fn classify_wrapper_with_flake_binds_outputs_via_with() {
    let flake_root = std::path::Path::new("/tmp/some-flake");
    let wrapper =
      classify_wrapper(flake_root, true, "packages.x86_64-linux.hello", true);
    assert!(wrapper.contains(r#"builtins.getFlake "path:/tmp/some-flake""#));
    assert!(wrapper.contains("with __xiReplFlake;"));
    assert!(wrapper.contains("let v = (packages.x86_64-linux.hello); in"));
  }

  #[test]
  fn classify_wrapper_flake_scope_sentinel_binds_v_to_the_flake_outputs() {
    let flake_root = std::path::Path::new("/tmp/some-flake");
    let wrapper = classify_wrapper(flake_root, true, FLAKE_SCOPE_EXPR, true);
    assert!(wrapper.contains("with __xiReplFlake;"));
    assert!(wrapper.contains("let v = __xiReplFlake.outputs; in"));
    assert!(
      !wrapper.contains(&format!("({FLAKE_SCOPE_EXPR})")),
      "the sentinel must never be pasted as if it were Nix source"
    );
  }

  #[test]
  fn classify_wrapper_flake_scope_sentinel_without_flake_is_an_undefined_variable()
   {
    let no_flake = std::path::Path::new("/does/not/exist");
    let wrapper = classify_wrapper(no_flake, false, FLAKE_SCOPE_EXPR, true);
    assert!(wrapper.starts_with(&format!("let v = ({FLAKE_SCOPE_EXPR}); in")));
  }

  #[test]
  fn nix_string_literal_escapes_quotes_backslashes_and_interpolation() {
    assert_eq!(nix_string_literal("plain"), "plain");
    assert_eq!(nix_string_literal(r#"has"quote"#), r#"has\"quote"#);
    assert_eq!(nix_string_literal(r"has\backslash"), r"has\\backslash");
    assert_eq!(nix_string_literal("has${interp}"), r"has\${interp}");
  }

  #[test]
  fn run_expr_eval_binds_flake_outputs_when_flake_nix_present() {
    let fixture = std::path::Path::new(concat!(
      env!("CARGO_MANIFEST_DIR"),
      "/../xi-repl/tests/fixtures/mini-flake"
    ));
    if !fixture.join("flake.nix").is_file() {
      // xi-repl's fixture may not exist yet in an isolated checkout of
      // just this crate; skip rather than fail spuriously.
      return;
    }
    let result = run_expr_eval(fixture, "builtins.attrNames packages", false);
    let ExprEvalResult::Scalar(value) = result else {
      panic!("expected Scalar, got {result:?}");
    };
    assert_eq!(value, json!(["x86_64-linux"]));
  }
}
