//! The redaction boundary every observable surface sits behind.
//!
//! Two layers, because one is not enough:
//!
//! 1. [`xi_core::sources::credentials::Secret`] has no `Serialize` impl and
//!    prints `<redacted>` in `Debug`. A field that tries to log one does not
//!    compile, which is the only guarantee that survives a refactor. That type
//!    lives in `xi-core` ([[xi-sources-credentials]] owns *which* credentials
//!    exist and where they come from); earlier revisions of this module defined
//!    a second, weaker newtype of its own (`Credential`, `String`-backed, no
//!    `with_secret` closure) for the same job. It is gone:
//!    `xi-sources-observability` already depends on `xi-core`, so there is no
//!    cycle in depending on `Secret` directly, and two opaque-credential types
//!    is how a future leak gets written against the weaker one instead of the
//!    one that is actually enforced.
//! 2. [`Scrubber`] handles the text a credential *leaks into* rather than the
//!    credential itself: a git error that echoes a URL with userinfo, a hook's
//!    stderr tail. Compile-time typing cannot reach that text, so it is
//!    filtered on the way to the sink.
//!
//! [`lint`] cuts across both: it scans the tree for log and metric calls
//! that name a secret-shaped key, catching the case where someone reads a
//! [`xi_core::sources::credentials::Secret`] through `with_secret` and then
//! logs the resulting `&str` directly, which defeats layer 1 without
//! defeating the type system.

use xi_core::sources::credentials::Secret;

/// The placeholder every surface prints in place of a secret.
pub const REDACTED: &str = "<redacted>";

/// Replaces registered secret values with [`REDACTED`] in free text.
///
/// Registration is explicit: the scrubber cannot guess what a secret
/// looks like, and a heuristic that tried would both miss real tokens
/// and mangle innocent text. Short values are refused, because scrubbing
/// a two-character secret would redact most of the log.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
  secrets: Vec<String>,
}

/// Shortest secret the scrubber will act on.
///
/// Below this a value appears too often in ordinary text for
/// substitution to leave a readable log; such a value is not a usable
/// credential either.
pub const MIN_SCRUBBABLE_LEN: usize = 6;

impl Scrubber {
  /// An empty scrubber. Passes text through unchanged.
  #[must_use]
  pub const fn new() -> Self {
    Self {
      secrets: Vec::new(),
    }
  }

  /// Register a credential whose value must never reach a sink.
  ///
  /// Values shorter than [`MIN_SCRUBBABLE_LEN`] are ignored rather than
  /// rejected: the caller is wiring up config, and a too-short token is
  /// a config problem for `xi-sources-credentials` to report, not a
  /// reason for the logger to refuse to start.
  ///
  /// Takes the secret through [`Secret::with_secret`] rather than
  /// exposing it to the caller first, so registering a credential never
  /// requires a call site to hold the bare value outside this function.
  pub fn register(&mut self, credential: &Secret) {
    credential.with_secret(|secret| {
      if secret.len() >= MIN_SCRUBBABLE_LEN
        && !self.secrets.iter().any(|s| s == secret)
      {
        self.secrets.push(secret.to_owned());
      }
    });
  }

  /// How many secrets are registered.
  #[must_use]
  pub const fn len(&self) -> usize {
    self.secrets.len()
  }

  /// Whether no secret is registered.
  #[must_use]
  pub const fn is_empty(&self) -> bool {
    self.secrets.is_empty()
  }

  /// Replace every registered secret in `text` with [`REDACTED`].
  ///
  /// Longest secret first, so a token that contains another token does
  /// not leave a fragment behind.
  #[must_use]
  pub fn scrub(&self, text: &str) -> String {
    if self.secrets.is_empty() {
      return text.to_owned();
    }
    let mut ordered: Vec<&String> = self.secrets.iter().collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));

    let mut out = text.to_owned();
    for secret in ordered {
      if out.contains(secret.as_str()) {
        out = out.replace(secret.as_str(), REDACTED);
      }
    }
    out
  }
}

pub mod lint {
  //! A source lint for log and metric calls that name a secret.
  //!
  //! [`Secret`](xi_core::sources::credentials::Secret) stops the typed path.
  //! This stops the untyped one: `info!(token = %cfg.token.expose())`
  //! type-checks and would leak, so the key names themselves are banned.
  //!
  //! It runs as a test rather than as a `rustc` lint because the
  //! alternative is a custom clippy driver, and a driver that only CI
  //! has is a lint nobody runs locally. Wire it with
  //! [`scan_workspace`] over `crates/`; the scan walks whatever crates
  //! exist, so the gitops daemon is covered the day it lands without
  //! this list being updated.

  use std::{fs, io, path::Path};

  /// Field and label names that must never appear on a log or metric
  /// call.
  ///
  /// Spelled as whole identifiers; `token_path` and `signature_kind`
  /// are fine, because naming *where* a secret lives is not naming the
  /// secret.
  pub const BANNED_KEYS: &[&str] = &[
    "token",
    "password",
    "passphrase",
    "secret",
    "credential",
    "private_key",
    "ssh_key",
    "gpg_signature",
    "signature_body",
    "auth_header",
  ];

  /// Macros and methods whose arguments reach an observable surface.
  const OBSERVABLE_CALLS: &[&str] = &[
    "trace!",
    "debug!",
    "info!",
    "warn!",
    "error!",
    "event!",
    "trace_span!",
    "debug_span!",
    "info_span!",
    "warn_span!",
    "error_span!",
    "span!",
    ".set(",
    ".inc(",
    ".observe(",
    ".with_label_values(",
  ];

  /// One rejected call site.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Violation {
    /// Path of the offending file, as walked.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// The banned key that triggered the rejection.
    pub key:  String,
    /// The source line, trimmed.
    pub text: String,
  }

  impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(
        f,
        "{}:{}: observable call names `{}`: {}",
        self.file, self.line, self.key, self.text
      )
    }
  }

  /// Scan every `.rs` file under `root` for banned keys on observable
  /// calls.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from walking or reading the tree.
  pub fn scan_workspace(root: &Path) -> io::Result<Vec<Violation>> {
    let mut found = Vec::new();
    scan_dir(root, &mut found)?;
    found.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(found)
  }

  fn scan_dir(dir: &Path, found: &mut Vec<Violation>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
      let path = entry?.path();
      let name = path.file_name().unwrap_or_default().to_string_lossy();
      if path.is_dir() {
        // `target` holds generated code, not ours.
        if name != "target" {
          scan_dir(&path, found)?;
        }
      } else if path.extension().is_some_and(|ext| ext == "rs") {
        scan_file(&path, found)?;
      }
    }
    Ok(())
  }

  fn scan_file(path: &Path, found: &mut Vec<Violation>) -> io::Result<()> {
    let raw = fs::read_to_string(path)?;
    // This module names every banned key in its own source. Scanning it
    // would report the lint's own definition as a violation.
    if raw.contains("BANNED_KEYS: &[&str]") {
      return Ok(());
    }

    for (index, line) in raw.lines().enumerate() {
      let text = line.trim();
      if text.starts_with("//") || !is_observable_call(text) {
        continue;
      }
      if let Some(key) = banned_key_in(text) {
        found.push(Violation {
          file: path.display().to_string(),
          line: index + 1,
          key:  key.to_owned(),
          text: text.to_owned(),
        });
      }
    }
    Ok(())
  }

  fn is_observable_call(text: &str) -> bool {
    OBSERVABLE_CALLS.iter().any(|call| text.contains(call))
  }

  /// The first banned key used as a field name on `text`, if any.
  ///
  /// Matching is on `key =` / `key:` / `"key"` so that a *mention* of
  /// the word in a message string ("token refresh failed") is not a
  /// violation while a field assignment is.
  fn banned_key_in(text: &str) -> Option<&'static str> {
    BANNED_KEYS.iter().copied().find(|key| {
      [format!("{key} ="), format!("{key}="), format!("\"{key}\"")]
        .iter()
        .any(|pattern| text.contains(pattern.as_str()))
    })
  }
}

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use super::*;

  // `Secret`'s own redaction guarantees (`Debug` prints [`REDACTED`],
  // `with_secret` is the only way out) are xi-core's to test; see
  // `crates/xi-core/src/sources/credentials.rs`. These tests cover what
  // this module adds on top: scrubbing free text that a `Secret`'s value
  // leaked into.

  #[test]
  fn scrubber_replaces_registered_secrets() {
    let credential = Secret::new("ghp_supersecretvalue".to_owned());
    let mut scrubber = Scrubber::new();
    scrubber.register(&credential);

    let text = "remote: fatal: bad auth for ghp_supersecretvalue@host";
    let scrubbed = scrubber.scrub(text);
    assert!(!scrubbed.contains("ghp_supersecretvalue"));
    assert!(scrubbed.contains(REDACTED));
    assert!(scrubbed.contains("remote: fatal: bad auth for"));
  }

  #[test]
  fn scrubber_prefers_the_longer_secret() {
    let mut scrubber = Scrubber::new();
    scrubber.register(&Secret::new("abcdef".to_owned()));
    scrubber.register(&Secret::new("abcdefghij".to_owned()));

    let scrubbed = scrubber.scrub("value=abcdefghij end");
    assert_eq!(scrubbed, format!("value={REDACTED} end"));
  }

  #[test]
  fn scrubber_ignores_values_too_short_to_be_secrets() {
    let mut scrubber = Scrubber::new();
    scrubber.register(&Secret::new("ok".to_owned()));
    assert!(scrubber.is_empty());
    assert_eq!(scrubber.scrub("ok then"), "ok then");
  }

  #[test]
  fn scrubber_registers_a_value_once() {
    let mut scrubber = Scrubber::new();
    scrubber.register(&Secret::new("abcdef".to_owned()));
    scrubber.register(&Secret::new("abcdef".to_owned()));
    assert_eq!(scrubber.len(), 1);
  }

  #[test]
  fn empty_scrubber_passes_text_through() {
    assert_eq!(Scrubber::new().scrub("untouched"), "untouched");
  }

  // -- lint ---------------------------------------------------------------

  #[test]
  fn lint_rejects_a_token_field_on_a_log_call() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("leak.rs");
    std::fs::write(&file, "fn f() {\n  info!(token = %t, \"fetching\");\n}\n")
      .unwrap();

    let found = lint::scan_workspace(tmp.path()).unwrap();
    assert_eq!(found.len(), 1, "got: {found:?}");
    assert_eq!(found[0].key, "token");
    assert_eq!(found[0].line, 2);
  }

  #[test]
  fn lint_rejects_a_secret_metric_label() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
      tmp.path().join("metric.rs"),
      "fn f() {\n  gauge.with_label_values(&[\"secret\"]).set(1.0);\n}\n",
    )
    .unwrap();

    let found = lint::scan_workspace(tmp.path()).unwrap();
    assert_eq!(found.len(), 1, "got: {found:?}");
    assert_eq!(found[0].key, "secret");
  }

  #[test]
  fn lint_allows_naming_where_a_secret_lives() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
      tmp.path().join("ok.rs"),
      "fn f() {\n  info!(token_path = %p, \"using askpass\");\n  \
       info!(credential_mode = %m, \"resolved\");\n}\n",
    )
    .unwrap();

    assert!(lint::scan_workspace(tmp.path()).unwrap().is_empty());
  }

  #[test]
  fn lint_allows_the_word_inside_a_message() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
      tmp.path().join("msg.rs"),
      "fn f() {\n  warn!(\"token refresh failed, retrying\");\n}\n",
    )
    .unwrap();

    assert!(lint::scan_workspace(tmp.path()).unwrap().is_empty());
  }

  #[test]
  fn lint_skips_commented_out_code() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
      tmp.path().join("commented.rs"),
      "fn f() {\n  // info!(token = %t);\n}\n",
    )
    .unwrap();

    assert!(lint::scan_workspace(tmp.path()).unwrap().is_empty());
  }

  #[test]
  fn lint_ignores_lines_that_are_not_observable_calls() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
      tmp.path().join("plain.rs"),
      "fn f(token: &str) -> usize {\n  let secret = token;\n  \
       secret.len()\n}\n",
    )
    .unwrap();

    assert!(lint::scan_workspace(tmp.path()).unwrap().is_empty());
  }
}
