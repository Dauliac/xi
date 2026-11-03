//! BDD: a credential never reaches an observable surface.
//!
//! `xi-sources-observability` task 7.1 asks for two guarantees:
//!
//! 1. [`Credential`] refuses `Serialize`, so no serde-backed sink can encode
//!    one. That half is a compile-time fact, already proven by
//!    `redact::tests::debug_never_prints_the_secret` inside the crate - a
//!    credential's `Debug` impl prints `<redacted>` because there is no
//!    other impl for a log call's `?` formatter to reach.
//! 2. The *pipeline* a daemon actually runs - the real `tracing_subscriber`
//!    stack from [`subscriber`], not a bare `format!("{credential:?}")` -
//!    carries that guarantee through unbroken, and free text that leaks a
//!    secret's bytes some other way (a git error echoing a URL with
//!    userinfo) is scrubbed before it reaches the sink.
//!
//! This file proves (2): it is the "through the real pipeline" counterpart
//! to `redact`'s unit tests, which prove the type alone.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use tracing::info;
use xi_sources_observability::{
  LogLevel,
  redact::{Credential, Scrubber},
  subscriber::{capture::CaptureWriter, init_with_writer},
};

const SECRET: &str = "ghp_integrationtestsecretvalue";

/// GIVEN a configured credential
/// WHEN code logs it with `?` (the only formatter `Credential` offers)
/// THEN the real ndjson pipeline's output contains `<redacted>`, never the
/// secret.
#[test]
fn a_credential_logged_through_the_real_pipeline_is_redacted() {
  let writer = CaptureWriter::new();
  let _guard = init_with_writer(LogLevel::Info, writer.clone());

  let credential = Credential::new(SECRET.to_owned());
  info!(cred = ?credential, "fetching with configured credentials");

  let lines = writer.json_lines().expect("captured output must be ndjson");
  assert_eq!(lines.len(), 1, "got: {lines:?}");
  let rendered = lines[0].to_string();
  assert!(!rendered.contains(SECRET), "secret leaked into: {rendered}");
  assert!(rendered.contains("<redacted>"), "got: {rendered}");
}

/// GIVEN a credential registered with a [`Scrubber`]
/// WHEN third-party text that happens to carry the secret's bytes (a git
/// error echoing a URL with userinfo) is scrubbed before being logged
/// THEN the real pipeline's output contains the scrubbed text, never the
/// secret.
#[test]
fn free_text_carrying_a_secret_is_scrubbed_before_it_reaches_the_pipeline() {
  let writer = CaptureWriter::new();
  let _guard = init_with_writer(LogLevel::Info, writer.clone());

  let credential = Credential::new(SECRET.to_owned());
  let mut scrubber = Scrubber::new();
  scrubber.register(&credential);

  let git_error =
    format!("remote: fatal: bad credentials for https://{SECRET}@host/repo.git");
  let scrubbed = scrubber.scrub(&git_error);
  info!(detail = %scrubbed, "fetch failed");

  let lines = writer.json_lines().expect("captured output must be ndjson");
  assert_eq!(lines.len(), 1, "got: {lines:?}");
  let rendered = lines[0].to_string();
  assert!(!rendered.contains(SECRET), "secret leaked into: {rendered}");
  assert!(rendered.contains("<redacted>"), "got: {rendered}");
}

/// GIVEN the real pipeline, a span carrying a credential-shaped field name
/// but a type that is not a credential at all (an ordinary string the
/// caller already decided is safe to log, such as a token's file path)
/// WHEN it is logged
/// THEN it passes through unredacted - this is the boundary
/// `redact::lint` enforces statically (task 7.2, not this bead): naming
/// *where* a secret lives is not naming the secret, so this module does
/// not and should not scrub it at runtime.
#[test]
fn a_non_secret_field_is_not_touched_by_the_pipeline() {
  let writer = CaptureWriter::new();
  let _guard = init_with_writer(LogLevel::Info, writer.clone());

  info!(token_path = "/run/xi/token", "using askpass");

  let lines = writer.json_lines().expect("captured output must be ndjson");
  assert_eq!(lines.len(), 1, "got: {lines:?}");
  assert_eq!(lines[0]["token_path"], "/run/xi/token");
}
