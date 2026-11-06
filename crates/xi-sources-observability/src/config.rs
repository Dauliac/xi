//! The `[sources.observability]` config surface.
//!
//! Three settings, each off or inert by default, because every one of
//! them is an operator decision: a log level, a Prometheus port, an OTLP
//! endpoint. The defaults produce a daemon that logs at `info` to its
//! scope's sink and opens no socket at all.
//!
//! This is a *nested* table under `[sources]` rather than three
//! top-level keys, so the exporter's future-expansion keys
//! (`prometheus.bind`) have somewhere to live without widening the
//! closed `[sources]` key set. `xi-sources-config` owns that outer
//! table; this module owns the sub-table and parses it directly, which
//! keeps the heavyweight observability types out of the config loader.

use std::{fmt, path::Path, str::FromStr, time::Duration};

use serde::{Deserialize, Serialize};
use xi_core::sources::config::ConfigError;

/// The sub-table of `[sources]` this module owns.
pub const OBSERVABILITY_KEY: &str = "observability";

/// Every key the `[sources.observability]` table accepts.
///
/// Closed, like the outer table: a typo is a load-time error rather
/// than a setting that silently does nothing. `prometheus` and `otlp`
/// are themselves sub-tables.
pub const OBSERVABILITY_KEYS: &[&str] = &["log-level", "prometheus", "otlp"];

/// Every key `[sources.observability.prometheus]` accepts.
pub const PROMETHEUS_KEYS: &[&str] = &["port", "bind"];

/// Every key `[sources.observability.otlp]` accepts.
pub const OTLP_KEYS: &[&str] = &["endpoint", "batch-max-latency"];

/// Lowest port the exporter will bind.
///
/// Below 1024 needs privileges the daemon should not be asking for, and
/// a metrics endpoint is never worth a capability grant.
pub const MIN_PROMETHEUS_PORT: u16 = 1024;

/// Hard cap on OTLP export latency, per spec.
pub const MAX_OTLP_BATCH_LATENCY: Duration = Duration::from_secs(5);

/// How verbose the daemon's structured log is.
#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  PartialOrd,
  Ord,
  Default,
  Serialize,
  Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
  /// Everything, including per-file-operation detail.
  Trace,
  /// Enough to follow a tick's decisions.
  Debug,
  /// One line per phase outcome. The default.
  #[default]
  Info,
  /// Only what needs an operator's attention eventually.
  Warn,
  /// Only what is already broken.
  Error,
}

impl LogLevel {
  /// Every spelling, in the order errors list them.
  pub const ALL: &'static [&'static str] =
    &["trace", "debug", "info", "warn", "error"];

  /// The wire spelling, matching the serde representation.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Trace => "trace",
      Self::Debug => "debug",
      Self::Info => "info",
      Self::Warn => "warn",
      Self::Error => "error",
    }
  }

  /// The `tracing` level this maps to.
  #[must_use]
  pub const fn to_tracing(self) -> tracing::Level {
    match self {
      Self::Trace => tracing::Level::TRACE,
      Self::Debug => tracing::Level::DEBUG,
      Self::Info => tracing::Level::INFO,
      Self::Warn => tracing::Level::WARN,
      Self::Error => tracing::Level::ERROR,
    }
  }

  /// Whether a record at `level` passes this threshold.
  #[must_use]
  pub fn admits(self, level: Self) -> bool {
    level >= self
  }
}

impl FromStr for LogLevel {
  type Err = String;

  fn from_str(raw: &str) -> Result<Self, Self::Err> {
    match raw.trim().to_ascii_lowercase().as_str() {
      "trace" => Ok(Self::Trace),
      "debug" => Ok(Self::Debug),
      "info" => Ok(Self::Info),
      "warn" | "warning" => Ok(Self::Warn),
      "error" => Ok(Self::Error),
      other => {
        Err(format!(
          "invalid 'log-level' value '{other}': expected one of {}",
          Self::ALL.join(", ")
        ))
      },
    }
  }
}

impl fmt::Display for LogLevel {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The exporter's settings, when an operator asked for one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrometheusConfig {
  /// Loopback port to bind.
  pub port: u16,
  /// Address to bind. Loopback only; see [`validate_bind`].
  ///
  /// A key rather than a constant so a misconfiguration is reported at
  /// load time with a name the operator can search for, instead of the
  /// exporter silently ignoring what they asked for.
  pub bind: String,
}

/// The address the exporter binds when no layer names one.
pub const DEFAULT_PROMETHEUS_BIND: &str = "127.0.0.1";

impl PrometheusConfig {
  /// An exporter on `port`, bound to loopback.
  #[must_use]
  pub fn loopback(port: u16) -> Self {
    Self {
      port,
      bind: DEFAULT_PROMETHEUS_BIND.to_owned(),
    }
  }
}

/// The OTLP exporter's settings, when an operator named a collector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtlpConfig {
  /// Base endpoint of the collector, e.g. `http://collector:4318`.
  pub endpoint:          String,
  /// Longest a root span may sit in the batch before it is flushed.
  ///
  /// Capped at [`MAX_OTLP_BATCH_LATENCY`] so the spec's "within 5
  /// seconds" guarantee cannot be configured away.
  pub batch_max_latency: Duration,
}

impl OtlpConfig {
  /// Export to `endpoint` with the maximum sanctioned batch latency.
  #[must_use]
  pub const fn new(endpoint: String) -> Self {
    Self {
      endpoint,
      batch_max_latency: MAX_OTLP_BATCH_LATENCY,
    }
  }
}

/// The resolved observability settings for one scope.
///
/// Unlike `xi-sources-config`'s layered types this is already resolved:
/// there is no useful "partially specified" state for three independent
/// settings, and the precedence that matters (env over file) is applied
/// by [`ObservabilityConfig::load`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
  /// Log verbosity threshold.
  pub log_level:  LogLevel,
  /// Exporter settings, or `None` for "do not open a socket".
  pub prometheus: Option<PrometheusConfig>,
  /// OTLP settings, or `None` for "do not export traces".
  pub otlp:       Option<OtlpConfig>,
}

impl Default for ObservabilityConfig {
  fn default() -> Self {
    Self {
      log_level:  LogLevel::Info,
      prometheus: None,
      otlp:       None,
    }
  }
}

/// Env var naming the log level.
pub const LOG_LEVEL_ENV: &str = "XI_SOURCES_OBSERVABILITY_LOG_LEVEL";
/// Env var naming the exporter port.
pub const PROMETHEUS_PORT_ENV: &str = "XI_SOURCES_PROMETHEUS_PORT";
/// Env var naming the exporter bind address.
///
/// Exists so the spec's "non-loopback bind is refused" path is
/// reachable without inventing a config key that only exists to be
/// rejected.
pub const PROMETHEUS_BIND_ENV: &str = "XI_SOURCES_PROMETHEUS_BIND";
/// Env var naming the OTLP collector.
pub const OTLP_ENDPOINT_ENV: &str = "XI_SOURCES_OTLP_ENDPOINT";

impl ObservabilityConfig {
  /// Parse the `[sources.observability]` table out of a config file's
  /// text.
  ///
  /// A document without the table is not an error: it yields the
  /// defaults, so a `sources.toml` written before this capability
  /// existed keeps working.
  ///
  /// # Errors
  ///
  /// Returns a [`ConfigError::File`] naming `path` and the 1-based line
  /// of the offending key for a syntax error, an unknown key, a
  /// wrongly-typed value, or a value outside its sanctioned range.
  pub fn from_toml_str(path: &Path, raw: &str) -> Result<Self, ConfigError> {
    // `Document`, not `DocumentMut`: only the immutable parse retains
    // source spans, and without spans every error reports line 1.
    let doc = toml_edit::Document::parse(raw).map_err(|e| {
      ConfigError::File {
        path:    path.to_path_buf(),
        line:    e.span().map_or(1, |span| line_of(raw, span.start)),
        message: e.message().to_owned(),
      }
    })?;

    let Some(table) = doc
      .get("sources")
      .and_then(toml_edit::Item::as_table)
      .and_then(|sources| sources.get(OBSERVABILITY_KEY))
      .and_then(toml_edit::Item::as_table)
    else {
      return Ok(Self::default());
    };

    let fail = |key: &str, message: String| {
      ConfigError::File {
        path: path.to_path_buf(),
        line: line_of_key(raw, table, key),
        message,
      }
    };
    reject_unknown(table, OBSERVABILITY_KEYS, "observability", &fail)?;

    let mut config = Self::default();

    if let Some(item) = table.get("log-level") {
      let raw_level = item.as_str().ok_or_else(|| {
        fail("log-level", "'log-level' must be a string".into())
      })?;
      config.log_level =
        raw_level.parse().map_err(|msg| fail("log-level", msg))?;
    }

    config.prometheus = parse_prometheus(raw, table, &fail)?;
    config.otlp = parse_otlp(raw, table, &fail)?;
    Ok(config)
  }

  /// Parse the table out of a config file, treating "absent" as
  /// "defaults".
  ///
  /// # Errors
  ///
  /// Propagates [`Self::from_toml_str`] errors and any I/O error other
  /// than `NotFound`.
  pub fn from_toml_file(path: &Path) -> Result<Self, ConfigError> {
    match std::fs::read_to_string(path) {
      Ok(raw) => Self::from_toml_str(path, &raw),
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
        Ok(Self::default())
      },
      Err(err) => {
        Err(ConfigError::File {
          path:    path.to_path_buf(),
          line:    1,
          message: err.to_string(),
        })
      },
    }
  }

  /// Apply `XI_SOURCES_*` overrides on top of this config.
  ///
  /// The environment is the higher rung, matching the ladder
  /// `xi-sources-config` already documents: a daemon started by hand
  /// with an env override wins over the file the module wrote.
  ///
  /// # Errors
  ///
  /// Returns a [`ConfigError::Env`] naming the offending variable.
  pub fn apply_env_with(
    mut self,
    lookup: &dyn Fn(&str) -> Option<String>,
  ) -> Result<Self, ConfigError> {
    let present = |var: &str| lookup(var).filter(|v| !v.trim().is_empty());
    let env_fail = |var: &str, message: String| {
      ConfigError::Env {
        var: var.to_owned(),
        message,
      }
    };

    if let Some(raw) = present(LOG_LEVEL_ENV) {
      self.log_level =
        raw.parse().map_err(|msg| env_fail(LOG_LEVEL_ENV, msg))?;
    }

    if let Some(raw) = present(PROMETHEUS_PORT_ENV) {
      let port =
        parse_port(&raw).map_err(|msg| env_fail(PROMETHEUS_PORT_ENV, msg))?;
      self.prometheus = Some(PrometheusConfig::loopback(port));
    }

    // A bind without a port configures nothing, so it is reported
    // rather than ignored: an operator who sets only the address has
    // misunderstood the pair and would otherwise see no exporter and no
    // explanation.
    if let Some(bind) = present(PROMETHEUS_BIND_ENV) {
      let exporter = self.prometheus.as_mut().ok_or_else(|| {
        env_fail(
          PROMETHEUS_BIND_ENV,
          format!(
            "'{PROMETHEUS_BIND_ENV}' names an address but no port is \
             configured; set '{PROMETHEUS_PORT_ENV}' or 'prometheus.port' too"
          ),
        )
      })?;
      exporter.bind = bind;
    }

    if let Some(endpoint) = present(OTLP_ENDPOINT_ENV) {
      validate_otlp_endpoint(&endpoint)
        .map_err(|msg| env_fail(OTLP_ENDPOINT_ENV, msg))?;
      self.otlp = Some(OtlpConfig::new(endpoint));
    }

    Ok(self)
  }

  /// Load the file for `path`, then apply the process environment.
  ///
  /// # Errors
  ///
  /// Propagates [`Self::from_toml_file`] and [`Self::apply_env_with`]
  /// errors.
  pub fn load(path: &Path) -> Result<Self, ConfigError> {
    Self::from_toml_file(path)?.apply_env_with(&|key| std::env::var(key).ok())
  }
}

/// Reject any key outside `allowed` in `table`.
fn reject_unknown(
  table: &toml_edit::Table,
  allowed: &[&str],
  table_name: &str,
  fail: &dyn Fn(&str, String) -> ConfigError,
) -> Result<(), ConfigError> {
  for (key, _) in table {
    if !allowed.contains(&key) {
      return Err(fail(
        key,
        format!(
          "unknown key '{key}' in [sources.{table_name}] (expected one of {})",
          allowed.join(", ")
        ),
      ));
    }
  }
  Ok(())
}

fn parse_prometheus(
  raw: &str,
  parent: &toml_edit::Table,
  fail: &dyn Fn(&str, String) -> ConfigError,
) -> Result<Option<PrometheusConfig>, ConfigError> {
  let Some(item) = parent.get("prometheus") else {
    return Ok(None);
  };
  let table = item
    .as_table()
    .ok_or_else(|| fail("prometheus", "'prometheus' must be a table".into()))?;

  let sub_fail = |key: &str, message: String| {
    match fail(key, message.clone()) {
      ConfigError::File { path, .. } => {
        ConfigError::File {
          path,
          line: line_of_key(raw, table, key),
          message,
        }
      },
      env @ ConfigError::Env { .. } => env,
    }
  };
  reject_unknown(
    table,
    PROMETHEUS_KEYS,
    "observability.prometheus",
    &sub_fail,
  )?;

  // Null means "no exporter", which is the documented default spelling
  // in the Nix option tree. Absent means the same thing.
  let Some(port_item) = table.get("port") else {
    return Ok(None);
  };
  let port = port_item
    .as_integer()
    .ok_or_else(|| sub_fail("port", "'port' must be an integer".into()))?;
  let port = u16::try_from(port)
    .map_err(|_| sub_fail("port", port_range_message(port)))
    .and_then(|port| {
      if port < MIN_PROMETHEUS_PORT {
        Err(sub_fail("port", port_range_message(i64::from(port))))
      } else {
        Ok(port)
      }
    })?;

  let mut config = PrometheusConfig::loopback(port);
  if let Some(bind) = table.get("bind") {
    let bind = bind
      .as_str()
      .ok_or_else(|| sub_fail("bind", "'bind' must be a string".into()))?;
    // `clone_into` reuses the loopback default's allocation instead of
    // dropping it and allocating a second time.
    bind.clone_into(&mut config.bind);
  }
  Ok(Some(config))
}

fn port_range_message(port: i64) -> String {
  format!(
    "invalid 'port' value {port}: expected an integer in \
     {MIN_PROMETHEUS_PORT}-65535"
  )
}

/// Parse a port from an env var's text.
fn parse_port(raw: &str) -> Result<u16, String> {
  let parsed: i64 = raw.trim().parse().map_err(|_| {
    format!(
      "invalid port '{raw}': expected an integer in \
       {MIN_PROMETHEUS_PORT}-65535"
    )
  })?;
  let port = u16::try_from(parsed).map_err(|_| port_range_message(parsed))?;
  if port < MIN_PROMETHEUS_PORT {
    return Err(port_range_message(parsed));
  }
  Ok(port)
}

fn parse_otlp(
  raw: &str,
  parent: &toml_edit::Table,
  fail: &dyn Fn(&str, String) -> ConfigError,
) -> Result<Option<OtlpConfig>, ConfigError> {
  let Some(item) = parent.get("otlp") else {
    return Ok(None);
  };
  let table = item
    .as_table()
    .ok_or_else(|| fail("otlp", "'otlp' must be a table".into()))?;

  let sub_fail = |key: &str, message: String| {
    match fail(key, message.clone()) {
      ConfigError::File { path, .. } => {
        ConfigError::File {
          path,
          line: line_of_key(raw, table, key),
          message,
        }
      },
      env @ ConfigError::Env { .. } => env,
    }
  };
  reject_unknown(table, OTLP_KEYS, "observability.otlp", &sub_fail)?;

  let Some(endpoint_item) = table.get("endpoint") else {
    return Ok(None);
  };
  let endpoint = endpoint_item.as_str().ok_or_else(|| {
    sub_fail("endpoint", "'endpoint' must be a string".into())
  })?;
  validate_otlp_endpoint(endpoint).map_err(|msg| sub_fail("endpoint", msg))?;

  let mut config = OtlpConfig::new(endpoint.to_owned());
  if let Some(latency) = table.get("batch-max-latency") {
    let text = latency.as_str().ok_or_else(|| {
      sub_fail(
        "batch-max-latency",
        "'batch-max-latency' must be a string such as '5s'".into(),
      )
    })?;
    let parsed =
      humantime_like(text).map_err(|msg| sub_fail("batch-max-latency", msg))?;
    if parsed > MAX_OTLP_BATCH_LATENCY || parsed.is_zero() {
      return Err(sub_fail(
        "batch-max-latency",
        format!(
          "invalid 'batch-max-latency' value '{text}': must be greater than \
           zero and at most {}s",
          MAX_OTLP_BATCH_LATENCY.as_secs()
        ),
      ));
    }
    config.batch_max_latency = parsed;
  }
  Ok(Some(config))
}

/// Reject an OTLP endpoint that is not an absolute http(s) URL.
///
/// # Errors
///
/// Returns the reason the endpoint is unusable, without a location
/// prefix.
pub fn validate_otlp_endpoint(raw: &str) -> Result<(), String> {
  let endpoint = raw.trim();
  if endpoint.is_empty() {
    return Err("invalid 'endpoint': empty".to_owned());
  }
  let Some(rest) = endpoint
    .strip_prefix("http://")
    .or_else(|| endpoint.strip_prefix("https://"))
  else {
    return Err(format!(
      "invalid 'endpoint' value '{endpoint}': expected an http:// or \
       https:// URL, as in 'http://collector:4318'"
    ));
  };
  if rest.split('/').next().unwrap_or_default().is_empty() {
    return Err(format!(
      "invalid 'endpoint' value '{endpoint}': missing host"
    ));
  }
  Ok(())
}

/// Parse a short duration string (`5s`, `500ms`, `2500 ms`).
///
/// Only the units a batch latency is ever spelled in, so the config
/// surface does not drag in a parser for `1y 3mon`.
fn humantime_like(raw: &str) -> Result<Duration, String> {
  let text = raw.trim();
  let bad = || {
    format!(
      "invalid 'batch-max-latency' value '{text}': expected a duration such \
       as '5s' or '500ms'"
    )
  };
  let (digits, unit) = text
    .find(|c: char| !c.is_ascii_digit())
    .map_or((text, "s"), |idx| (&text[..idx], text[idx..].trim()));
  let value: u64 = digits.parse().map_err(|_| bad())?;
  match unit {
    "ms" => Ok(Duration::from_millis(value)),
    "s" | "sec" | "secs" => Ok(Duration::from_secs(value)),
    _ => Err(bad()),
  }
}

/// Refuse any bind address outside loopback.
///
/// A metrics endpoint carries the shape of a machine's deployment
/// history, and `/healthz` is a free liveness oracle for anyone who can
/// reach it. Neither belongs on a routable interface, so the exporter
/// refuses rather than warning: a warning in a log nobody reads is how
/// this ends up exposed.
///
/// # Errors
///
/// Returns a message naming the rejected address.
pub fn validate_bind(bind: &str) -> Result<std::net::IpAddr, String> {
  let addr: std::net::IpAddr = bind.trim().parse().map_err(|_| {
    format!(
      "invalid 'prometheus.bind' value '{bind}': expected an IP address, and \
       it must be a loopback one such as {DEFAULT_PROMETHEUS_BIND}"
    )
  })?;
  if addr.is_loopback() {
    Ok(addr)
  } else {
    Err(format!(
      "refusing to bind the sources exporter to non-loopback address \
       '{bind}': /metrics and /healthz are loopback-only surfaces. Use \
       {DEFAULT_PROMETHEUS_BIND} and put a reverse proxy in front if a remote \
       scraper needs them."
    ))
  }
}

/// 1-based line number of a byte offset within `raw`.
fn line_of(raw: &str, offset: usize) -> usize {
  let clamped = offset.min(raw.len());
  raw[..clamped].bytes().filter(|b| *b == b'\n').count() + 1
}

/// 1-based line of `key` inside `table`, falling back to the header.
fn line_of_key(raw: &str, table: &toml_edit::Table, key: &str) -> usize {
  table
    .key(key)
    .and_then(toml_edit::Key::span)
    .or_else(|| table.span())
    .map_or(1, |span| line_of(raw, span.start))
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use std::collections::HashMap;

  use super::*;

  fn parse(raw: &str) -> Result<ObservabilityConfig, ConfigError> {
    ObservabilityConfig::from_toml_str(Path::new("sources.toml"), raw)
  }

  fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
      .iter()
      .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
      .collect();
    move |key: &str| map.get(key).cloned()
  }

  // -- defaults -----------------------------------------------------------

  #[test]
  fn no_table_means_info_and_no_sockets() {
    let config =
      parse("[sources]\nremote = 'https://git.example/cfg'\n").unwrap();
    assert_eq!(config, ObservabilityConfig::default());
    assert_eq!(config.log_level, LogLevel::Info);
    assert!(config.prometheus.is_none());
    assert!(config.otlp.is_none());
  }

  #[test]
  fn empty_document_means_defaults() {
    assert_eq!(parse("").unwrap(), ObservabilityConfig::default());
  }

  // -- log level ----------------------------------------------------------

  #[test]
  fn every_level_spelling_parses() {
    for spelling in LogLevel::ALL {
      let raw = format!("[sources.observability]\nlog-level = '{spelling}'\n");
      assert_eq!(parse(&raw).unwrap().log_level.as_str(), *spelling);
    }
  }

  #[test]
  fn unknown_level_names_the_accepted_set() {
    let err = parse("[sources.observability]\nlog-level = 'chatty'\n")
      .expect_err("must reject");
    let message = err.to_string();
    assert!(message.contains("chatty"), "{message}");
    for spelling in LogLevel::ALL {
      assert!(message.contains(spelling), "{message}");
    }
  }

  #[test]
  fn wrongly_typed_level_is_rejected_with_its_line() {
    let err = parse("[sources.observability]\nlog-level = 3\n")
      .expect_err("must reject");
    match err {
      ConfigError::File { line, message, .. } => {
        assert_eq!(line, 2);
        assert!(message.contains("must be a string"), "{message}");
      },
      other => panic!("expected a File error, got {other:?}"),
    }
  }

  #[test]
  fn level_thresholds_order_correctly() {
    assert!(LogLevel::Info.admits(LogLevel::Warn));
    assert!(LogLevel::Info.admits(LogLevel::Info));
    assert!(!LogLevel::Info.admits(LogLevel::Debug));
    assert!(LogLevel::Trace.admits(LogLevel::Trace));
    assert!(!LogLevel::Error.admits(LogLevel::Warn));
  }

  // -- prometheus ---------------------------------------------------------

  #[test]
  fn a_port_configures_a_loopback_exporter() {
    let config =
      parse("[sources.observability.prometheus]\nport = 9321\n").unwrap();
    let exporter = config.prometheus.unwrap();
    assert_eq!(exporter.port, 9321);
    assert_eq!(exporter.bind, DEFAULT_PROMETHEUS_BIND);
  }

  #[test]
  fn an_empty_prometheus_table_configures_nothing() {
    let config = parse("[sources.observability.prometheus]\n").unwrap();
    assert!(config.prometheus.is_none());
  }

  #[test]
  fn privileged_ports_are_refused() {
    let err = parse("[sources.observability.prometheus]\nport = 80\n")
      .expect_err("must reject");
    assert!(err.to_string().contains("1024-65535"), "{err}");
  }

  #[test]
  fn out_of_range_ports_are_refused() {
    let err = parse("[sources.observability.prometheus]\nport = 70000\n")
      .expect_err("must reject");
    assert!(err.to_string().contains("1024-65535"), "{err}");
  }

  #[test]
  fn unknown_prometheus_key_is_a_load_error() {
    let err = parse("[sources.observability.prometheus]\nprot = 9321\n")
      .expect_err("must reject");
    assert!(err.to_string().contains("unknown key 'prot'"), "{err}");
  }

  #[test]
  fn unknown_observability_key_is_a_load_error() {
    let err = parse("[sources.observability]\nlog_level = 'info'\n")
      .expect_err("must reject");
    let message = err.to_string();
    assert!(message.contains("unknown key 'log_level'"), "{message}");
    assert!(message.contains("log-level"), "{message}");
  }

  // -- bind validation ----------------------------------------------------

  #[test]
  fn loopback_binds_are_accepted() {
    assert!(validate_bind("127.0.0.1").is_ok());
    assert!(validate_bind("::1").is_ok());
    assert!(validate_bind("127.0.0.53").is_ok());
  }

  #[test]
  fn non_loopback_bind_is_refused_and_names_the_address() {
    let err = validate_bind("0.0.0.0").expect_err("must reject");
    assert!(err.contains("0.0.0.0"), "{err}");
    assert!(err.contains("non-loopback"), "{err}");
  }

  #[test]
  fn garbage_bind_is_refused() {
    assert!(validate_bind("everywhere").is_err());
  }

  #[test]
  fn a_configured_non_loopback_bind_parses_but_fails_validation() {
    // Parsing accepts any string so the error can name the key and
    // line; the refusal happens where the socket is opened.
    let config = parse(
      "[sources.observability.prometheus]\nport = 9321\nbind = '0.0.0.0'\n",
    )
    .unwrap();
    let exporter = config.prometheus.unwrap();
    assert_eq!(exporter.bind, "0.0.0.0");
    assert!(validate_bind(&exporter.bind).is_err());
  }

  // -- otlp ---------------------------------------------------------------

  #[test]
  fn an_endpoint_configures_otlp_with_the_capped_latency() {
    let config = parse(
      "[sources.observability.otlp]\nendpoint = 'http://collector:4318'\n",
    )
    .unwrap();
    let otlp = config.otlp.unwrap();
    assert_eq!(otlp.endpoint, "http://collector:4318");
    assert_eq!(otlp.batch_max_latency, MAX_OTLP_BATCH_LATENCY);
  }

  #[test]
  fn a_non_http_endpoint_is_refused() {
    let err =
      parse("[sources.observability.otlp]\nendpoint = 'collector:4318'\n")
        .expect_err("must reject");
    assert!(err.to_string().contains("http://"), "{err}");
  }

  #[test]
  fn batch_latency_cannot_exceed_the_spec_cap() {
    let err = parse(
      "[sources.observability.otlp]\nendpoint = 'http://c:4318'\n\
       batch-max-latency = '30s'\n",
    )
    .expect_err("must reject");
    assert!(err.to_string().contains("at most 5s"), "{err}");
  }

  #[test]
  fn batch_latency_accepts_sub_second_values() {
    let config = parse(
      "[sources.observability.otlp]\nendpoint = 'http://c:4318'\n\
       batch-max-latency = '250ms'\n",
    )
    .unwrap();
    assert_eq!(
      config.otlp.unwrap().batch_max_latency,
      Duration::from_millis(250)
    );
  }

  // -- environment --------------------------------------------------------

  #[test]
  fn env_overrides_the_file_level() {
    let config = parse("[sources.observability]\nlog-level = 'warn'\n")
      .unwrap()
      .apply_env_with(&env(&[(LOG_LEVEL_ENV, "debug")]))
      .unwrap();
    assert_eq!(config.log_level, LogLevel::Debug);
  }

  #[test]
  fn blank_env_does_not_override() {
    let config = parse("[sources.observability]\nlog-level = 'warn'\n")
      .unwrap()
      .apply_env_with(&env(&[(LOG_LEVEL_ENV, "  ")]))
      .unwrap();
    assert_eq!(config.log_level, LogLevel::Warn);
  }

  #[test]
  fn env_can_enable_the_exporter() {
    let config = ObservabilityConfig::default()
      .apply_env_with(&env(&[(PROMETHEUS_PORT_ENV, "9321")]))
      .unwrap();
    assert_eq!(config.prometheus.unwrap().port, 9321);
  }

  #[test]
  fn env_bind_without_a_port_is_an_error_not_a_no_op() {
    let err = ObservabilityConfig::default()
      .apply_env_with(&env(&[(PROMETHEUS_BIND_ENV, "0.0.0.0")]))
      .expect_err("must reject");
    match err {
      ConfigError::Env { var, message } => {
        assert_eq!(var, PROMETHEUS_BIND_ENV);
        assert!(message.contains(PROMETHEUS_PORT_ENV), "{message}");
      },
      other => panic!("expected an Env error, got {other:?}"),
    }
  }

  #[test]
  fn env_bind_overrides_a_configured_exporter() {
    let config = parse("[sources.observability.prometheus]\nport = 9321\n")
      .unwrap()
      .apply_env_with(&env(&[(PROMETHEUS_BIND_ENV, "0.0.0.0")]))
      .unwrap();
    assert_eq!(config.prometheus.unwrap().bind, "0.0.0.0");
  }

  #[test]
  fn env_port_outside_the_range_names_the_variable() {
    let err = ObservabilityConfig::default()
      .apply_env_with(&env(&[(PROMETHEUS_PORT_ENV, "80")]))
      .expect_err("must reject");
    assert!(err.to_string().contains(PROMETHEUS_PORT_ENV), "{err}");
  }

  #[test]
  fn env_can_enable_otlp() {
    let config = ObservabilityConfig::default()
      .apply_env_with(&env(&[(OTLP_ENDPOINT_ENV, "https://c:4318")]))
      .unwrap();
    assert_eq!(config.otlp.unwrap().endpoint, "https://c:4318");
  }

  #[test]
  fn env_otlp_endpoint_is_validated() {
    let err = ObservabilityConfig::default()
      .apply_env_with(&env(&[(OTLP_ENDPOINT_ENV, "collector:4318")]))
      .expect_err("must reject");
    assert!(err.to_string().contains(OTLP_ENDPOINT_ENV), "{err}");
  }

  // -- file loading -------------------------------------------------------

  #[test]
  fn a_missing_file_means_defaults() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config =
      ObservabilityConfig::from_toml_file(&tmp.path().join("nope.toml"))
        .unwrap();
    assert_eq!(config, ObservabilityConfig::default());
  }

  #[test]
  fn syntax_errors_carry_their_line() {
    let err = parse("[sources.observability\nlog-level = 'info'\n")
      .expect_err("must reject");
    assert!(matches!(err, ConfigError::File { .. }), "{err:?}");
  }
}
