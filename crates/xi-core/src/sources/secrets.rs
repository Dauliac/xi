//! The `[sources.secrets.<name>]` stanza.
//!
//! Secrets are declared next to the rest of the sources config but are
//! deliberately *not* part of the scalar precedence ladder in
//! [`super::config::Layers`]: a secret is a record, not a value, so the
//! ladder merges them by name instead of letting one rung replace the
//! whole set.
//!
//! The payloads never enter a Nix closure. `xi push` uploads them
//! out-of-band over SSH, which is the whole reason this stanza exists
//! rather than a `builtins.readFile` in the flake.
//!
//! Owned by the `xi-sources-push` capability; parsed here because
//! `xi-core` is where the `[sources]` table is read and because the
//! daemons cannot depend on the push crate.

use std::{
  collections::{BTreeMap, BTreeSet},
  fmt,
  path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::config::{ConfigError, line_of};

/// Every key a `[sources.secrets.<name>]` table accepts.
///
/// Closed, like [`super::config::SOURCES_KEYS`]: a typo is a load-time
/// error rather than a secret that silently lands with the wrong mode.
pub const SECRET_KEYS: &[&str] = &[
  "source",
  "destination",
  "owner",
  "group",
  "mode",
  "upload-at",
  "upload_at",
];

/// Mode a secret lands with when the stanza does not say.
pub const DEFAULT_SECRET_MODE: &str = "0600";

/// Owner a secret lands as when the stanza does not say.
pub const DEFAULT_SECRET_OWNER: &str = "root";

/// When in the push pipeline a secret is uploaded.
#[derive(
  Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum UploadAt {
  /// Before activation. The default, because a unit that needs the
  /// secret to start is the common case.
  #[default]
  PreActivate,
  /// After activation, for secrets a freshly activated generation has to
  /// be running to receive.
  PostActivate,
}

impl UploadAt {
  /// Both spellings, for the "one of a, b" error tail.
  pub const ALL: &'static [&'static str] = &["pre_activate", "post_activate"];

  /// Parse the config spelling, accepting snake and kebab case.
  ///
  /// The spec writes `pre_activate`; the rest of the `[sources]` table is
  /// kebab-case. Accepting both costs nothing and spares the operator a
  /// coin flip.
  #[must_use]
  pub fn from_config_str(raw: &str) -> Option<Self> {
    match raw.trim() {
      "pre_activate" | "pre-activate" => Some(Self::PreActivate),
      "post_activate" | "post-activate" => Some(Self::PostActivate),
      _ => None,
    }
  }

  /// Stable `snake_case` token used on the wire.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::PreActivate => "pre_activate",
      Self::PostActivate => "post_activate",
    }
  }
}

impl fmt::Display for UploadAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// One declared secret: a local file and where it lands on the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretSpec {
  /// Stanza name, i.e. the `<name>` in `[sources.secrets.<name>]`.
  pub name:        String,
  /// Path on the operator's machine. May be relative to the config file.
  pub source:      PathBuf,
  /// Absolute path on the target.
  pub destination: PathBuf,
  /// Owning user on the target. Defaults to [`DEFAULT_SECRET_OWNER`].
  pub owner:       String,
  /// Owning group on the target. `None` means the owner's primary group.
  pub group:       Option<String>,
  /// Octal mode string. Defaults to [`DEFAULT_SECRET_MODE`].
  pub mode:        String,
  /// Which side of activation the upload happens on.
  pub upload_at:   UploadAt,
}

impl SecretSpec {
  /// The `source` path resolved against the directory holding the config
  /// file that declared it.
  ///
  /// A relative `source` is the documented spelling (`source = "./key"`),
  /// and a push can be started from any CWD, so the relative path has to
  /// be anchored to something stable.
  #[must_use]
  pub fn source_from(&self, config_dir: &Path) -> PathBuf {
    if self.source.is_absolute() {
      self.source.clone()
    } else {
      config_dir.join(&self.source)
    }
  }
}

/// Parse the `[sources.secrets]` sub-table.
///
/// `raw` and `path` are only used to build the file+line error location;
/// `table` is the already-parsed `secrets` table.
///
/// # Errors
///
/// Returns a [`ConfigError::File`] naming the offending stanza's line for
/// an unknown key, a wrongly-typed value, a missing required key, or a
/// value that fails validation.
pub fn parse_secrets_table(
  path: &Path,
  raw: &str,
  table: &toml_edit::Table,
) -> Result<BTreeMap<String, SecretSpec>, ConfigError> {
  let mut out = BTreeMap::new();

  for (name, item) in table {
    let line = table
      .key(name)
      .and_then(toml_edit::Key::span)
      .or_else(|| table.span())
      .map_or(1, |span| line_of(raw, span.start));
    let fail = |message: String| {
      ConfigError::File {
        path: path.to_path_buf(),
        line,
        message,
      }
    };

    let stanza = item.as_table().ok_or_else(|| {
      fail(format!(
        "[sources.secrets.{name}] must be a table of secret settings"
      ))
    })?;

    out.insert(name.to_owned(), parse_one_secret(name, stanza, &fail)?);
  }

  Ok(out)
}

/// Parse a single `[sources.secrets.<name>]` stanza.
fn parse_one_secret(
  name: &str,
  stanza: &toml_edit::Table,
  fail: &impl Fn(String) -> ConfigError,
) -> Result<SecretSpec, ConfigError> {
  for (key, _) in stanza {
    if !SECRET_KEYS.contains(&key) {
      return Err(fail(format!(
        "unknown key '{key}' in [sources.secrets.{name}]"
      )));
    }
  }

  let string = |key: &str| -> Result<Option<&str>, ConfigError> {
    stanza.get(key).map_or_else(
      || Ok(None),
      |item| {
        item.as_str().map(Some).ok_or_else(|| {
          fail(format!(
            "'{key}' in [sources.secrets.{name}] must be a string"
          ))
        })
      },
    )
  };

  let source = string("source")?.ok_or_else(|| {
    fail(format!("[sources.secrets.{name}] needs a 'source' path"))
  })?;
  if source.trim().is_empty() {
    return Err(fail(format!(
      "'source' in [sources.secrets.{name}] must not be empty"
    )));
  }

  let destination = string("destination")?.ok_or_else(|| {
    fail(format!(
      "[sources.secrets.{name}] needs a 'destination' path"
    ))
  })?;
  if !Path::new(destination.trim()).is_absolute() {
    return Err(fail(format!(
      "'destination' in [sources.secrets.{name}] must be absolute; got \
       '{destination}'"
    )));
  }

  let mode = match string("mode")? {
    Some(raw) => {
      validate_mode(raw).map_err(|m| {
        fail(format!("'mode' in [sources.secrets.{name}]: {m}"))
      })?;
      raw.trim().to_owned()
    },
    None => DEFAULT_SECRET_MODE.to_owned(),
  };

  let upload_at = match string("upload-at")?.or(string("upload_at")?) {
    Some(raw) => {
      UploadAt::from_config_str(raw).ok_or_else(|| {
        fail(format!(
          "'upload_at' in [sources.secrets.{name}] must be one of {}; got \
           '{raw}'",
          UploadAt::ALL.join(", ")
        ))
      })?
    },
    None => UploadAt::default(),
  };

  let owner = string("owner")?
    .map(str::trim)
    .filter(|o| !o.is_empty())
    .unwrap_or(DEFAULT_SECRET_OWNER)
    .to_owned();
  let group = string("group")?
    .map(str::trim)
    .filter(|g| !g.is_empty())
    .map(ToOwned::to_owned);

  Ok(SecretSpec {
    name: name.to_owned(),
    source: PathBuf::from(source.trim()),
    destination: PathBuf::from(destination.trim()),
    owner,
    group,
    mode,
    upload_at,
  })
}

/// Reject a mode string `chmod` would not accept.
///
/// # Errors
///
/// Returns the reason, without a location prefix.
pub fn validate_mode(raw: &str) -> Result<(), String> {
  let trimmed = raw.trim();
  if trimmed.is_empty() {
    return Err("must not be empty".to_owned());
  }
  if !(3..=4).contains(&trimmed.len())
    || !trimmed.bytes().all(|b| (b'0'..=b'7').contains(&b))
  {
    return Err(format!(
      "must be a 3- or 4-digit octal string such as 0400; got '{trimmed}'"
    ));
  }
  Ok(())
}

/// Merge two secret sets, letting `high` win per name.
///
/// Per *name*, not per set: two config files each declaring a different
/// secret both contribute, which is what makes a machine-local stanza
/// usable alongside a repo-wide one.
#[must_use]
pub fn merge_secrets(
  high: &BTreeMap<String, SecretSpec>,
  low: &BTreeMap<String, SecretSpec>,
) -> BTreeMap<String, SecretSpec> {
  let mut merged = low.clone();
  for (name, spec) in high {
    merged.insert(name.clone(), spec.clone());
  }
  merged
}

/// Names declared both here and by a Nix secret module on the target.
///
/// `xi push` warns on each and defers to the module: agenix and sops-nix
/// put the secret in the activation script, so xi uploading the same name
/// out-of-band would race the module for the destination path.
#[must_use]
pub fn conflicting_names(
  declared: &BTreeMap<String, SecretSpec>,
  module_declared: &BTreeSet<String>,
) -> Vec<String> {
  declared
    .keys()
    .filter(|name| module_declared.contains(name.as_str()))
    .cloned()
    .collect()
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  fn parse(raw: &str) -> Result<BTreeMap<String, SecretSpec>, ConfigError> {
    let doc = toml_edit::Document::parse(raw).expect("valid toml");
    let table = doc
      .get("sources")
      .and_then(toml_edit::Item::as_table)
      .and_then(|t| t.get("secrets"))
      .and_then(toml_edit::Item::as_table)
      .expect("secrets table");
    parse_secrets_table(Path::new(".xi.toml"), raw, table)
  }

  #[test]
  fn defaults_fill_the_optional_keys() {
    let secrets = parse(
      r#"
        [sources.secrets.key]
        source = "./key"
        destination = "/run/keys/key"
      "#,
    )
    .expect("parses");

    let key = secrets.get("key").expect("key stanza");
    assert_eq!(key.mode, DEFAULT_SECRET_MODE);
    assert_eq!(key.owner, DEFAULT_SECRET_OWNER);
    assert_eq!(key.group, None);
    assert_eq!(key.upload_at, UploadAt::PreActivate);
  }

  #[test]
  fn both_upload_at_spellings_parse() {
    for spelling in ["post_activate", "post-activate"] {
      let secrets = parse(&format!(
        r#"
          [sources.secrets.key]
          source = "./key"
          destination = "/run/keys/key"
          upload-at = "{spelling}"
        "#
      ))
      .expect("parses");
      assert_eq!(
        secrets.get("key").expect("key").upload_at,
        UploadAt::PostActivate
      );
    }
  }

  #[test]
  fn a_relative_destination_is_rejected() {
    let err = parse(
      r#"
        [sources.secrets.key]
        source = "./key"
        destination = "run/keys/key"
      "#,
    )
    .expect_err("relative destination");
    assert!(format!("{err}").contains("must be absolute"), "got: {err}");
  }

  #[test]
  fn an_unknown_key_is_rejected_with_a_line() {
    let err = parse(
      r#"
        [sources.secrets.key]
        source = "./key"
        destination = "/run/keys/key"
        onwer = "root"
      "#,
    )
    .expect_err("typo");
    let ConfigError::File { line, message, .. } = err else {
      panic!("expected a file error")
    };
    assert!(message.contains("unknown key 'onwer'"), "got: {message}");
    assert!(line > 1, "the error should point at the stanza, got {line}");
  }

  #[test]
  fn a_non_octal_mode_is_rejected() {
    let err = parse(
      r#"
        [sources.secrets.key]
        source = "./key"
        destination = "/run/keys/key"
        mode = "rw-------"
      "#,
    )
    .expect_err("bad mode");
    assert!(format!("{err}").contains("octal"), "got: {err}");
  }

  #[test]
  fn merge_is_per_name_not_per_set() {
    let high = parse(
      r#"
        [sources.secrets.a]
        source = "./a-high"
        destination = "/run/keys/a"
      "#,
    )
    .expect("parses");
    let low = parse(
      r#"
        [sources.secrets.a]
        source = "./a-low"
        destination = "/run/keys/a"

        [sources.secrets.b]
        source = "./b"
        destination = "/run/keys/b"
      "#,
    )
    .expect("parses");

    let merged = merge_secrets(&high, &low);
    assert_eq!(merged.len(), 2, "b must survive from the lower rung");
    assert_eq!(
      merged.get("a").expect("a").source,
      PathBuf::from("./a-high"),
      "the higher rung wins for a shared name"
    );
  }

  #[test]
  fn a_relative_source_anchors_to_the_config_dir() {
    let secrets = parse(
      r#"
        [sources.secrets.key]
        source = "./key"
        destination = "/run/keys/key"
      "#,
    )
    .expect("parses");
    let resolved = secrets
      .get("key")
      .expect("key")
      .source_from(Path::new("/srv/cfg"));
    assert_eq!(resolved, PathBuf::from("/srv/cfg/./key"));
  }

  #[test]
  fn module_declared_names_are_reported_as_conflicts() {
    let secrets = parse(
      r#"
        [sources.secrets.ssh-key]
        source = "./key"
        destination = "/run/keys/key"

        [sources.secrets.other]
        source = "./other"
        destination = "/run/keys/other"
      "#,
    )
    .expect("parses");
    let module = BTreeSet::from(["ssh-key".to_owned()]);
    assert_eq!(conflicting_names(&secrets, &module), vec!["ssh-key"]);
  }
}
