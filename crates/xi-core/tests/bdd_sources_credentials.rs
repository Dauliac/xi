//! BDD: contract for the `xi-sources-credentials` capability
//! (openspec/changes/xi-sources-credentials/specs/xi-sources-credentials/spec.
//! md).
//!
//! One test per SC-R-* criterion in
//! `openspec/changes/xi-sources-credentials/proposal.md`, named after
//! it, so a failure here names exactly one broken requirement.
//!
//! Nothing here reaches a network or a real sshd. The capability's job
//! is to decide *what* to hand git and *what to say* when it cannot, and
//! both are pure: [`plan`] builds the fallthrough chain as data, and
//! [`verdict`] turns git's stderr into a degraded reason. The two
//! impure edges are an existence check on configured secret paths,
//! which a tempdir covers, and the credential helper's stdio, which is
//! driven through in-memory buffers.
//!
//! The negative claims matter as much as the positive ones here: a token
//! that reaches a log line is a leak, so SC-R-008 is checked against the
//! `Debug` and `--explain` surfaces rather than trusted.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{collections::HashMap, fs, io, path::PathBuf};

use tempfile::TempDir;
use xi_core::sources::{
  Scope,
  credentials::{
    Attempt,
    CredentialsExplain,
    FailureClass,
    Flow,
    GIT_ASKPASS,
    GIT_SSH_COMMAND,
    GIT_TERMINAL_PROMPT,
    Mode,
    Outcome,
    REASON_CREDENTIALS_MISSING,
    REASON_DNS_FAILURE,
    REASON_HOST_KEY_CHANGED,
    REASON_SECRET_NOT_MOUNTED,
    REASON_UNSUPPORTED_SCHEME,
    REDACTED,
    Readiness,
    Request,
    ResolvedAnonymous,
    ResolvedCredentials,
    ResolvedHttps,
    ResolvedSsh,
    Route,
    Secret,
    SecretError,
    StrictHostKeyChecking,
    default_known_hosts,
    effective_policy,
    host_key_fingerprints,
    plan,
    run_credential_helper,
    unblocking_keys,
    verdict,
  },
};

const PUBLIC_SSH: &str = "git@github.com:you/nix-config.git";
const PUBLIC_HTTPS: &str = "https://github.com/you/nix-config.git";
const TOKEN: &str = "ghp_notarealtoken";

/// An environment the known-hosts default resolves against, so no test
/// reads the operator's real `~/.ssh`.
fn env_of(home: &std::path::Path) -> impl Fn(&str) -> Option<String> + use<> {
  let vars: HashMap<String, String> = [
    ("HOME".to_owned(), home.display().to_string()),
    (
      "XDG_DATA_HOME".to_owned(),
      home.join(".local/share").display().to_string(),
    ),
  ]
  .into_iter()
  .collect();
  move |key: &str| vars.get(key).cloned()
}

/// Shorthand for the chain a fetch of `url` would try.
fn chain(
  url: &str,
  credentials: &ResolvedCredentials,
  home: &std::path::Path,
) -> Readiness {
  plan(
    &Request {
      remote_url: url,
      scope:      Scope::User,
      flow:       Flow::Normal,
    },
    credentials,
    &env_of(home),
  )
}

/// The modes of a chain that is expected to be `Ready`.
fn modes(readiness: &Readiness) -> Vec<Mode> {
  match readiness {
    Readiness::Ready { attempts, .. } => {
      attempts.iter().map(|a| a.mode).collect()
    },
    other => panic!("expected a Ready chain, got {other:?}"),
  }
}

fn attempts_of(readiness: &Readiness) -> &[Attempt] {
  match readiness {
    Readiness::Ready { attempts, .. } => attempts,
    other => panic!("expected a Ready chain, got {other:?}"),
  }
}

// ---------------------------------------------------------------------------
// SC-R-001 / SC-R-006: the anonymous end of the chain
// ---------------------------------------------------------------------------

/// SC-R-001: nothing configured and a public remote still has something
/// to try, on both routes.
#[test]
fn sc_r_001_an_unconfigured_public_remote_can_still_be_fetched() {
  let tmp = TempDir::new().expect("tempdir");
  let credentials = ResolvedCredentials::default();

  let ssh = chain(PUBLIC_SSH, &credentials, tmp.path());
  assert_eq!(
    modes(&ssh),
    [Mode::SshAgent, Mode::SshAnonymous],
    "with no identity file the chain is agent first, then anonymous"
  );

  let https = chain(PUBLIC_HTTPS, &credentials, tmp.path());
  assert_eq!(
    modes(&https),
    [Mode::HttpsAnonymous],
    "an unconfigured https remote tries an unauthenticated fetch"
  );

  // Every attempt turns a credential prompt into an error, or an
  // unattended daemon tick would hang on a tty it does not have.
  for readiness in [&ssh, &https] {
    for attempt in attempts_of(readiness) {
      let prompt = attempt
        .env
        .iter()
        .find(|(key, _)| key == GIT_TERMINAL_PROMPT);
      assert_eq!(
        prompt.map(|(_, value)| value.as_str()),
        Some("0"),
        "{:?} must disable the terminal prompt",
        attempt.mode
      );
    }
  }
}

/// SC-R-006: `anonymous.allow = false` removes the anonymous end of the
/// chain, so a public remote is refused rather than silently fetched.
#[test]
fn sc_r_006_refusing_anonymous_leaves_a_public_remote_unfetchable() {
  let tmp = TempDir::new().expect("tempdir");
  let credentials = ResolvedCredentials {
    anonymous: ResolvedAnonymous { allow: false },
    ssh: ResolvedSsh {
      use_agent: false,
      ..ResolvedSsh::default()
    },
    ..ResolvedCredentials::default()
  };

  // No identity, no agent, no anonymous: nothing is left to try, and
  // that is reported without spawning git.
  let Readiness::Degraded { reason } =
    chain(PUBLIC_SSH, &credentials, tmp.path())
  else {
    panic!("with every ssh mode disabled the chain must be Degraded");
  };
  assert!(
    reason.starts_with(REASON_CREDENTIALS_MISSING),
    "the reason must carry the wire spelling: {reason}"
  );
  assert!(
    reason.contains("identity-file"),
    "the reason must name a key that would unblock it: {reason}"
  );

  let Readiness::Degraded { reason } =
    chain(PUBLIC_HTTPS, &credentials, tmp.path())
  else {
    panic!("https with no token and no anonymous must be Degraded");
  };
  assert!(reason.contains("token-file"), "got: {reason}");
}

// ---------------------------------------------------------------------------
// SC-R-003: the ssh identity file
// ---------------------------------------------------------------------------

/// SC-R-003: a configured identity is handed to git through
/// `GIT_SSH_COMMAND`, with `IdentitiesOnly=yes` so a loaded agent
/// cannot answer in its place, and nothing touches `~/.ssh/config`.
#[test]
fn sc_r_003_an_identity_file_is_passed_through_git_ssh_command() {
  let tmp = TempDir::new().expect("tempdir");
  let key = tmp.path().join("deploy_key");
  fs::write(&key, b"-----BEGIN OPENSSH PRIVATE KEY-----\n").expect("write key");

  let credentials = ResolvedCredentials {
    ssh: ResolvedSsh {
      identity_file: Some(key.clone()),
      ..ResolvedSsh::default()
    },
    ..ResolvedCredentials::default()
  };

  let readiness = chain(PUBLIC_SSH, &credentials, tmp.path());
  assert_eq!(
    modes(&readiness).first().copied(),
    Some(Mode::SshIdentityFile),
    "an explicit identity is tried before the agent"
  );

  let first = &attempts_of(&readiness)[0];
  let (_, command) = first
    .env
    .iter()
    .find(|(key, _)| key == GIT_SSH_COMMAND)
    .expect("the identity mode must set GIT_SSH_COMMAND");

  assert!(
    command.contains(&key.display().to_string()),
    "the identity path must be in the command: {command}"
  );
  assert!(
    command.contains("IdentitiesOnly=yes"),
    "without IdentitiesOnly a loaded agent answers instead: {command}"
  );
  assert!(
    command.contains("StrictHostKeyChecking=yes"),
    "the host-key policy must be pinned on the command: {command}"
  );
  assert!(
    command.contains("UserKnownHostsFile="),
    "the known-hosts file must be pinned too: {command}"
  );
  assert!(
    !command.contains("ssh.config") && !command.contains("-F"),
    "xi must not point ssh at a config file of its own: {command}"
  );
}

// ---------------------------------------------------------------------------
// SC-R-004 / SC-R-008: the https token
// ---------------------------------------------------------------------------

/// SC-R-004: a token file is reached through a one-shot
/// `credential.helper`, and the token itself never appears in the
/// wiring xi hands to git.
#[test]
fn sc_r_004_a_token_file_is_read_by_a_helper_and_never_inlined() {
  let tmp = TempDir::new().expect("tempdir");
  let token_file = tmp.path().join("token");
  fs::write(&token_file, format!("{TOKEN}\n")).expect("write token");

  let credentials = ResolvedCredentials {
    https: ResolvedHttps {
      token_file: Some(token_file.clone()),
      ..ResolvedHttps::default()
    },
    ..ResolvedCredentials::default()
  };

  let readiness = chain(PUBLIC_HTTPS, &credentials, tmp.path());
  assert_eq!(
    modes(&readiness).first().copied(),
    Some(Mode::HttpsTokenFile)
  );

  let first = &attempts_of(&readiness)[0];
  let wiring = format!("{:?} {:?}", first.env, first.config);
  assert!(
    wiring.contains("credential.helper"),
    "the token mode must install a credential helper: {wiring}"
  );
  assert!(
    !wiring.contains(TOKEN),
    "the token must never be inlined into the git wiring: {wiring}"
  );

  // The helper is the only thing that reads the file, and it writes the
  // token exactly once, to git's stdin protocol.
  let mut out = Vec::new();
  run_credential_helper(
    "get",
    &token_file,
    "git",
    &mut io::Cursor::new(b"protocol=https\nhost=github.com\n\n".to_vec()),
    &mut out,
  )
  .expect("the helper must answer a get");
  let reply = String::from_utf8(out).expect("utf8 reply");
  assert_eq!(reply, format!("username=git\npassword={TOKEN}\n"));

  // A trailing newline in the file would truncate git's reply at the
  // wrong place, so it has to be trimmed on the way in.
  assert!(
    !reply.contains(&format!("{TOKEN}\n\n")),
    "the token's own trailing newline must be stripped: {reply:?}"
  );
}

/// SC-R-004, non-`get` half: git also invokes a helper to `store` and
/// `erase`, and neither must emit the token.
#[test]
fn sc_r_004_the_helper_answers_only_the_read_operations() {
  let tmp = TempDir::new().expect("tempdir");
  let token_file = tmp.path().join("token");
  fs::write(&token_file, format!("{TOKEN}\n")).expect("write token");

  for operation in ["store", "erase", "approve", "reject"] {
    let mut out = Vec::new();
    run_credential_helper(
      operation,
      &token_file,
      "git",
      &mut io::Cursor::new(b"protocol=https\nhost=github.com\n\n".to_vec()),
      &mut out,
    )
    .expect("the helper must not fail on a write operation");
    assert!(
      out.is_empty(),
      "`{operation}` must emit nothing, got {:?}",
      String::from_utf8_lossy(&out)
    );
  }
}

/// SC-R-008: no surface that an operator or a log can see carries the
/// secret's value.
#[test]
fn sc_r_008_no_reported_surface_carries_the_secret_value() {
  let tmp = TempDir::new().expect("tempdir");
  let token_file = tmp.path().join("token");
  fs::write(&token_file, format!("{TOKEN}\n")).expect("write token");

  // The type itself: `Debug` is what a careless `{:?}` on a config
  // struct would reach, so it must not be the value.
  let secret = Secret::from_file(&token_file).expect("read the token");
  let debug = format!("{secret:?}");
  assert_eq!(debug, REDACTED);
  assert!(!debug.contains(TOKEN));
  assert_eq!(
    secret.len(),
    TOKEN.len(),
    "the length is safe to log and is what tells an operator about a stray \
     newline"
  );
  secret.with_secret(|value| {
    assert_eq!(value, TOKEN, "the one accessor still yields the value");
  });

  // `Serialize` does not merely omit the value: it refuses, so a type
  // that ends up inside an envelope is a runtime error rather than a
  // silent leak.
  let serialised = serde_json::to_string(&secret);
  assert!(
    serialised.is_err(),
    "serialising a secret must fail, got {serialised:?}"
  );

  // The `--explain` surface: the mode's name and the path, never the
  // contents of the path.
  let credentials = ResolvedCredentials {
    https: ResolvedHttps {
      token_file: Some(token_file.clone()),
      ..ResolvedHttps::default()
    },
    ..ResolvedCredentials::default()
  };
  let explain = CredentialsExplain::describe(
    PUBLIC_HTTPS,
    &credentials,
    Scope::User,
    &env_of(tmp.path()),
  );
  assert_eq!(explain.route, Route::Https);
  assert_eq!(explain.modes[0].mode, Mode::HttpsTokenFile);
  assert_eq!(explain.modes[0].source.as_ref(), Some(&token_file));

  let rendered = format!("{explain:?}");
  assert!(
    !rendered.contains(TOKEN),
    "--explain must carry the path reference, not the secret: {rendered}"
  );
  let as_json =
    serde_json::to_string(&explain).expect("the explain block serialises");
  assert!(
    !as_json.contains(TOKEN),
    "the envelope must not carry the secret: {as_json}"
  );
}

// ---------------------------------------------------------------------------
// SC-R-005: trust on first use, and only on first use
// ---------------------------------------------------------------------------

/// SC-R-005: `accept-new` is honoured during bootstrap and downgraded
/// to `yes` for every later fetch, which is what keeps
/// trust-on-first-use from becoming a standing man-in-the-middle
/// window.
#[test]
fn sc_r_005_accept_new_is_honoured_only_during_bootstrap() {
  assert_eq!(
    effective_policy(StrictHostKeyChecking::AcceptNew, Flow::Bootstrap),
    StrictHostKeyChecking::AcceptNew,
    "a first clone has no known-hosts entry to compare against"
  );
  assert_eq!(
    effective_policy(StrictHostKeyChecking::AcceptNew, Flow::Normal),
    StrictHostKeyChecking::Yes,
    "a daemon tick accepting a new key is a silent key swap"
  );

  // The other two policies are the operator's to choose and are passed
  // through in both flows.
  for configured in [StrictHostKeyChecking::Yes, StrictHostKeyChecking::No] {
    for flow in [Flow::Bootstrap, Flow::Normal] {
      assert_eq!(effective_policy(configured, flow), configured);
    }
  }
}

/// SC-R-005, refusal half: a changed host key is classified as such,
/// not as a credential problem, so it is reported instead of being
/// retried with the next mode.
#[test]
fn sc_r_005_a_changed_host_key_is_not_a_credential_failure() {
  let changed = "@@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! \
                 @@@@\nHost key for github.com has changed";
  assert_eq!(classify(changed), FailureClass::HostKeyChanged);
  assert!(
    !classify(changed).is_credential(),
    "a key swap must not be retried as if a credential were missing"
  );

  // And it wins the verdict even when a credential mode also failed.
  let outcomes = vec![
    Outcome::from_stderr(
      Mode::SshIdentityFile,
      "permission denied (publickey)",
    ),
    Outcome::from_stderr(Mode::SshAgent, changed),
  ];
  let reason = verdict(PUBLIC_SSH, Route::Ssh, &outcomes);
  assert!(
    reason.starts_with(REASON_HOST_KEY_CHANGED),
    "the first non-credential failure wins: {reason}"
  );
}

fn classify(stderr: &str) -> FailureClass {
  xi_core::sources::credentials::classify_failure(stderr)
}

/// SC-R-005, fingerprint half: the key accepted on first use is
/// readable back out, so bootstrap can log what it trusted, and a host
/// that later presents a second different key is visibly two-keyed
/// rather than silently re-trusted.
#[test]
fn sc_r_005_the_accepted_host_key_is_readable_back_for_logging() {
  let tmp = TempDir::new().expect("tempdir");
  let known_hosts = tmp.path().join("known_hosts");

  // A missing file is "no keys known", which is the normal pre-TOFU
  // state rather than an error.
  assert_eq!(
    host_key_fingerprints(&known_hosts, "github.com").expect("read"),
    Vec::<String>::new()
  );

  // What ssh writes after accepting a key on first use.
  let first =
    "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
  fs::write(&known_hosts, format!("github.com ssh-ed25519 {first}\n"))
    .expect("write known_hosts");

  let after_tofu =
    host_key_fingerprints(&known_hosts, "github.com").expect("read");
  assert_eq!(after_tofu.len(), 1, "one accepted key, one fingerprint");
  assert!(
    after_tofu[0].starts_with("SHA256:"),
    "the fingerprint must be in ssh-keygen's spelling so an operator can \
     compare it by eye: {}",
    after_tofu[0]
  );

  // A second, different key for the same host is a second fingerprint,
  // which is what makes a silent key swap detectable rather than a
  // matter of trusting ssh's own comparison.
  let second =
    "AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  fs::write(
    &known_hosts,
    format!(
      "github.com ssh-ed25519 {first}\ngithub.com ssh-ed25519 {second}\n"
    ),
  )
  .expect("rewrite known_hosts");

  let both = host_key_fingerprints(&known_hosts, "github.com").expect("read");
  assert_eq!(both.len(), 2, "both keys must be visible: {both:?}");
  assert_ne!(both[0], both[1], "two keys, two distinct fingerprints");
  assert_eq!(both[0], after_tofu[0], "the first key is unchanged");

  // Another host's entry is not this host's business.
  assert_eq!(
    host_key_fingerprints(&known_hosts, "gitlab.com").expect("read"),
    Vec::<String>::new()
  );
}

/// Task 4.2: the known-hosts default is per scope, and the user-scope
/// one is resolved from the environment rather than from the real
/// `~/.ssh`.
#[test]
fn the_known_hosts_default_follows_the_scope() {
  let tmp = TempDir::new().expect("tempdir");
  let lookup = env_of(tmp.path());

  assert_eq!(
    default_known_hosts(Scope::User, &lookup),
    tmp.path().join(".ssh/known_hosts")
  );
  assert_eq!(
    default_known_hosts(Scope::System, &lookup),
    PathBuf::from("/etc/ssh/ssh_known_hosts"),
    "a system-scope fetch has no operator home to read"
  );

  // No HOME at all falls back to the system file rather than to a
  // relative path that would land wherever the daemon happened to run.
  let empty = |_: &str| None;
  assert_eq!(
    default_known_hosts(Scope::User, &empty),
    PathBuf::from("/etc/ssh/ssh_known_hosts")
  );
}

// ---------------------------------------------------------------------------
// SC-R-002: what the daemon reports when nothing works
// ---------------------------------------------------------------------------

/// SC-R-002: every mode failing on credentials reports
/// `credentials-missing`, naming the remote and the keys that would
/// unblock it, with no busy-loop implied.
#[test]
fn sc_r_002_a_private_remote_with_no_credentials_names_what_would_unblock_it() {
  let outcomes = vec![
    Outcome::from_stderr(Mode::SshAgent, "permission denied (publickey)"),
    Outcome::from_stderr(Mode::SshAnonymous, "permission denied (publickey)"),
  ];
  let reason = verdict(PUBLIC_SSH, Route::Ssh, &outcomes);

  assert!(
    reason.starts_with(REASON_CREDENTIALS_MISSING),
    "got: {reason}"
  );
  assert!(
    reason.contains(PUBLIC_SSH),
    "the reason must name the remote: {reason}"
  );
  for key in unblocking_keys(Route::Ssh) {
    assert!(
      reason.contains(key),
      "the reason must name `{key}`: {reason}"
    );
  }
  for mode in [Mode::SshAgent, Mode::SshAnonymous] {
    assert!(
      reason.contains(mode.as_str()),
      "the reason must say what was tried: {reason}"
    );
  }
}

/// SC-R-002, hard-failure half: a non-credential failure is reported as
/// itself, so an operator is not sent after a credential they already
/// have.
#[test]
fn sc_r_002_a_dns_failure_is_not_reported_as_a_missing_credential() {
  let outcomes = vec![Outcome::from_stderr(
    Mode::HttpsAnonymous,
    "fatal: unable to access: Could not resolve host: github.com",
  )];
  let reason = verdict(PUBLIC_HTTPS, Route::Https, &outcomes);
  assert!(reason.starts_with(REASON_DNS_FAILURE), "got: {reason}");
  assert!(
    !reason.contains(REASON_CREDENTIALS_MISSING),
    "a name-resolution failure is not a credential problem: {reason}"
  );
}

/// An unsupported scheme is refused before any credential is built, so
/// a token is never sent over plaintext.
#[test]
fn an_unsupported_scheme_is_refused_before_any_credential_is_built() {
  let tmp = TempDir::new().expect("tempdir");
  let credentials = ResolvedCredentials::default();

  for url in [
    "http://github.com/you/cfg.git",
    "git://github.com/you/cfg.git",
  ] {
    let Readiness::Degraded { reason } = chain(url, &credentials, tmp.path())
    else {
      panic!("{url} must not be routable");
    };
    assert!(
      reason.starts_with(REASON_UNSUPPORTED_SCHEME),
      "got: {reason}"
    );
    assert!(
      reason.contains(url),
      "the reason must name the url: {reason}"
    );
  }

  // A local path is routable and needs no credentials at all.
  let local = chain("/srv/nix-config", &credentials, tmp.path());
  assert_eq!(modes(&local), [Mode::Local]);
}

// ---------------------------------------------------------------------------
// SC-R-007: secrets that are not mounted yet
// ---------------------------------------------------------------------------

/// SC-R-007: a token file a secret manager has not mounted yet is
/// `Pending` and names the path, not `Degraded`: it will appear, and
/// reporting a missing credential would send the operator after the
/// wrong problem.
#[test]
fn sc_r_007_an_unmounted_secret_is_pending_and_names_the_mount_path() {
  let tmp = TempDir::new().expect("tempdir");
  let not_yet = PathBuf::from("/run/secrets/xi-sources-token");

  let credentials = ResolvedCredentials {
    https: ResolvedHttps {
      token_file: Some(not_yet.clone()),
      ..ResolvedHttps::default()
    },
    ..ResolvedCredentials::default()
  };

  let Readiness::Pending { reason } =
    chain(PUBLIC_HTTPS, &credentials, tmp.path())
  else {
    panic!("a configured-but-absent secret must be Pending");
  };
  assert!(
    reason.starts_with(REASON_SECRET_NOT_MOUNTED),
    "the reason must carry the wire spelling: {reason}"
  );
  assert!(
    reason.contains(&not_yet.display().to_string()),
    "the reason must name the mount path: {reason}"
  );

  // Once mounted, the same configuration reads normally.
  let mounted = tmp.path().join("token");
  fs::write(&mounted, format!("{TOKEN}\n")).expect("write token");
  let credentials = ResolvedCredentials {
    https: ResolvedHttps {
      token_file: Some(mounted.clone()),
      ..ResolvedHttps::default()
    },
    ..ResolvedCredentials::default()
  };
  assert_eq!(
    modes(&chain(PUBLIC_HTTPS, &credentials, tmp.path())),
    [Mode::HttpsTokenFile, Mode::HttpsAnonymous],
    "the token is tried first, with the SC-R-001 anonymous fallthrough still \
     behind it"
  );
  Secret::from_file(&mounted).expect("a mounted secret reads");
}

/// SC-R-007, degenerate half: a mounted but empty secret is an error
/// rather than an empty password, which would otherwise reach the
/// remote as a failed authentication the operator cannot explain.
#[test]
fn sc_r_007_an_empty_secret_file_is_rejected_rather_than_sent() {
  let tmp = TempDir::new().expect("tempdir");

  let empty = tmp.path().join("empty");
  fs::write(&empty, b"   \n\n").expect("write empty");
  assert!(matches!(
    Secret::from_file(&empty),
    Err(SecretError::Empty { .. })
  ));

  let missing = tmp.path().join("missing");
  assert!(matches!(
    Secret::from_file(&missing),
    Err(SecretError::NotMounted { .. })
  ));
}

// ---------------------------------------------------------------------------
// The oauth route
// ---------------------------------------------------------------------------

/// A configured askpass moves an https remote onto the oauth route, and
/// is handed over through `GIT_ASKPASS`.
#[test]
fn a_configured_askpass_routes_https_to_oauth() {
  let tmp = TempDir::new().expect("tempdir");
  let askpass = tmp.path().join("askpass");
  fs::write(&askpass, b"#!/bin/sh\necho token\n").expect("write askpass");

  let credentials = ResolvedCredentials {
    oauth: xi_core::sources::credentials::ResolvedOauth {
      askpass: Some(askpass.clone()),
    },
    ..ResolvedCredentials::default()
  };

  let readiness = chain(PUBLIC_HTTPS, &credentials, tmp.path());
  assert_eq!(modes(&readiness), [Mode::OauthAskpass]);

  let (_, value) = attempts_of(&readiness)[0]
    .env
    .iter()
    .find(|(key, _)| key == GIT_ASKPASS)
    .expect("the oauth mode must set GIT_ASKPASS");
  assert_eq!(value, &askpass.display().to_string());
}
