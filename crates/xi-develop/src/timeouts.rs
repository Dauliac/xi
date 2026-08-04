//! Single source of truth for every timeout / budget used by the async CQRS
//! daemon (`xi develop daemon`) and its clients (prompt hook, subscribers,
//! `xi develop events`, …).
//!
//! Layering, lowest to highest precedence:
//!
//! 1. **Defaults** — see [`Timeouts::default`]; the numbers documented in
//!    `openspec/changes/async-daemon-cqrs/proposal.md` under
//!    _"Timeouts are user-configurable"_.
//! 2. **Project config** — `[develop.timeouts]` table in `.xi.toml` at the
//!    flake root.
//! 3. **Environment variables** — `XI_DEVELOP_*` variables override the
//!    project config on a per-field basis.
//!
//! Every field is parsed as milliseconds from TOML / env and stored as a
//! [`Duration`] internally. The subscriber ring is a count (not a duration)
//! and stored as `usize`.

use std::path::Path;
use std::time::Duration;

use tracing::warn;

/// Async-daemon timeouts and budgets.
///
/// See the module docs for the layering rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
  /// Max wall-time a `Query` (fast path) is allowed to take on the daemon
  /// side before the client should treat the daemon as unresponsive.
  pub state_query: Duration,

  /// Per-tick prompt budget. If the hook cannot answer in this window it
  /// must fall through to the offline cache and enqueue for a later tick.
  pub prompt_budget: Duration,

  /// A daemon job that has not progressed for at least this long is
  /// declared "stuck" by the client.
  pub stuck_threshold: Duration,

  /// TTL floor for the hook offline cache. Only trusted if the entry's
  /// `flake.lock` hash still matches the current lock hash.
  pub offline_cache_ttl: Duration,

  /// Per-subscriber ring buffer size. Overflows are surfaced as a
  /// `Gap { from_seq, to_seq }` event.
  pub subscriber_ring_size: usize,

  /// Cadence of the broadcaster's `KeepAlive` frames. A subscriber that
  /// misses two of these in a row assumes the daemon is dead.
  pub keepalive: Duration,

  /// Coalescing window applied to job-progress bursts before broadcasting.
  pub broadcast_batching: Duration,

  /// Idle-shutdown grace: if there are zero subscribers for this long the
  /// daemon self-terminates. Persisted state on disk is untouched.
  pub idle_shutdown: Duration,

  /// Upper bound on the client reconnect backoff.
  pub reconnect_backoff_cap: Duration,
}

impl Default for Timeouts {
  fn default() -> Self {
    Self {
      state_query:           Duration::from_millis(20),
      prompt_budget:         Duration::from_millis(100),
      stuck_threshold:       Duration::from_secs(30),
      offline_cache_ttl:     Duration::from_secs(24 * 60 * 60),
      subscriber_ring_size:  1000,
      keepalive:             Duration::from_secs(30),
      broadcast_batching:    Duration::from_millis(100),
      idle_shutdown:         Duration::from_secs(30 * 60),
      reconnect_backoff_cap: Duration::from_secs(60),
    }
  }
}

impl Timeouts {
  /// Load timeouts by reading `.xi.toml` (`[develop.timeouts]`) at
  /// `flake_dir` (or `.` if `None`), then overlaying process env vars.
  ///
  /// This is the entry point every daemon/client component should use. It
  /// never fails: bad values are logged and dropped, leaving the previous
  /// layer intact.
  #[must_use]
  pub fn load(flake_dir: Option<&Path>) -> Self {
    let dir = flake_dir.unwrap_or_else(|| Path::new("."));
    let config_path = dir.join(".xi.toml");
    let project_doc = std::fs::read_to_string(&config_path)
      .ok()
      .and_then(|raw| match raw.parse::<toml_edit::DocumentMut>() {
        Ok(d) => Some(d),
        Err(e) => {
          warn!("Failed to parse {}: {e}", config_path.display());
          None
        },
      });

    Self::resolve(project_doc.as_ref(), EnvSource::process())
  }

  /// Layered loader.
  ///
  /// Composes [`Timeouts::default`] with an optional parsed `.xi.toml`
  /// document (only the `[develop.timeouts]` table is consulted) and an
  /// environment source. The env source is a lookup closure — process env
  /// in production, a fixture map in tests.
  #[must_use]
  pub fn resolve(
    project_doc: Option<&toml_edit::DocumentMut>,
    env: EnvSource<'_>,
  ) -> Self {
    let mut out = Self::default();

    if let Some(doc) = project_doc {
      apply_toml(&mut out, doc);
    }

    apply_env(&mut out, &env);

    out
  }
}

/// Abstract environment lookup so tests can inject fixtures without
/// touching the real process env.
pub struct EnvSource<'a> {
  lookup: Box<dyn Fn(&str) -> Option<String> + 'a>,
}

impl<'a> EnvSource<'a> {
  /// Real process environment.
  #[must_use]
  pub fn process() -> Self {
    Self {
      lookup: Box::new(|k| std::env::var(k).ok()),
    }
  }

  /// Empty environment — useful for defaults-only or project-only tests.
  #[must_use]
  pub fn empty() -> Self {
    Self {
      lookup: Box::new(|_| None),
    }
  }

  /// Environment backed by an in-memory slice of `(key, value)` pairs.
  #[must_use]
  pub fn from_pairs(pairs: &'a [(&'a str, &'a str)]) -> Self {
    Self {
      lookup: Box::new(move |k| {
        pairs
          .iter()
          .find(|(pk, _)| *pk == k)
          .map(|(_, v)| (*v).to_string())
      }),
    }
  }

  fn get(&self, key: &str) -> Option<String> {
    (self.lookup)(key)
  }
}

fn apply_toml(out: &mut Timeouts, doc: &toml_edit::DocumentMut) {
  let Some(table) = doc
    .get("develop")
    .and_then(|v| v.as_table())
    .and_then(|t| t.get("timeouts"))
    .and_then(|v| v.as_table())
  else {
    return;
  };

  if let Some(ms) = read_u64(table, "state_query_ms") {
    out.state_query = Duration::from_millis(ms);
  }
  if let Some(ms) = read_u64(table, "prompt_budget_ms") {
    out.prompt_budget = Duration::from_millis(ms);
  }
  if let Some(ms) = read_u64(table, "stuck_threshold_ms") {
    out.stuck_threshold = Duration::from_millis(ms);
  }
  if let Some(ms) = read_u64(table, "offline_cache_ttl_ms") {
    out.offline_cache_ttl = Duration::from_millis(ms);
  }
  if let Some(n) = read_u64(table, "subscriber_ring_size") {
    out.subscriber_ring_size = usize::try_from(n).unwrap_or(usize::MAX);
  }
  if let Some(ms) = read_u64(table, "keepalive_ms") {
    out.keepalive = Duration::from_millis(ms);
  }
  if let Some(ms) = read_u64(table, "broadcast_batching_ms") {
    out.broadcast_batching = Duration::from_millis(ms);
  }
  if let Some(ms) = read_u64(table, "idle_shutdown_ms") {
    out.idle_shutdown = Duration::from_millis(ms);
  }
  if let Some(ms) = read_u64(table, "reconnect_backoff_cap_ms") {
    out.reconnect_backoff_cap = Duration::from_millis(ms);
  }
}

fn apply_env(out: &mut Timeouts, env: &EnvSource<'_>) {
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_STATE_QUERY_MS") {
    out.state_query = Duration::from_millis(ms);
  }
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_PROMPT_BUDGET_MS") {
    out.prompt_budget = Duration::from_millis(ms);
  }
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_STUCK_THRESHOLD_MS") {
    out.stuck_threshold = Duration::from_millis(ms);
  }
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_OFFLINE_CACHE_TTL_MS") {
    out.offline_cache_ttl = Duration::from_millis(ms);
  }
  if let Some(n) = parse_env_u64(env, "XI_DEVELOP_SUBSCRIBER_RING_SIZE") {
    out.subscriber_ring_size = usize::try_from(n).unwrap_or(usize::MAX);
  }
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_KEEPALIVE_MS") {
    out.keepalive = Duration::from_millis(ms);
  }
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_BROADCAST_BATCHING_MS") {
    out.broadcast_batching = Duration::from_millis(ms);
  }
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_IDLE_SHUTDOWN_MS") {
    out.idle_shutdown = Duration::from_millis(ms);
  }
  if let Some(ms) = parse_env_u64(env, "XI_DEVELOP_RECONNECT_BACKOFF_CAP_MS") {
    out.reconnect_backoff_cap = Duration::from_millis(ms);
  }
}

fn read_u64(table: &toml_edit::Table, key: &str) -> Option<u64> {
  let item = table.get(key)?;
  let n = item.as_integer()?;
  if n < 0 {
    warn!(
      "Ignoring negative value for [develop.timeouts].{key} = {n}; using \
       previous layer"
    );
    return None;
  }
  #[allow(clippy::cast_sign_loss)]
  Some(n as u64)
}

fn parse_env_u64(env: &EnvSource<'_>, key: &str) -> Option<u64> {
  let raw = env.get(key)?;
  match raw.trim().parse::<u64>() {
    Ok(n) => Some(n),
    Err(e) => {
      warn!("Ignoring invalid env var {key}={raw:?}: {e}; using previous layer");
      None
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse_doc(src: &str) -> toml_edit::DocumentMut {
    src.parse().expect("valid TOML")
  }

  #[test]
  fn defaults_match_proposal() {
    let t = Timeouts::default();
    assert_eq!(t.state_query, Duration::from_millis(20));
    assert_eq!(t.prompt_budget, Duration::from_millis(100));
    assert_eq!(t.stuck_threshold, Duration::from_secs(30));
    assert_eq!(t.offline_cache_ttl, Duration::from_secs(24 * 60 * 60));
    assert_eq!(t.subscriber_ring_size, 1000);
    assert_eq!(t.keepalive, Duration::from_secs(30));
    assert_eq!(t.broadcast_batching, Duration::from_millis(100));
    assert_eq!(t.idle_shutdown, Duration::from_secs(30 * 60));
    assert_eq!(t.reconnect_backoff_cap, Duration::from_secs(60));
  }

  #[test]
  fn empty_env_and_no_project_yields_defaults() {
    let t = Timeouts::resolve(None, EnvSource::empty());
    assert_eq!(t, Timeouts::default());
  }

  #[test]
  fn project_config_overrides_default() {
    let doc = parse_doc(
      r#"
[develop.timeouts]
state_query_ms       = 50
prompt_budget_ms     = 250
stuck_threshold_ms   = 15000
offline_cache_ttl_ms = 3600000
subscriber_ring_size = 2048
keepalive_ms         = 45000
broadcast_batching_ms = 200
idle_shutdown_ms     = 60000
reconnect_backoff_cap_ms = 120000
"#,
    );

    let t = Timeouts::resolve(Some(&doc), EnvSource::empty());

    assert_eq!(t.state_query, Duration::from_millis(50));
    assert_eq!(t.prompt_budget, Duration::from_millis(250));
    assert_eq!(t.stuck_threshold, Duration::from_millis(15_000));
    assert_eq!(t.offline_cache_ttl, Duration::from_millis(3_600_000));
    assert_eq!(t.subscriber_ring_size, 2048);
    assert_eq!(t.keepalive, Duration::from_millis(45_000));
    assert_eq!(t.broadcast_batching, Duration::from_millis(200));
    assert_eq!(t.idle_shutdown, Duration::from_millis(60_000));
    assert_eq!(t.reconnect_backoff_cap, Duration::from_millis(120_000));
  }

  #[test]
  fn env_overrides_project_config() {
    let doc = parse_doc(
      r#"
[develop.timeouts]
state_query_ms   = 50
prompt_budget_ms = 250
"#,
    );
    let pairs = [
      ("XI_DEVELOP_STATE_QUERY_MS", "5"),
      ("XI_DEVELOP_PROMPT_BUDGET_MS", "80"),
      ("XI_DEVELOP_STUCK_THRESHOLD_MS", "10000"),
      ("XI_DEVELOP_SUBSCRIBER_RING_SIZE", "512"),
    ];
    let env = EnvSource::from_pairs(&pairs);

    let t = Timeouts::resolve(Some(&doc), env);

    // Env wins over project.
    assert_eq!(t.state_query, Duration::from_millis(5));
    assert_eq!(t.prompt_budget, Duration::from_millis(80));
    // Env-only override still applies.
    assert_eq!(t.stuck_threshold, Duration::from_millis(10_000));
    assert_eq!(t.subscriber_ring_size, 512);
    // Untouched fields fall back to defaults.
    assert_eq!(t.offline_cache_ttl, Timeouts::default().offline_cache_ttl);
    assert_eq!(t.keepalive, Timeouts::default().keepalive);
  }

  #[test]
  fn env_alone_overrides_default() {
    let pairs = [("XI_DEVELOP_PROMPT_BUDGET_MS", "42")];
    let t = Timeouts::resolve(None, EnvSource::from_pairs(&pairs));
    assert_eq!(t.prompt_budget, Duration::from_millis(42));
    assert_eq!(t.state_query, Timeouts::default().state_query);
  }

  #[test]
  fn invalid_env_value_falls_through_to_project() {
    let doc = parse_doc(
      r#"
[develop.timeouts]
prompt_budget_ms = 250
"#,
    );
    let pairs = [("XI_DEVELOP_PROMPT_BUDGET_MS", "not-a-number")];
    let t = Timeouts::resolve(Some(&doc), EnvSource::from_pairs(&pairs));
    assert_eq!(t.prompt_budget, Duration::from_millis(250));
  }

  #[test]
  fn negative_toml_value_ignored() {
    let doc = parse_doc(
      r#"
[develop.timeouts]
prompt_budget_ms = -1
"#,
    );
    let t = Timeouts::resolve(Some(&doc), EnvSource::empty());
    assert_eq!(t.prompt_budget, Timeouts::default().prompt_budget);
  }

  #[test]
  fn missing_develop_table_yields_defaults() {
    let doc = parse_doc(
      r#"
[ci]
backend = "auto"
"#,
    );
    let t = Timeouts::resolve(Some(&doc), EnvSource::empty());
    assert_eq!(t, Timeouts::default());
  }
}
