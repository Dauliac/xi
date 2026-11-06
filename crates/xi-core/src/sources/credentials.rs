//! The `xi-sources-credentials` capability: how every git subprocess xi
//! spawns gets its credentials, and how it fails when it cannot.
//!
//! [`super::config`] owns the `[sources]` surface; this module owns the
//! `[sources.credentials.<kind>]` sub-tables, the fallthrough chain that
//! turns them into concrete git invocations, and the opaque [`Secret`]
//! newtype that keeps a token out of logs, envelopes and closures.
//!
//! The shape is deliberately split in three, so the daemon
//! (`xi-sources-gitops`), the bootstrap clone (`xi-sources-bootstrap`)
//! and `--mode sources` push (`xi-sources-push`) all reach the same
//! decisions without duplicating any of them:
//!
//! - [`plan`] answers "can this remote be reached at all, and with which modes,
//!   in which order" *before* any subprocess runs. It is pure: no network, no
//!   secret reads beyond an existence check.
//! - [`Attempt::apply`] wires one mode into a [`Command`].
//! - [`classify_failure`] and [`verdict`] turn what git printed into the single
//!   `Degraded` reason the envelope carries, with the rule that a
//!   non-credential failure always outranks a credential one.
//!
//! # Three readings the spec delta left open
//!
//! The capability spec routes `ssh://` to the `ssh` kind and `https://`
//! to the `https` kind, and says the `oauth` kind has no fallthrough,
//! without saying which scheme reaches it. xi resolves the three gaps
//! as follows, and `openspec/changes/xi-sources-credentials/specs/`
//! records the same decisions:
//!
//! 1. `oauth` is an **`https` alternative**: an `https://` remote routes to
//!    `oauth` when `credentials.oauth.askpass` is set, and to `https`
//!    otherwise. That matches the pattern the proposal names it for  - CI
//!    systems handing out short-lived tokens over HTTPS.
//! 2. An absolute path and a `file://` URL route to [`Route::Local`]: there is
//!    no credential surface on a filesystem remote, so wiring one would be
//!    theatre. `xi <scope> switch /srv/mirror/cfg` keeps working.
//! 3. Every other scheme, `http://` and `git://` included, refuses with
//!    `unsupported-scheme`. Both are cleartext; a token or an identity sent
//!    over them is a token or an identity leaked.

use std::{
  borrow::Cow,
  fmt,
  fs,
  io,
  path::{Path, PathBuf},
  process::Command,
};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, Serializer, ser::Error as _};
use sha2::{Digest, Sha256};
use tracing::warn;

use super::{
  Scope,
  config::{ConfigError, Provenance, env_var_for, line_of, str_enum},
  scp_separator,
};

// ---------------------------------------------------------------------------
// Secret  -  the opaque newtype every credential value travels in
// ---------------------------------------------------------------------------

/// What [`Secret`]'s `Debug` prints instead of the value.
pub const REDACTED: &str = "<redacted>";

/// A credential value, readable only through [`Secret::with_secret`].
///
/// Three properties, all of them load-bearing:
///
/// - `Debug` prints [`REDACTED`], so a `{:?}` on any enclosing struct is safe.
///   This is what makes `tracing::debug!(?config)` harmless.
/// - `Serialize` *refuses*. A token has no business in the `xi sources status`
///   envelope, and a type that silently serialised would make that a one-line
///   mistake rather than a compile-clean impossibility.
/// - The only accessor takes a closure, so the value's lifetime is bounded by a
///   syntactic region the lint rule can see. That rule is
///   `xi_sources_observability::redact::lint`, run against the whole tree by
///   `crates/xi-sources-observability/tests/credential_lint.rs`. It lives there
///   rather than here because that crate depends on this one, so a test there
///   can scan this file and not the other way round.
///
/// The backing store is [`SecretString`], so the bytes are zeroed on
/// drop rather than left in freed memory.
pub struct Secret(SecretString);

impl Secret {
  /// Read a secret from a file, trimming the trailing newline.
  ///
  /// A token file written by `echo` or by sops-nix ends in a newline,
  /// and git's credential protocol is line-oriented: passing the
  /// newline through would truncate the reply at the wrong place.
  ///
  /// # Errors
  ///
  /// Returns [`SecretError::NotMounted`] when the path does not exist,
  /// which is the sops-nix / agenix "secret not mounted yet" case and
  /// is reported as `Pending` rather than as a failure.
  /// [`SecretError::Unreadable`] covers every other I/O error, and
  /// [`SecretError::Empty`] a file with no content.
  pub fn from_file(path: &Path) -> Result<Self, SecretError> {
    let raw = match fs::read_to_string(path) {
      Ok(raw) => raw,
      Err(err) if err.kind() == io::ErrorKind::NotFound => {
        return Err(SecretError::NotMounted {
          path: path.to_path_buf(),
        });
      },
      Err(source) => {
        return Err(SecretError::Unreadable {
          path: path.to_path_buf(),
          source,
        });
      },
    };

    let token = raw.trim();
    if token.is_empty() {
      return Err(SecretError::Empty {
        path: path.to_path_buf(),
      });
    }
    Ok(Self(SecretString::from(token.to_owned())))
  }

  /// Build a secret from a value already in memory.
  ///
  /// Used by the credential-helper path, which has the token as a
  /// `String` the moment it finishes reading the file.
  #[must_use]
  pub fn new(value: String) -> Self {
    Self(SecretString::from(value))
  }

  /// Run `f` over the secret value.
  ///
  /// The single accessor, by design. Returning the value instead would
  /// make every call site a place a token could escape into a log line.
  pub fn with_secret<T>(&self, f: impl FnOnce(&str) -> T) -> T {
    f(self.0.expose_secret())
  }

  /// Length of the secret in bytes.
  ///
  /// Safe to log: a length is not a credential, and it is the one thing
  /// an operator debugging "my token file has a stray newline" needs.
  #[must_use]
  pub fn len(&self) -> usize {
    self.0.expose_secret().len()
  }

  /// Whether the secret is zero-length. Never true via
  /// [`Secret::from_file`], which rejects an empty file outright.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }
}

impl fmt::Debug for Secret {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(REDACTED)
  }
}

impl fmt::Display for Secret {
  /// Prints [`REDACTED`], so an accidental `{}` in a message is inert
  /// rather than a leak.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(REDACTED)
  }
}

impl Serialize for Secret {
  /// Always fails. See the type-level note: a refusal at the
  /// serialisation boundary is the only version of "tokens stay out of
  /// the envelope" that cannot be forgotten.
  fn serialize<S: Serializer>(
    &self,
    _serializer: S,
  ) -> Result<S::Ok, S::Error> {
    Err(S::Error::custom(
      "credential values are never serialised; serialise the configured path \
       instead",
    ))
  }
}

/// Why a secret file could not be turned into a [`Secret`].
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
  /// The path does not exist. Distinct from unreadable on purpose: this
  /// is the normal state of a sops-nix or agenix secret before the
  /// secret manager has mounted it, and it resolves itself.
  #[error("secret is not mounted yet at {}", path.display())]
  NotMounted {
    /// The path the secret manager is expected to mount.
    path: PathBuf,
  },

  /// The path exists but could not be read.
  #[error("{}: {source}", path.display())]
  Unreadable {
    /// The unreadable path.
    path:   PathBuf,
    /// The underlying failure.
    #[source]
    source: io::Error,
  },

  /// The file exists and is readable but holds nothing.
  #[error("{} is empty", path.display())]
  Empty {
    /// The empty path.
    path: PathBuf,
  },
}

// ---------------------------------------------------------------------------
// Closed enums
// ---------------------------------------------------------------------------

/// ssh's `StrictHostKeyChecking` policy, as a closed set.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum StrictHostKeyChecking {
  /// Reject any host whose key is not already in known-hosts. The
  /// default everywhere except the bootstrap first fetch.
  #[default]
  Yes,
  /// Accept anything. Honoured when set, because an operator who spells
  /// it has said what they want; never a default.
  No,
  /// Add an unknown host's key on first use, still refusing a host
  /// whose key changed. Permitted only during bootstrap; see
  /// [`effective_policy`].
  AcceptNew,
}

str_enum!(
  StrictHostKeyChecking,
  "yes" => Yes,
  "no" => No,
  "accept-new" => AcceptNew,
);

/// Which of the four credential kinds a remote routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Route {
  /// `ssh://` or git's scp-like `[user@]host:path`.
  Ssh,
  /// `https://` with no `oauth.askpass` configured.
  Https,
  /// `https://` with `oauth.askpass` configured.
  Oauth,
  /// An absolute path or `file://`: a filesystem remote, no
  /// credentials involved.
  Local,
  /// Anything else, `http://` and `git://` included.
  Unsupported,
}

str_enum!(
  Route,
  "ssh" => Ssh,
  "https" => Https,
  "oauth" => Oauth,
  "local" => Local,
  "unsupported" => Unsupported,
);

/// One concrete way to authenticate, in fallthrough order per route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
  /// ssh with `-i <identity-file> -o IdentitiesOnly=yes`.
  SshIdentityFile,
  /// Plain ssh, so `ssh-agent` answers if one is running.
  SshAgent,
  /// ssh with no identity offered at all.
  SshAnonymous,
  /// A one-shot `credential.helper` reading the configured token file.
  HttpsTokenFile,
  /// The operator's own `credential.helper` command.
  HttpsCredentialHelper,
  /// https with every credential helper disabled.
  HttpsAnonymous,
  /// git's `GIT_ASKPASS`, pointed at the configured script.
  OauthAskpass,
  /// A filesystem remote: nothing to configure.
  Local,
}

str_enum!(
  Mode,
  "ssh-identity-file" => SshIdentityFile,
  "ssh-agent" => SshAgent,
  "ssh-anonymous" => SshAnonymous,
  "https-token-file" => HttpsTokenFile,
  "https-credential-helper" => HttpsCredentialHelper,
  "https-anonymous" => HttpsAnonymous,
  "oauth-askpass" => OauthAskpass,
  "local" => Local,
);

/// Which flow is driving the fetch.
///
/// The only thing this changes is whether `accept-new` is honoured, but
/// that one bit is the difference between trust-on-first-use and a
/// standing man-in-the-middle window. See [`effective_policy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flow {
  /// A `xi sources bootstrap` first clone, phase 3.
  Bootstrap,
  /// Everything else: daemon ticks, `switch`, push.
  #[default]
  Normal,
}

// ---------------------------------------------------------------------------
// Degraded reasons  -  the wire spellings
// ---------------------------------------------------------------------------

/// Every mode failed, and every failure was credential-shaped.
pub const REASON_CREDENTIALS_MISSING: &str = "credentials-missing";
/// The remote URL's scheme has no credential surface xi will use.
pub const REASON_UNSUPPORTED_SCHEME: &str = "unsupported-scheme";
/// The host's key differs from the one in known-hosts.
pub const REASON_HOST_KEY_CHANGED: &str = "host-key-changed";
/// The host's key is absent from known-hosts and the policy is `yes`.
pub const REASON_HOST_KEY_UNKNOWN: &str = "host-key-unknown";
/// The host name did not resolve.
pub const REASON_DNS_FAILURE: &str = "dns-failure";
/// The host resolved but the connection did not complete.
pub const REASON_NETWORK_FAILURE: &str = "network-failure";
/// git failed for a reason outside this capability's vocabulary.
pub const REASON_FETCH_FAILED: &str = "fetch-failed";
/// A configured secret path is absent; the secret manager has not
/// mounted it yet. Reported as `Pending`, not as a failure.
pub const REASON_SECRET_NOT_MOUNTED: &str = "secret-not-mounted";

// ---------------------------------------------------------------------------
// Config surface  -  [sources.credentials.<kind>]
// ---------------------------------------------------------------------------

/// The four kinds the `[sources.credentials]` table accepts.
///
/// Closed, like [`super::config::SOURCES_KEYS`]: a fifth kind is a
/// load-time error naming it, not a silently ignored table.
pub const CREDENTIAL_KINDS: &[&str] = &["ssh", "https", "oauth", "anonymous"];

/// Every leaf key, as the dotted path used for env vars and provenance.
///
/// The env spelling is mechanical: [`env_var_for`] uppercases and maps
/// both `-` and `.` to `_`, so `credentials.ssh.identity-file` is
/// `XI_SOURCES_CREDENTIALS_SSH_IDENTITY_FILE`.
pub const CREDENTIAL_KEYS: &[&str] = &[
  "credentials.ssh.identity-file",
  "credentials.ssh.use-agent",
  "credentials.ssh.known-hosts-file",
  "credentials.ssh.strict-host-key-checking",
  "credentials.https.token-file",
  "credentials.https.credential-helper",
  "credentials.https.username",
  "credentials.oauth.askpass",
  "credentials.anonymous.allow",
];

/// Leaf keys of `[sources.credentials.ssh]`.
const SSH_KEYS: &[&str] = &[
  "identity-file",
  "use-agent",
  "known-hosts-file",
  "strict-host-key-checking",
];
/// Leaf keys of `[sources.credentials.https]`.
const HTTPS_KEYS: &[&str] = &["token-file", "credential-helper", "username"];
/// Leaf keys of `[sources.credentials.oauth]`.
const OAUTH_KEYS: &[&str] = &["askpass"];
/// Leaf keys of `[sources.credentials.anonymous]`.
const ANONYMOUS_KEYS: &[&str] = &["allow"];

/// Username git uses for token auth when no layer names one.
pub const DEFAULT_HTTPS_USERNAME: &str = "git";

/// One layer's view of `[sources.credentials]`: every leaf optional.
///
/// Stacked leaf-by-leaf by [`CredentialsConfig::resolve`], exactly like
/// the flat `[sources]` keys, so a `.xi.toml` identity file and an
/// `XI_SOURCES_CREDENTIALS_HTTPS_TOKEN_FILE` combine rather than one
/// shadowing the other wholesale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialsConfig {
  /// `[sources.credentials.ssh]`.
  pub ssh:       SshConfig,
  /// `[sources.credentials.https]`.
  pub https:     HttpsConfig,
  /// `[sources.credentials.oauth]`.
  pub oauth:     OauthConfig,
  /// `[sources.credentials.anonymous]`.
  pub anonymous: AnonymousConfig,
}

/// `[sources.credentials.ssh]`, unresolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshConfig {
  /// Private key to offer, and only that key.
  pub identity_file:            Option<PathBuf>,
  /// Whether `ssh-agent` may answer. Defaults to true.
  pub use_agent:                Option<bool>,
  /// Known-hosts file override. Defaults per scope; see
  /// [`default_known_hosts`].
  pub known_hosts_file:         Option<PathBuf>,
  /// Host-key policy. Defaults to [`StrictHostKeyChecking::Yes`].
  pub strict_host_key_checking: Option<StrictHostKeyChecking>,
}

/// `[sources.credentials.https]`, unresolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpsConfig {
  /// File holding the personal access token on a single line.
  pub token_file:        Option<PathBuf>,
  /// An operator-supplied `credential.helper` command instead.
  pub credential_helper: Option<String>,
  /// Username to pair with the token. Defaults to
  /// [`DEFAULT_HTTPS_USERNAME`].
  pub username:          Option<String>,
}

/// `[sources.credentials.oauth]`, unresolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OauthConfig {
  /// Executable that prints the token, mirroring `GIT_ASKPASS`.
  pub askpass: Option<PathBuf>,
}

/// `[sources.credentials.anonymous]`, unresolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnonymousConfig {
  /// Whether an unauthenticated fetch may end the chain. Defaults to
  /// true; set false for a repo that MUST be private.
  pub allow: Option<bool>,
}

impl CredentialsConfig {
  /// True when this layer configures no credential at all.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    *self == Self::default()
  }

  /// Parse the `[sources.credentials]` sub-tree of a config file.
  ///
  /// `raw` is the whole document, used only to turn a key's span into a
  /// line number, so an error points at the offending line rather than
  /// at the table header.
  ///
  /// # Errors
  ///
  /// Returns [`ConfigError::File`] for an unknown kind, an unknown leaf
  /// key, a wrongly-typed value, or a value that fails validation.
  pub fn from_toml_item(
    path: &Path,
    raw: &str,
    item: &toml_edit::Item,
  ) -> Result<Self, ConfigError> {
    let fail_at = |span: Option<core::ops::Range<usize>>, message: String| {
      ConfigError::File {
        path: path.to_path_buf(),
        line: span.map_or(1, |span| line_of(raw, span.start)),
        message,
      }
    };

    let Some(table) = item.as_table_like() else {
      return Err(fail_at(
        item.span(),
        "'credentials' must be a table of credential kinds".to_owned(),
      ));
    };

    let mut config = Self::default();
    for (kind, kind_item) in table.iter() {
      if !CREDENTIAL_KINDS.contains(&kind) {
        return Err(fail_at(
          table
            .key(kind)
            .and_then(toml_edit::Key::span)
            .or_else(|| kind_item.span()),
          format!(
            "unknown credential kind '{kind}' in [sources.credentials] \
             (expected one of {})",
            CREDENTIAL_KINDS.join(", ")
          ),
        ));
      }

      let reader = KindReader::new(path, raw, kind, kind_item)?;
      match kind {
        "ssh" => config.ssh = reader.ssh()?,
        "https" => config.https = reader.https()?,
        "oauth" => config.oauth = reader.oauth()?,
        // `CREDENTIAL_KINDS` is the match arms' only source of truth,
        // so the fallthrough is the `anonymous` arm rather than a
        // panic that could never fire.
        _ => config.anonymous = reader.anonymous()?,
      }
    }

    Ok(config)
  }

  /// Read the `XI_SOURCES_CREDENTIALS_*` layer from an environment.
  ///
  /// # Errors
  ///
  /// Returns [`ConfigError::Env`] naming the offending variable when a
  /// value fails the same validation the TOML layer applies.
  pub fn from_env_with(
    lookup: &dyn Fn(&str) -> Option<String>,
  ) -> Result<Self, ConfigError> {
    let get = |key: &str| lookup(&env_var_for(key)).filter(|v| !v.is_empty());
    let fail = |key: &str, message: String| {
      ConfigError::Env {
        var: env_var_for(key),
        message,
      }
    };
    let abs = |key: &str| -> Result<Option<PathBuf>, ConfigError> {
      get(key).map_or_else(
        || Ok(None),
        |raw| {
          super::config::validate_abs_path(leaf_of(key), &raw)
            .map(Some)
            .map_err(|m| fail(key, m))
        },
      )
    };
    let flag = |key: &str| -> Result<Option<bool>, ConfigError> {
      get(key).map_or_else(
        || Ok(None),
        |raw| {
          super::config::parse_bool(&raw).map(Some).ok_or_else(|| {
            fail(
              key,
              format!(
                "invalid '{}' value '{raw}': expected one of true, false, 1, 0",
                leaf_of(key)
              ),
            )
          })
        },
      )
    };

    let policy_key = "credentials.ssh.strict-host-key-checking";
    let strict = get(policy_key).map_or_else(
      || Ok(None),
      |raw| {
        StrictHostKeyChecking::from_config_str(&raw)
          .map(Some)
          .ok_or_else(|| {
            fail(
              policy_key,
              format!(
                "invalid '{}' value '{raw}': expected one of {}",
                leaf_of(policy_key),
                StrictHostKeyChecking::ALL.join(", ")
              ),
            )
          })
      },
    )?;

    Ok(Self {
      ssh:       SshConfig {
        identity_file:            abs("credentials.ssh.identity-file")?,
        use_agent:                flag("credentials.ssh.use-agent")?,
        known_hosts_file:         abs("credentials.ssh.known-hosts-file")?,
        strict_host_key_checking: strict,
      },
      https:     HttpsConfig {
        token_file:        abs("credentials.https.token-file")?,
        credential_helper: get("credentials.https.credential-helper"),
        username:          get("credentials.https.username"),
      },
      oauth:     OauthConfig {
        askpass: abs("credentials.oauth.askpass")?,
      },
      anonymous: AnonymousConfig {
        allow: flag("credentials.anonymous.allow")?,
      },
    })
  }

  /// Walk the four rungs leaf by leaf, recording provenance.
  ///
  /// `rungs` is highest precedence first. Leaf keys are recorded in
  /// `provenance` under their dotted path, so `xi <scope> sources
  /// status --explain` names the rung that answered for
  /// `credentials.ssh.identity-file` the same way it does for `branch`.
  #[must_use]
  pub fn resolve(
    rungs: [(Provenance, &Self); 4],
    provenance: &mut std::collections::BTreeMap<String, Provenance>,
  ) -> ResolvedCredentials {
    // One rung-walk per leaf: find the first rung that set it, record
    // where the answer came from, and fall back to the built-in.
    macro_rules! pick {
      ($key:literal, $($field:ident).+, $default:expr) => {{
        let found = rungs
          .iter()
          .find_map(|(rung, layer)| {
            layer.$($field).+.clone().map(|value| (*rung, value))
          });
        match found {
          Some((rung, value)) => {
            provenance.insert($key.to_owned(), rung);
            Some(value)
          },
          None => {
            provenance.insert($key.to_owned(), Provenance::Default);
            $default
          },
        }
      }};
    }

    ResolvedCredentials {
      ssh:       ResolvedSsh {
        identity_file:            pick!(
          "credentials.ssh.identity-file",
          ssh.identity_file,
          None
        ),
        use_agent:                pick!(
          "credentials.ssh.use-agent",
          ssh.use_agent,
          Some(true)
        )
        .unwrap_or(true),
        known_hosts_file:         pick!(
          "credentials.ssh.known-hosts-file",
          ssh.known_hosts_file,
          None
        ),
        strict_host_key_checking: pick!(
          "credentials.ssh.strict-host-key-checking",
          ssh.strict_host_key_checking,
          Some(StrictHostKeyChecking::Yes)
        )
        .unwrap_or_default(),
      },
      https:     ResolvedHttps {
        token_file:        pick!(
          "credentials.https.token-file",
          https.token_file,
          None
        ),
        credential_helper: pick!(
          "credentials.https.credential-helper",
          https.credential_helper,
          None
        ),
        username:          pick!(
          "credentials.https.username",
          https.username,
          Some(DEFAULT_HTTPS_USERNAME.to_owned())
        )
        .unwrap_or_else(|| DEFAULT_HTTPS_USERNAME.to_owned()),
      },
      oauth:     ResolvedOauth {
        askpass: pick!("credentials.oauth.askpass", oauth.askpass, None),
      },
      anonymous: ResolvedAnonymous {
        allow: pick!(
          "credentials.anonymous.allow",
          anonymous.allow,
          Some(true)
        )
        .unwrap_or(true),
      },
    }
  }

  /// Render this layer as a `[sources.credentials]` table.
  ///
  /// Only leaves the layer actually set are emitted, so `xi sources
  /// init` writes a minimal block rather than freezing defaults.
  /// Paths are emitted; secrets never are, because a secret is never in
  /// the config in the first place  -  only the path to it is.
  #[must_use]
  pub fn to_toml_table(&self) -> Option<toml_edit::Table> {
    if self.is_empty() {
      return None;
    }

    let mut out = toml_edit::Table::new();
    out.set_implicit(true);

    let mut ssh = toml_edit::Table::new();
    put_path(&mut ssh, "identity-file", self.ssh.identity_file.as_ref());
    put_bool(&mut ssh, "use-agent", self.ssh.use_agent);
    put_path(
      &mut ssh,
      "known-hosts-file",
      self.ssh.known_hosts_file.as_ref(),
    );
    if let Some(policy) = self.ssh.strict_host_key_checking {
      ssh.insert(
        "strict-host-key-checking",
        toml_edit::value(policy.as_str()),
      );
    }

    let mut https = toml_edit::Table::new();
    put_path(&mut https, "token-file", self.https.token_file.as_ref());
    put_str(
      &mut https,
      "credential-helper",
      self.https.credential_helper.as_deref(),
    );
    put_str(&mut https, "username", self.https.username.as_deref());

    let mut oauth = toml_edit::Table::new();
    put_path(&mut oauth, "askpass", self.oauth.askpass.as_ref());

    let mut anonymous = toml_edit::Table::new();
    put_bool(&mut anonymous, "allow", self.anonymous.allow);

    for (name, table) in [
      ("ssh", ssh),
      ("https", https),
      ("oauth", oauth),
      ("anonymous", anonymous),
    ] {
      if !table.is_empty() {
        out.insert(name, toml_edit::Item::Table(table));
      }
    }

    Some(out)
  }
}

fn put_path(table: &mut toml_edit::Table, key: &str, value: Option<&PathBuf>) {
  if let Some(path) = value {
    table.insert(key, toml_edit::value(path.display().to_string()));
  }
}

fn put_str(table: &mut toml_edit::Table, key: &str, value: Option<&str>) {
  if let Some(text) = value {
    table.insert(key, toml_edit::value(text));
  }
}

fn put_bool(table: &mut toml_edit::Table, key: &str, value: Option<bool>) {
  if let Some(flag) = value {
    table.insert(key, toml_edit::value(flag));
  }
}

/// The last dotted component of a key, for error messages.
fn leaf_of(key: &str) -> &str {
  key.rsplit('.').next().unwrap_or(key)
}

/// Typed reads of one `[sources.credentials.<kind>]` table.
///
/// Exists so the four kinds share one "unknown key", one "wrong type"
/// and one "which line" implementation instead of four near-copies.
struct KindReader<'a> {
  path:  &'a Path,
  raw:   &'a str,
  kind:  &'a str,
  table: &'a dyn toml_edit::TableLike,
}

impl<'a> KindReader<'a> {
  fn new(
    path: &'a Path,
    raw: &'a str,
    kind: &'a str,
    item: &'a toml_edit::Item,
  ) -> Result<Self, ConfigError> {
    let table = item.as_table_like().ok_or_else(|| {
      ConfigError::File {
        path:    path.to_path_buf(),
        line:    item.span().map_or(1, |span| line_of(raw, span.start)),
        message: format!(
          "'[sources.credentials.{kind}]' must be a table of settings"
        ),
      }
    })?;
    Ok(Self {
      path,
      raw,
      kind,
      table,
    })
  }

  fn line_for(&self, key: &str) -> usize {
    self
      .table
      .key(key)
      .and_then(toml_edit::Key::span)
      .or_else(|| self.table.get(key).and_then(toml_edit::Item::span))
      .map_or(1, |span| line_of(self.raw, span.start))
  }

  fn fail(&self, key: &str, message: String) -> ConfigError {
    ConfigError::File {
      path: self.path.to_path_buf(),
      line: self.line_for(key),
      message,
    }
  }

  /// Reject a leaf key outside the kind's closed set.
  fn check_keys(&self, allowed: &[&str]) -> Result<(), ConfigError> {
    for (key, _) in self.table.iter() {
      if !allowed.contains(&key) {
        return Err(self.fail(
          key,
          format!(
            "unknown key '{key}' in [sources.credentials.{}] (expected one of \
             {})",
            self.kind,
            allowed.join(", ")
          ),
        ));
      }
    }
    Ok(())
  }

  fn string(&self, key: &str) -> Result<Option<&'a str>, ConfigError> {
    self.table.get(key).map_or_else(
      || Ok(None),
      |item| {
        item
          .as_str()
          .map(Some)
          .ok_or_else(|| self.fail(key, format!("'{key}' must be a string")))
      },
    )
  }

  fn boolean(&self, key: &str) -> Result<Option<bool>, ConfigError> {
    self.table.get(key).map_or_else(
      || Ok(None),
      |item| {
        item
          .as_bool()
          .map(Some)
          .ok_or_else(|| self.fail(key, format!("'{key}' must be a boolean")))
      },
    )
  }

  fn abs_path(&self, key: &str) -> Result<Option<PathBuf>, ConfigError> {
    self.string(key)?.map_or_else(
      || Ok(None),
      |raw| {
        super::config::validate_abs_path(key, raw)
          .map(Some)
          .map_err(|m| self.fail(key, m))
      },
    )
  }

  fn ssh(&self) -> Result<SshConfig, ConfigError> {
    self.check_keys(SSH_KEYS)?;
    let key = "strict-host-key-checking";
    let policy = self.string(key)?.map_or_else(
      || Ok(None),
      |raw| {
        StrictHostKeyChecking::from_config_str(raw)
          .map(Some)
          .ok_or_else(|| {
            self.fail(
              key,
              format!(
                "invalid '{key}' value '{raw}': expected one of {}",
                StrictHostKeyChecking::ALL.join(", ")
              ),
            )
          })
      },
    )?;

    Ok(SshConfig {
      identity_file:            self.abs_path("identity-file")?,
      use_agent:                self.boolean("use-agent")?,
      known_hosts_file:         self.abs_path("known-hosts-file")?,
      strict_host_key_checking: policy,
    })
  }

  fn https(&self) -> Result<HttpsConfig, ConfigError> {
    self.check_keys(HTTPS_KEYS)?;
    Ok(HttpsConfig {
      token_file:        self.abs_path("token-file")?,
      credential_helper: self.string("credential-helper")?.map(str::to_owned),
      username:          self.string("username")?.map(str::to_owned),
    })
  }

  fn oauth(&self) -> Result<OauthConfig, ConfigError> {
    self.check_keys(OAUTH_KEYS)?;
    Ok(OauthConfig {
      askpass: self.abs_path("askpass")?,
    })
  }

  fn anonymous(&self) -> Result<AnonymousConfig, ConfigError> {
    self.check_keys(ANONYMOUS_KEYS)?;
    Ok(AnonymousConfig {
      allow: self.boolean("allow")?,
    })
  }
}

// ---------------------------------------------------------------------------
// Resolved credentials
// ---------------------------------------------------------------------------

/// Every credential leaf, resolved, with defaults applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedCredentials {
  /// Resolved `ssh` kind.
  pub ssh:       ResolvedSsh,
  /// Resolved `https` kind.
  pub https:     ResolvedHttps,
  /// Resolved `oauth` kind.
  pub oauth:     ResolvedOauth,
  /// Resolved `anonymous` kind.
  pub anonymous: ResolvedAnonymous,
}

/// Resolved `[sources.credentials.ssh]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSsh {
  /// Private key to offer exclusively, if configured.
  pub identity_file:            Option<PathBuf>,
  /// Whether `ssh-agent` may answer. Defaults to true.
  pub use_agent:                bool,
  /// Known-hosts override; `None` means the per-scope default.
  pub known_hosts_file:         Option<PathBuf>,
  /// Configured host-key policy, *before* [`effective_policy`].
  pub strict_host_key_checking: StrictHostKeyChecking,
}

impl Default for ResolvedSsh {
  fn default() -> Self {
    Self {
      identity_file:            None,
      use_agent:                true,
      known_hosts_file:         None,
      strict_host_key_checking: StrictHostKeyChecking::Yes,
    }
  }
}

/// Resolved `[sources.credentials.https]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHttps {
  /// File holding the token, if configured.
  pub token_file:        Option<PathBuf>,
  /// Operator-supplied helper command, if configured.
  pub credential_helper: Option<String>,
  /// Username paired with the token. Never empty.
  pub username:          String,
}

impl Default for ResolvedHttps {
  fn default() -> Self {
    Self {
      token_file:        None,
      credential_helper: None,
      username:          DEFAULT_HTTPS_USERNAME.to_owned(),
    }
  }
}

/// Resolved `[sources.credentials.oauth]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedOauth {
  /// Askpass executable, if configured.
  pub askpass: Option<PathBuf>,
}

/// Resolved `[sources.credentials.anonymous]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAnonymous {
  /// Whether an unauthenticated fetch may end the chain.
  pub allow: bool,
}

impl Default for ResolvedAnonymous {
  fn default() -> Self {
    Self { allow: true }
  }
}

impl ResolvedCredentials {
  /// Reject the combinations the schema cannot express on its own.
  ///
  /// Two of them:
  ///
  /// - `https.token-file` *and* `https.credential-helper` both set is not an
  ///   error  -  it is the documented fallthrough  -  but neither set with
  ///   `anonymous.allow = false` leaves the https route with nothing to try,
  ///   and that is better said at load time than discovered on the daemon's
  ///   first tick.
  /// - `ssh.use-agent = false` with no `identity-file` and `anonymous.allow =
  ///   false` is the same dead end on the ssh side.
  ///
  /// # Errors
  ///
  /// Returns the reason, without a location prefix, so the caller adds
  /// the file+line or the variable name.
  pub fn check_consistency(&self) -> Result<(), String> {
    if self.anonymous.allow {
      return Ok(());
    }

    if !self.ssh.use_agent && self.ssh.identity_file.is_none() {
      return Err(
        "'[sources.credentials.anonymous] allow = false' with \
         'credentials.ssh.use-agent = false' and no \
         'credentials.ssh.identity-file' leaves an ssh remote with no way to \
         authenticate"
          .to_owned(),
      );
    }
    if self.https.token_file.is_none()
      && self.https.credential_helper.is_none()
      && self.oauth.askpass.is_none()
    {
      return Err(
        "'[sources.credentials.anonymous] allow = false' needs one of \
         'credentials.https.token-file', \
         'credentials.https.credential-helper' or 'credentials.oauth.askpass' \
         for an https remote to authenticate"
          .to_owned(),
      );
    }
    Ok(())
  }

  /// Which kind `remote_url` routes to.
  ///
  /// Pure shape analysis: no network, no filesystem, so it cannot be
  /// slow and cannot differ across machines. See the module docs for
  /// the three readings this encodes.
  #[must_use]
  pub fn route(&self, remote_url: &str) -> Route {
    match route_scheme(remote_url) {
      Route::Https if self.oauth.askpass.is_some() => Route::Oauth,
      other => other,
    }
  }
}

/// Scheme-only routing, before the `oauth` override.
fn route_scheme(remote_url: &str) -> Route {
  let url = remote_url.trim();

  if let Some(idx) = url.find("://") {
    return match url[..idx].to_ascii_lowercase().as_str() {
      "ssh" => Route::Ssh,
      "https" => Route::Https,
      "file" => Route::Local,
      _ => Route::Unsupported,
    };
  }

  // git's scp-like `[user@]host:path` is always ssh.
  if scp_separator(url).is_some() {
    return Route::Ssh;
  }

  if Path::new(url).is_absolute() {
    return Route::Local;
  }

  Route::Unsupported
}

// ---------------------------------------------------------------------------
// TOFU  -  accept-new is bootstrap-only
// ---------------------------------------------------------------------------

/// The host-key policy a fetch actually runs with.
///
/// [`StrictHostKeyChecking::AcceptNew`] is honoured only during
/// [`Flow::Bootstrap`]. Anywhere else it is promoted to `yes` and the
/// promotion is logged, so a config that would leave a standing
/// trust-on-first-use window on every daemon tick narrows to the one
/// fetch that genuinely has no prior knowledge of the host.
#[must_use]
pub fn effective_policy(
  configured: StrictHostKeyChecking,
  flow: Flow,
) -> StrictHostKeyChecking {
  if configured == StrictHostKeyChecking::AcceptNew && flow == Flow::Normal {
    warn!(
      "strict-host-key-checking = \"accept-new\" is permitted only during \
       bootstrap; using \"yes\" for this fetch"
    );
    return StrictHostKeyChecking::Yes;
  }
  configured
}

// ---------------------------------------------------------------------------
// Known-hosts fingerprints
// ---------------------------------------------------------------------------

/// SHA-256 fingerprints recorded for `host` in a known-hosts file.
///
/// Returns them in file order, in OpenSSH's `SHA256:<base64>` spelling,
/// so bootstrap can log the key it just accepted and a later tick can
/// see that a host now has two different keys without re-deriving
/// ssh's own comparison.
///
/// A missing file yields an empty list: "no keys known" is a normal
/// state, not an error. Lines that are not parseable host-key entries
/// (comments, blank lines, hashed `|1|` entries whose host cannot be
/// matched textually) are skipped.
///
/// # Errors
///
/// Propagates any I/O error other than `NotFound`.
pub fn host_key_fingerprints(
  known_hosts: &Path,
  host: &str,
) -> io::Result<Vec<String>> {
  let raw = match fs::read_to_string(known_hosts) {
    Ok(raw) => raw,
    Err(err) if err.kind() == io::ErrorKind::NotFound => {
      return Ok(Vec::new());
    },
    Err(err) => return Err(err),
  };

  let wanted = host.trim().to_ascii_lowercase();
  Ok(
    raw
      .lines()
      .filter_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
          return None;
        }
        let mut fields = line.split_whitespace();
        let hosts = fields.next()?;
        let _keytype = fields.next()?;
        let blob = fields.next()?;
        let matches = hosts
          .split(',')
          .any(|pattern| pattern.to_ascii_lowercase() == wanted);
        matches.then(|| fingerprint_of_blob(blob)).flatten()
      })
      .collect(),
  )
}

/// OpenSSH's `SHA256:<unpadded-base64>` fingerprint of a base64 key blob.
fn fingerprint_of_blob(blob: &str) -> Option<String> {
  let bytes = base64_decode(blob)?;
  let digest = Sha256::digest(&bytes);
  Some(format!("SHA256:{}", base64_encode_unpadded(&digest)))
}

/// The standard base64 alphabet. Written out rather than pulled in as a
/// dependency: the two functions below are the whole need, and a
/// fingerprint has to be byte-identical to `ssh-keygen -l` output.
const BASE64: &[u8; 64] =
  b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_decode(text: &str) -> Option<Vec<u8>> {
  let mut out = Vec::with_capacity(text.len() / 4 * 3);
  let mut acc: u32 = 0;
  let mut bits: u32 = 0;
  for byte in text.trim().bytes() {
    if byte == b'=' {
      break;
    }
    let value = u32::try_from(BASE64.iter().position(|b| *b == byte)?).ok()?;
    acc = (acc << 6) | value;
    bits += 6;
    if bits >= 8 {
      bits -= 8;
      out.push(u8::try_from((acc >> bits) & 0xFF).ok()?);
    }
  }
  Some(out)
}

fn base64_encode_unpadded(bytes: &[u8]) -> String {
  let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
  for chunk in bytes.chunks(3) {
    let b0 = chunk.first().copied().unwrap_or(0);
    let b1 = chunk.get(1).copied().unwrap_or(0);
    let b2 = chunk.get(2).copied().unwrap_or(0);
    let packed = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
    for (index, shift) in [18_u32, 12, 6, 0].into_iter().enumerate() {
      if index <= chunk.len() {
        let sextet = usize::try_from((packed >> shift) & 0x3F).unwrap_or(0);
        out.push(char::from(BASE64[sextet]));
      }
    }
  }
  out
}

// ---------------------------------------------------------------------------
// SSH invocation
// ---------------------------------------------------------------------------

/// Where ssh looks for known hosts when no layer overrides it.
///
/// System scope reads the machine-wide file, because a system-scope
/// fetch runs as root and has no `$HOME` worth trusting. User scope
/// reads the operator's own file.
#[must_use]
pub fn default_known_hosts(
  scope: Scope,
  lookup: &dyn Fn(&str) -> Option<String>,
) -> PathBuf {
  const SYSTEM: &str = "/etc/ssh/ssh_known_hosts";
  match scope {
    Scope::System => PathBuf::from(SYSTEM),
    Scope::User => {
      lookup("HOME").filter(|home| !home.is_empty()).map_or_else(
        || PathBuf::from(SYSTEM),
        |home| PathBuf::from(home).join(".ssh").join("known_hosts"),
      )
    },
  }
}

/// The ssh command line one [`Mode`] hands to git via `GIT_SSH_COMMAND`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshInvocation {
  /// The ssh program. `ssh` in production; a field so a test can point
  /// it at a stub transport without a real sshd or a real key.
  pub program:          String,
  /// Offered with `-o IdentitiesOnly=yes` when set.
  pub identity_file:    Option<PathBuf>,
  /// Passed as `-o UserKnownHostsFile=`.
  pub known_hosts_file: PathBuf,
  /// Passed as `-o StrictHostKeyChecking=`. Already through
  /// [`effective_policy`].
  pub policy:           StrictHostKeyChecking,
  /// Whether `ssh-agent` may answer. False adds
  /// `-o IdentityAgent=none`.
  pub offer_agent:      bool,
}

impl SshInvocation {
  /// Render the `GIT_SSH_COMMAND` value.
  ///
  /// With an identity file this is exactly the composition the
  /// capability spec pins:
  /// `ssh -i <identity-file> -o IdentitiesOnly=yes
  /// -o UserKnownHostsFile=<known-hosts-file>
  /// -o StrictHostKeyChecking=<policy>`.
  ///
  /// `IdentitiesOnly=yes` is not optional: without it a live agent
  /// holding ten other identities offers all of them, the server
  /// rate-limits or picks the wrong one, and the operator's explicit
  /// deploy key is not what authenticated.
  ///
  /// There is deliberately no `-o BatchMode=yes` here. Hang prevention
  /// is `GIT_TERMINAL_PROMPT=0`, which every [`Attempt`] already sets,
  /// and keeping this string to the four pinned options means the spec
  /// and the code can be compared by eye.
  #[must_use]
  pub fn to_git_ssh_command(&self) -> String {
    let mut parts = vec![quote(&self.program)];

    if let Some(identity) = self.identity_file.as_ref() {
      parts.push("-i".to_owned());
      parts.push(quote(&identity.display().to_string()));
      parts.push("-o".to_owned());
      parts.push("IdentitiesOnly=yes".to_owned());
    } else if !self.offer_agent {
      // No key and no agent: the anonymous ssh mode. Both options are
      // needed, since IdentitiesOnly alone still lets ssh read
      // ~/.ssh/id_* from disk.
      parts.push("-o".to_owned());
      parts.push("IdentitiesOnly=yes".to_owned());
      parts.push("-o".to_owned());
      parts.push("IdentityAgent=none".to_owned());
    }

    parts.push("-o".to_owned());
    parts.push(quote(&format!(
      "UserKnownHostsFile={}",
      self.known_hosts_file.display()
    )));
    parts.push("-o".to_owned());
    parts.push(format!("StrictHostKeyChecking={}", self.policy.as_str()));

    parts.join(" ")
  }
}

/// Shell-quote a value destined for a `GIT_SSH_COMMAND` word.
fn quote(value: &str) -> String {
  shlex::try_quote(value).map_or_else(|_| value.to_owned(), Cow::into_owned)
}

// ---------------------------------------------------------------------------
// Attempts and the plan
// ---------------------------------------------------------------------------

/// One mode, rendered as the environment and `git -c` overrides it
/// needs.
///
/// Carrying the wiring as data rather than mutating a [`Command`] in
/// place is what lets [`plan`] stay pure and lets a test assert the
/// exact composition without spawning anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
  /// Which mode this is.
  pub mode:   Mode,
  /// Environment variables to set on the git subprocess.
  pub env:    Vec<(String, String)>,
  /// `git -c <value>` overrides, in order.
  pub config: Vec<String>,
}

impl Attempt {
  /// Wire this attempt into `command`.
  ///
  /// Sets the environment but *not* the `-c` flags: those are git
  /// arguments and must precede the subcommand, which only the caller
  /// knows how to order. Use [`Attempt::config_args`] for those.
  pub fn apply(&self, command: &mut Command) {
    for (key, value) in &self.env {
      command.env(key, value);
    }
  }

  /// The `-c <value>` pairs, flattened, ready to splice before a git
  /// subcommand.
  #[must_use]
  pub fn config_args(&self) -> Vec<String> {
    self
      .config
      .iter()
      .flat_map(|value| ["-c".to_owned(), value.clone()])
      .collect()
  }
}

/// `GIT_SSH_COMMAND`: the ssh command line git uses for ssh remotes.
pub const GIT_SSH_COMMAND: &str = "GIT_SSH_COMMAND";
/// `GIT_ASKPASS`: the program git asks for a username or password.
pub const GIT_ASKPASS: &str = "GIT_ASKPASS";
/// `GIT_TERMINAL_PROMPT`: `0` turns a credential prompt into an error.
pub const GIT_TERMINAL_PROMPT: &str = "GIT_TERMINAL_PROMPT";

/// What xi knows about a remote before it spawns anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
  /// The chain has at least one mode to try.
  Ready {
    /// Which kind the remote routed to.
    route:    Route,
    /// The modes to try, in fallthrough order.
    attempts: Vec<Attempt>,
  },
  /// A configured secret path does not exist yet. The sops-nix /
  /// agenix case: the secret manager has not mounted it, and it will.
  Pending {
    /// What is missing, naming the mount path.
    reason: String,
  },
  /// Nothing can be tried. Reported without spawning git, because
  /// spawning it would fail for a reason the operator cannot act on.
  Degraded {
    /// What is wrong, prefixed with one of the `REASON_*` spellings.
    reason: String,
  },
}

/// One fetch's inputs, beyond the credentials themselves.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
  /// The remote being reached.
  pub remote_url: &'a str,
  /// Which half of the machine is fetching, for known-hosts defaults.
  pub scope:      Scope,
  /// Whether `accept-new` is permitted; see [`effective_policy`].
  pub flow:       Flow,
}

/// Build the fallthrough chain for one remote.
///
/// Pure except for an existence check on configured secret paths, which
/// is what separates `Pending` (sops has not mounted it yet) from
/// `Degraded` (nothing to try). No network, no secret contents read.
///
/// `lookup` supplies the environment the known-hosts default resolves
/// against, so a test never has to touch the operator's real
/// `~/.ssh`.
#[must_use]
pub fn plan(
  request: &Request<'_>,
  credentials: &ResolvedCredentials,
  lookup: &dyn Fn(&str) -> Option<String>,
) -> Readiness {
  let route = credentials.route(request.remote_url);

  let attempts = match route {
    Route::Unsupported => {
      return Readiness::Degraded {
        reason: format!(
          "{REASON_UNSUPPORTED_SCHEME}: xi will not send credentials to \
           '{}'. Use an https:// URL, an ssh:// URL, git's scp-style \
           'host:path', or an absolute local path.",
          request.remote_url
        ),
      };
    },
    Route::Local => vec![local_attempt()],
    Route::Ssh => ssh_attempts(request, credentials, lookup),
    Route::Https => https_attempts(request.scope, credentials),
    Route::Oauth => {
      match oauth_attempt(credentials) {
        Ok(attempt) => vec![attempt],
        Err(pending) => return pending,
      }
    },
  };

  // A configured secret that is not on disk yet is `Pending`, not a
  // failure: reporting `credentials-missing` for a sops-nix path that
  // mounts two seconds later would send the operator after the wrong
  // problem.
  if let Some(reason) = pending_secret(route, credentials) {
    return Readiness::Pending { reason };
  }

  if attempts.is_empty() {
    return Readiness::Degraded {
      reason: no_usable_mode_reason(request.remote_url, route, credentials),
    };
  }

  Readiness::Ready { route, attempts }
}

/// The `Pending` reason for a configured-but-unmounted secret path.
fn pending_secret(
  route: Route,
  credentials: &ResolvedCredentials,
) -> Option<String> {
  let configured = match route {
    Route::Ssh => credentials.ssh.identity_file.as_ref(),
    Route::Https => credentials.https.token_file.as_ref(),
    Route::Oauth => credentials.oauth.askpass.as_ref(),
    Route::Local | Route::Unsupported => None,
  }?;

  (!configured.exists()).then(|| {
    format!(
      "{REASON_SECRET_NOT_MOUNTED}: {} does not exist yet. If it is managed \
       by sops-nix or agenix, xi is waiting for the secret manager to mount \
       it.",
      configured.display()
    )
  })
}

fn local_attempt() -> Attempt {
  Attempt {
    mode:   Mode::Local,
    env:    vec![(GIT_TERMINAL_PROMPT.to_owned(), "0".to_owned())],
    config: Vec::new(),
  }
}

fn ssh_attempts(
  request: &Request<'_>,
  credentials: &ResolvedCredentials,
  lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<Attempt> {
  let ssh = &credentials.ssh;
  let known_hosts = ssh
    .known_hosts_file
    .clone()
    .unwrap_or_else(|| default_known_hosts(request.scope, lookup));
  let policy = effective_policy(ssh.strict_host_key_checking, request.flow);

  let invocation = |identity: Option<PathBuf>, offer_agent: bool| {
    let command = SshInvocation {
      program: "ssh".to_owned(),
      identity_file: identity,
      known_hosts_file: known_hosts.clone(),
      policy,
      offer_agent,
    };
    vec![
      (GIT_SSH_COMMAND.to_owned(), command.to_git_ssh_command()),
      (GIT_TERMINAL_PROMPT.to_owned(), "0".to_owned()),
    ]
  };

  let mut attempts = Vec::new();
  if let Some(identity) = ssh.identity_file.clone() {
    attempts.push(Attempt {
      mode:   Mode::SshIdentityFile,
      env:    invocation(Some(identity), false),
      config: Vec::new(),
    });
  }
  if ssh.use_agent {
    attempts.push(Attempt {
      mode:   Mode::SshAgent,
      env:    invocation(None, true),
      config: Vec::new(),
    });
  }
  if credentials.anonymous.allow {
    attempts.push(Attempt {
      mode:   Mode::SshAnonymous,
      env:    invocation(None, false),
      config: Vec::new(),
    });
  }
  attempts
}

fn https_attempts(
  scope: Scope,
  credentials: &ResolvedCredentials,
) -> Vec<Attempt> {
  let https = &credentials.https;
  let mut attempts = Vec::new();

  if let Some(token_file) = https.token_file.as_ref() {
    attempts.push(Attempt {
      mode:   Mode::HttpsTokenFile,
      env:    vec![(GIT_TERMINAL_PROMPT.to_owned(), "0".to_owned())],
      config: vec![
        // An empty helper first, clearing anything the machine's
        // /etc/gitconfig configured: an outer helper that answers with
        // a stale token would mask the one the operator just pointed
        // xi at.
        "credential.helper=".to_owned(),
        format!(
          "credential.helper={}",
          helper_invocation(scope, token_file, &https.username)
        ),
        format!("credential.username={}", https.username),
      ],
    });
  }

  if let Some(helper) = https.credential_helper.as_ref() {
    attempts.push(Attempt {
      mode:   Mode::HttpsCredentialHelper,
      env:    vec![(GIT_TERMINAL_PROMPT.to_owned(), "0".to_owned())],
      config: vec![
        "credential.helper=".to_owned(),
        format!("credential.helper={helper}"),
      ],
    });
  }

  if credentials.anonymous.allow {
    attempts.push(Attempt {
      mode:   Mode::HttpsAnonymous,
      env:    vec![(GIT_TERMINAL_PROMPT.to_owned(), "0".to_owned())],
      config: vec!["credential.helper=".to_owned()],
    });
  }

  attempts
}

/// The `oauth` kind, which has no fallthrough by spec: either the
/// askpass script answers or the fetch fails.
fn oauth_attempt(
  credentials: &ResolvedCredentials,
) -> Result<Attempt, Readiness> {
  let askpass = credentials.oauth.askpass.as_ref().ok_or_else(|| {
    Readiness::Degraded {
      reason: format!(
        "{REASON_CREDENTIALS_MISSING}: the oauth kind needs \
         '[sources.credentials.oauth] askpass'"
      ),
    }
  })?;

  Ok(Attempt {
    mode:   Mode::OauthAskpass,
    env:    vec![
      (GIT_ASKPASS.to_owned(), askpass.display().to_string()),
      (GIT_TERMINAL_PROMPT.to_owned(), "0".to_owned()),
    ],
    config: vec!["credential.helper=".to_owned()],
  })
}

/// The `Degraded` reason when the chain has nothing left to try.
///
/// Names the remote and the exact config keys that would unblock it,
/// because "credentials-missing" on its own sends the operator to the
/// documentation rather than to their editor.
fn no_usable_mode_reason(
  remote_url: &str,
  route: Route,
  credentials: &ResolvedCredentials,
) -> String {
  let keys = unblocking_keys(route);
  let anonymous = if credentials.anonymous.allow {
    ""
  } else {
    " '[sources.credentials.anonymous] allow = false' rules out an \
     unauthenticated fetch, so one of the authenticated modes is required."
  };
  format!(
    "{REASON_CREDENTIALS_MISSING}: no usable credential mode for {remote_url} \
     (routed to the '{}' kind). Set one of: {}.{anonymous}",
    route.as_str(),
    keys.join(", ")
  )
}

/// The config keys that would give a route something to try.
#[must_use]
pub fn unblocking_keys(route: Route) -> Vec<&'static str> {
  match route {
    Route::Ssh => {
      vec![
        "[sources.credentials.ssh] identity-file",
        "[sources.credentials.ssh] use-agent",
        "[sources.credentials.anonymous] allow",
      ]
    },
    Route::Https => {
      vec![
        "[sources.credentials.https] token-file",
        "[sources.credentials.https] credential-helper",
        "[sources.credentials.anonymous] allow",
      ]
    },
    Route::Oauth => vec!["[sources.credentials.oauth] askpass"],
    Route::Local | Route::Unsupported => Vec::new(),
  }
}

// ---------------------------------------------------------------------------
// Failure classification and the verdict
// ---------------------------------------------------------------------------

/// What kind of failure git reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
  /// The credential was missing, wrong, or refused.
  Credential,
  /// The host's key no longer matches known-hosts.
  HostKeyChanged,
  /// The host's key is not in known-hosts and the policy is `yes`.
  HostKeyUnknown,
  /// The host name did not resolve.
  Dns,
  /// The host resolved but the connection or the server failed.
  Network,
  /// Something else entirely: a missing branch, a corrupt repo.
  Other,
}

impl FailureClass {
  /// The `REASON_*` spelling this class maps to.
  #[must_use]
  pub const fn reason(self) -> &'static str {
    match self {
      Self::Credential => REASON_CREDENTIALS_MISSING,
      Self::HostKeyChanged => REASON_HOST_KEY_CHANGED,
      Self::HostKeyUnknown => REASON_HOST_KEY_UNKNOWN,
      Self::Dns => REASON_DNS_FAILURE,
      Self::Network => REASON_NETWORK_FAILURE,
      Self::Other => REASON_FETCH_FAILED,
    }
  }

  /// Whether this failure is about credentials.
  ///
  /// The whole point of the distinction: a non-credential failure
  /// outranks a credential one in [`verdict`], so a wrong identity
  /// never masks an unreachable host.
  #[must_use]
  pub const fn is_credential(self) -> bool {
    matches!(self, Self::Credential)
  }
}

/// Classify what git printed on stderr.
///
/// Order matters. A changed host key also prints "Host key verification
/// failed", and an HTTPS 403 also prints "unable to access", so the
/// more specific pattern has to be tested first.
#[must_use]
pub fn classify_failure(stderr: &str) -> FailureClass {
  let text = stderr.to_ascii_lowercase();
  let any = |needles: &[&str]| needles.iter().any(|n| text.contains(n));

  if any(&[
    "remote host identification has changed",
    "host key for",
    "key verification failed for host",
  ]) && any(&["changed", "has changed"])
  {
    return FailureClass::HostKeyChanged;
  }
  if any(&[
    "host key verification failed",
    "no matching host key",
    "no rsa host key is known",
  ]) {
    return FailureClass::HostKeyUnknown;
  }
  if any(&[
    "could not resolve host",
    "could not resolve hostname",
    "name or service not known",
    "nodename nor servname provided",
    "temporary failure in name resolution",
  ]) {
    return FailureClass::Dns;
  }
  if any(&[
    "authentication failed",
    "permission denied (publickey",
    "permission denied, please try again",
    "could not read username",
    "could not read password",
    "terminal prompts disabled",
    "invalid username or password",
    "403 forbidden",
    "401 unauthorized",
    "repository not found",
    "access denied",
  ]) {
    return FailureClass::Credential;
  }
  if any(&[
    "connection refused",
    "connection timed out",
    "connection reset",
    "operation timed out",
    "network is unreachable",
    "502 bad gateway",
    "503 service unavailable",
    "504 gateway timeout",
    "the remote end hung up",
  ]) {
    return FailureClass::Network;
  }
  FailureClass::Other
}

/// What one attempted mode did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
  /// Which mode ran.
  pub mode:   Mode,
  /// How its failure classified.
  pub class:  FailureClass,
  /// git's own message, trimmed. Never contains a secret: the token
  /// travels over the helper's stdout, not over the command line.
  pub detail: String,
}

impl Outcome {
  /// Classify a git failure into an outcome.
  #[must_use]
  pub fn from_stderr(mode: Mode, stderr: &str) -> Self {
    let detail = stderr.trim();
    Self {
      mode,
      class: classify_failure(detail),
      detail: detail.to_owned(),
    }
  }
}

/// The single `Degraded` reason for a chain that ran out of modes.
///
/// The rule the spec pins: the first non-credential failure wins, so a
/// DNS failure is reported as `dns-failure` even when credentials are
/// also unconfigured. Only when *every* mode failed on credentials is
/// the answer `credentials-missing`, and then the message names the
/// remote and the keys that would unblock it.
#[must_use]
pub fn verdict(remote_url: &str, route: Route, outcomes: &[Outcome]) -> String {
  if let Some(hard) = outcomes.iter().find(|o| !o.class.is_credential()) {
    return format!("{}: {}", hard.class.reason(), hard.detail);
  }

  let keys = unblocking_keys(route);
  let tried: Vec<&str> = outcomes.iter().map(|o| o.mode.as_str()).collect();
  if keys.is_empty() {
    return format!(
      "{REASON_CREDENTIALS_MISSING}: every credential mode failed for \
       {remote_url} (tried {})",
      tried.join(", ")
    );
  }
  format!(
    "{REASON_CREDENTIALS_MISSING}: every credential mode failed for \
     {remote_url} (routed to the '{}' kind, tried {}). Configure one of: {}.",
    route.as_str(),
    tried.join(", "),
    keys.join(", ")
  )
}

// ---------------------------------------------------------------------------
// The one-shot credential helper
// ---------------------------------------------------------------------------

/// The hidden verb git invokes as a credential helper.
pub const HELPER_VERB: &str = "credential-helper";

/// The `credential.helper` value that re-enters this binary.
///
/// Leading `!` is git's "run this through the shell" marker, which is
/// what lets the value carry flags. git appends the operation
/// (`get`, `store`, `erase`) as the final argument, which is why the
/// helper takes its own inputs as flags rather than positionally.
///
/// Falls back to the literal `xi` when the current executable cannot be
/// resolved, which is the right answer for a wrapper on `PATH`.
///
/// The token travels over the helper's *stdout*, never over this
/// command line: an argv is readable by any process on the machine, so
/// a `--token` flag here would undo the whole capability.
#[must_use]
pub fn helper_invocation(
  scope: Scope,
  token_file: &Path,
  username: &str,
) -> String {
  let exe = std::env::current_exe()
    .map_or_else(|_| "xi".to_owned(), |path| path.display().to_string());
  format!(
    "!{} {} sources {HELPER_VERB} --token-file {} --username {}",
    quote(&exe),
    scope.sources_verb(),
    quote(&token_file.display().to_string()),
    quote(username)
  )
}

/// git's `get` operation, the only one this helper answers.
const HELPER_GET: &str = "get";
/// The spelling the tasks list uses for the same operation.
const HELPER_FILL: &str = "fill";

/// Run the one-shot credential helper over an input and an output.
///
/// Implements git's credential-helper protocol for the read half only:
///
/// 1. Drain the `key=value` request from `input` up to the blank line. Draining
///    matters even though nothing in it is used: git writes the whole request
///    and a helper that exits early makes git see a broken pipe instead of an
///    answer.
/// 2. For `get` (or `fill`), read the token file and write
///    `username=<username>` and `password=<token>` to `output`.
/// 3. For `store` and `erase`, write nothing and succeed. xi has no credential
///    cache to update, and a helper that errors on them makes every successful
///    fetch print a warning.
///
/// The reply buffer is zeroed after the write, and the token itself
/// lives in a [`Secret`], so neither the plaintext nor the assembled
/// reply is left in freed memory.
///
/// # Errors
///
/// Propagates I/O errors from `input` and `output`, and returns
/// `InvalidData` when the token file is unreadable or empty. A
/// not-yet-mounted token file is `NotFound`, so a caller can tell the
/// sops-nix case apart.
pub fn run_credential_helper(
  operation: &str,
  token_file: &Path,
  username: &str,
  input: &mut dyn io::BufRead,
  output: &mut dyn io::Write,
) -> io::Result<()> {
  drain_request(input)?;

  if operation != HELPER_GET && operation != HELPER_FILL {
    return Ok(());
  }

  let secret = Secret::from_file(token_file).map_err(|err| {
    match err {
      SecretError::NotMounted { .. } => {
        io::Error::new(io::ErrorKind::NotFound, err.to_string())
      },
      SecretError::Unreadable { source, .. } => source,
      SecretError::Empty { .. } => {
        io::Error::new(io::ErrorKind::InvalidData, err.to_string())
      },
    }
  })?;

  // Built as bytes rather than as a String so the buffer can be zeroed
  // by hand once git has it; a String would leave the token in the
  // allocator's free list.
  let mut reply = secret.with_secret(|token| {
    format!("username={username}\npassword={token}\n").into_bytes()
  });
  let result = output.write_all(&reply).and_then(|()| output.flush());
  reply.fill(0);
  result
}

/// Read git's request up to the blank line that ends it.
fn drain_request(input: &mut dyn io::BufRead) -> io::Result<()> {
  let mut line = String::new();
  loop {
    line.clear();
    let read = input.read_line(&mut line)?;
    if read == 0 || line.trim().is_empty() {
      return Ok(());
    }
  }
}

// ---------------------------------------------------------------------------
// --explain
// ---------------------------------------------------------------------------

/// The `credentials` block of `xi <scope> sources status --explain`.
///
/// Carries the route, the modes in fallthrough order, and the path each
/// mode reads from. Never a secret: [`Secret`] refuses to serialise, and
/// nothing here holds one in the first place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialsExplain {
  /// Which kind the configured remote routes to.
  pub route: Route,
  /// The modes that would be tried, in order.
  pub modes: Vec<ModeExplain>,
}

/// One mode, as `--explain` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeExplain {
  /// The mode's wire name.
  pub mode:   Mode,
  /// The configured path this mode reads, if it reads one. The path,
  /// never its contents.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub source: Option<PathBuf>,
}

impl CredentialsExplain {
  /// Describe what a fetch of `remote_url` would try.
  ///
  /// An unroutable remote, or one whose secret is not mounted, still
  /// gets a route and an empty mode list rather than an error:
  /// `--explain` exists to show the operator what xi thinks, including
  /// when what xi thinks is "nothing will work".
  #[must_use]
  pub fn describe(
    remote_url: &str,
    credentials: &ResolvedCredentials,
    scope: Scope,
    lookup: &dyn Fn(&str) -> Option<String>,
  ) -> Self {
    let route = credentials.route(remote_url);
    let request = Request {
      remote_url,
      scope,
      flow: Flow::Normal,
    };
    let modes = match plan(&request, credentials, lookup) {
      Readiness::Ready { attempts, .. } => {
        attempts
          .iter()
          .map(|attempt| {
            ModeExplain {
              mode:   attempt.mode,
              source: source_path(attempt.mode, credentials),
            }
          })
          .collect()
      },
      Readiness::Pending { .. } | Readiness::Degraded { .. } => Vec::new(),
    };
    Self { route, modes }
  }
}

/// The configured path a mode reads a credential from.
fn source_path(
  mode: Mode,
  credentials: &ResolvedCredentials,
) -> Option<PathBuf> {
  match mode {
    Mode::SshIdentityFile => credentials.ssh.identity_file.clone(),
    Mode::HttpsTokenFile => credentials.https.token_file.clone(),
    Mode::OauthAskpass => credentials.oauth.askpass.clone(),
    Mode::SshAgent
    | Mode::SshAnonymous
    | Mode::HttpsCredentialHelper
    | Mode::HttpsAnonymous
    | Mode::Local => None,
  }
}
