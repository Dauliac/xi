use std::{
  env, fs,
  io::{self, Write},
  os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
  path::{Path, PathBuf},
};

use color_eyre::{
  Result,
  eyre::{Context, bail},
};
use toml_edit::DocumentMut;

const CONFIG_ENV: &str = "XI_CONFIG";
const CONFIG_FILE: &str = "config.toml";

#[derive(Debug)]
pub struct ConfigStore {
  path: PathBuf,
  document: DocumentMut,
}

/// Typed configuration loaded from `config.toml`.
#[derive(Debug, Clone, Default)]
pub struct Config {
  pub cache: CacheConfig,
  pub develop: DevelopConfig,
  pub build: BuildConfig,
  pub locate: LocateConfig,
}

/// Development shell settings from the `[develop]` section.
#[derive(Debug, Clone)]
pub struct DevelopConfig {
  /// Seconds between eval attempts (default: 5).
  pub eval_interval: u64,
  /// Extra file patterns to watch beyond `*.nix` and `flake.lock`.
  /// Example: `["*.yaml", "version.txt"]`
  pub watch_extra: Vec<String>,
}

impl Default for DevelopConfig {
  fn default() -> Self {
    Self {
      eval_interval: 5,
      watch_extra: Vec::new(),
    }
  }
}

/// Build settings from the `[build]` section.
#[derive(Debug, Clone)]
pub struct BuildConfig {
  /// Use nix-output-monitor (default: true).
  /// Set to false to globally disable nom (same as `--no-nom` / `XI_NO_NOM=1`).
  pub nom: bool,
  /// CI/build-all backend: "auto", "devour-flake", or "nix-fast-build".
  /// Defaults to devour-flake (always available). The xi Nix modules
  /// override this to "nix-fast-build" when `nixFastBuild.enable = true`.
  /// Same as `--backend` / `XI_CI_BACKEND`.
  pub ci_backend: String,
  /// Display tracebacks on errors (same as `--show-trace` / `XI_SHOW_TRACE`).
  pub show_trace: bool,
  /// Continue building despite errors (same as `--keep-going` / `XI_KEEP_GOING`).
  pub keep_going: bool,
  /// Allow impure builds (same as `--impure` / `XI_IMPURE`).
  pub impure: bool,
  /// Accept configuration from flakes (same as `--accept-flake-config` /
  /// `XI_ACCEPT_FLAKE_CONFIG`).
  pub accept_flake_config: bool,
  /// Build without internet access (same as `--offline` / `XI_OFFLINE`).
  pub offline: bool,
  /// Number of concurrent Nix jobs (same as `--max-jobs` / `XI_MAX_JOBS`).
  pub max_jobs: Option<usize>,
  /// Timeout in seconds for substituter connections (same as nix `connect-timeout`
  /// option). Prevents long hangs when substituters are unreachable.
  /// Default: 5. Set to 0 to use nix's default (no timeout).
  pub connect_timeout: Option<u64>,
}

impl Default for BuildConfig {
  fn default() -> Self {
    Self {
      nom: true,
      ci_backend: "devour-flake".to_string(),
      show_trace: false,
      keep_going: false,
      impure: false,
      accept_flake_config: false,
      offline: false,
      max_jobs: None,
      connect_timeout: Some(5),
    }
  }
}

/// Locate mode settings from the `[locate]` section.
///
/// ```toml
/// [locate]
/// cache_level = 2
/// ```
#[derive(Debug, Clone)]
pub struct LocateConfig {
  /// Cache level: 0=disabled, 1=choice only, 2=full (default).
  pub cache_level: u8,
}

impl Default for LocateConfig {
  fn default() -> Self {
    Self { cache_level: 2 }
  }
}

/// Binary cache push settings from the `[cache]` section.
///
/// ```toml
/// [cache]
/// async_push = true
/// queue_max_size = 100
/// queue_expiry_days = 7
/// queue_drain_interval = 300
///
/// [cache.my-s3]
/// push_url = "s3://bucket?region=eu-west-3"
/// signing_key = "/path/to/key"
///
/// [cache.cachix]
/// push_command = ["cachix", "push", "mycache"]
/// ```
#[derive(Debug, Clone, Default)]
pub struct CacheConfig {
  /// Named cache targets from `[cache.<name>]` sub-tables.
  pub targets: Vec<xi_core::args::CacheTarget>,
  /// Whether to push asynchronously in the background.
  pub async_push: bool,
  /// Queue configuration.
  pub queue: xi_core::cache_queue::QueueConfig,
}

impl ConfigStore {
  /// Load xi configuration from the default path.
  ///
  /// # Errors
  ///
  /// Returns an error when the default path cannot be determined, the file
  /// cannot be read, or the TOML document is malformed.
  pub fn load_default() -> Result<Self> {
    Self::load_from(default_config_path()?)
  }

  /// Load xi configuration from a specific path.
  ///
  /// Missing files are treated as an empty configuration and are only created
  /// when [`Self::save`] is called.
  ///
  /// # Errors
  ///
  /// Returns an error when the file cannot be read or parsed.
  pub fn load_from(path: impl Into<PathBuf>) -> Result<Self> {
    let path = path.into();
    let document = match fs::read_to_string(&path) {
      Ok(raw) => parse_document(&path, &raw)?,
      Err(err) if err.kind() == io::ErrorKind::NotFound => DocumentMut::new(),
      Err(err) => {
        return Err(err)
          .with_context(|| format!("failed to read {}", path.display()));
      },
    };

    Ok(Self { path, document })
  }

  #[must_use]
  pub fn path(&self) -> &Path {
    &self.path
  }

  /// Return the typed view of the known xi configuration fields.
  ///
  /// # Errors
  ///
  /// Returns an error when a known field is present with the wrong type or
  /// when unknown keys are found in the configuration.
  pub fn config(&self) -> Result<Config> {
    self.validate_unknown_keys()?;
    let cache = self.read_cache_config();
    let develop = self.read_develop_config();
    let build = self.read_build_config();
    let locate = self.read_locate_config();
    Ok(Config {
      cache,
      develop,
      build,
      locate,
    })
  }

  /// Reject unknown keys at every level of the configuration.
  fn validate_unknown_keys(&self) -> Result<()> {
    const TOP_LEVEL: &[&str] = &["cache", "develop", "build", "locate"];
    const CACHE_SCALAR: &[&str] = &[
      "async_push",
      "queue_max_size",
      "queue_expiry_days",
      "queue_drain_interval",
    ];
    const CACHE_TARGET: &[&str] = &["push_url", "signing_key", "push_command"];
    const DEVELOP: &[&str] = &["eval_interval", "watch_extra"];
    const BUILD: &[&str] = &[
      "nom",
      "ci_backend",
      "show_trace",
      "keep_going",
      "impure",
      "accept_flake_config",
      "offline",
      "max_jobs",
      "connect_timeout",
    ];

    if let Some(root) = self.document.as_table().into() {
      reject_unknown("", root, TOP_LEVEL)?;
    }

    if let Some(table) = self
      .document
      .get("cache")
      .and_then(toml_edit::Item::as_table)
    {
      // Scalar keys + any sub-table name are allowed at [cache] level.
      let unknown: Vec<&str> = table
        .iter()
        .filter(|(k, v)| !v.is_table() && !CACHE_SCALAR.contains(k))
        .map(|(k, _)| k)
        .collect();
      if !unknown.is_empty() {
        bail!(
          "unknown key(s) in [cache]: {}.\n\
           Hint: cache targets must be sub-tables, e.g. [cache.my-s3]",
          unknown.join(", ")
        );
      }

      for (name, value) in table {
        if let Some(sub) = value.as_table() {
          reject_unknown(&format!("cache.{name}"), sub, CACHE_TARGET)?;
        }
      }
    }

    if let Some(table) = self
      .document
      .get("develop")
      .and_then(toml_edit::Item::as_table)
    {
      reject_unknown("develop", table, DEVELOP)?;
    }

    if let Some(table) = self
      .document
      .get("build")
      .and_then(toml_edit::Item::as_table)
    {
      reject_unknown("build", table, BUILD)?;
    }

    if let Some(table) = self
      .document
      .get("locate")
      .and_then(toml_edit::Item::as_table)
    {
      const LOCATE: &[&str] = &["cache_level"];
      reject_unknown("locate", table, LOCATE)?;
    }

    Ok(())
  }

  fn read_develop_config(&self) -> DevelopConfig {
    let Some(table) = self.document.get("develop") else {
      return DevelopConfig::default();
    };

    let eval_interval = table
      .get("eval_interval")
      .and_then(toml_edit::Item::as_integer)
      .map_or(5, |v| v.max(1).cast_unsigned());

    let watch_extra = table
      .get("watch_extra")
      .and_then(toml_edit::Item::as_array)
      .map(|arr| {
        arr
          .iter()
          .filter_map(toml_edit::Value::as_str)
          .map(String::from)
          .collect()
      })
      .unwrap_or_default();

    DevelopConfig {
      eval_interval,
      watch_extra,
    }
  }

  fn read_build_config(&self) -> BuildConfig {
    let Some(table) = self.document.get("build") else {
      return BuildConfig::default();
    };

    let nom = table
      .get("nom")
      .and_then(toml_edit::Item::as_bool)
      .unwrap_or(true);

    let ci_backend = table
      .get("ci_backend")
      .and_then(toml_edit::Item::as_str)
      .unwrap_or("devour-flake")
      .to_string();

    let show_trace = table
      .get("show_trace")
      .and_then(toml_edit::Item::as_bool)
      .unwrap_or(false);

    let keep_going = table
      .get("keep_going")
      .and_then(toml_edit::Item::as_bool)
      .unwrap_or(false);

    let impure = table
      .get("impure")
      .and_then(toml_edit::Item::as_bool)
      .unwrap_or(false);

    let accept_flake_config = table
      .get("accept_flake_config")
      .and_then(toml_edit::Item::as_bool)
      .unwrap_or(false);

    let offline = table
      .get("offline")
      .and_then(toml_edit::Item::as_bool)
      .unwrap_or(false);

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let max_jobs = table
      .get("max_jobs")
      .and_then(toml_edit::Item::as_integer)
      .map(|v| v.max(0) as usize);

    // Priority: XI_CONNECT_TIMEOUT env > config.toml > default (5)
    let connect_timeout = env::var("XI_CONNECT_TIMEOUT")
      .ok()
      .and_then(|v| v.parse::<u64>().ok())
      .or_else(|| {
        table
          .get("connect_timeout")
          .and_then(toml_edit::Item::as_integer)
          .map(|v| v.max(0).cast_unsigned())
      })
      .map_or(Some(5), |v| if v == 0 { None } else { Some(v) });

    BuildConfig {
      nom,
      ci_backend,
      show_trace,
      keep_going,
      impure,
      accept_flake_config,
      offline,
      max_jobs,
      connect_timeout,
    }
  }

  fn read_locate_config(&self) -> LocateConfig {
    let Some(table) = self.document.get("locate") else {
      return LocateConfig::default();
    };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cache_level = table
      .get("cache_level")
      .and_then(toml_edit::Item::as_integer)
      .map_or(2, |v| v.clamp(0, 2) as u8);

    LocateConfig { cache_level }
  }

  fn read_cache_config(&self) -> CacheConfig {
    let Some(cache_item) = self.document.get("cache") else {
      return CacheConfig::default();
    };

    let defaults = xi_core::cache_queue::QueueConfig::default();

    let async_push = cache_item
      .get("async_push")
      .and_then(toml_edit::Item::as_bool)
      .unwrap_or(false);

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let queue_max_size = cache_item
      .get("queue_max_size")
      .and_then(toml_edit::Item::as_integer)
      .map_or(defaults.max_size, |v| v.max(1) as usize);

    let queue_expiry_secs = cache_item
      .get("queue_expiry_days")
      .and_then(toml_edit::Item::as_integer)
      .map_or(defaults.expiry_secs, |v| {
        v.max(1).cast_unsigned() * 24 * 3600
      });

    let queue_drain_interval = cache_item
      .get("queue_drain_interval")
      .and_then(toml_edit::Item::as_integer)
      .map_or(defaults.drain_interval_secs, |v| v.max(10).cast_unsigned());

    let mut targets = Vec::new();

    // Parse named sub-tables: [cache.<name>]
    if let Some(table) = cache_item.as_table() {
      for (key, value) in table {
        if let Some(sub) = value.as_table() {
          targets.push(Self::parse_cache_target(key, sub));
        }
      }
    }

    CacheConfig {
      targets,
      async_push,
      queue: xi_core::cache_queue::QueueConfig {
        max_size: queue_max_size,
        expiry_secs: queue_expiry_secs,
        drain_interval_secs: queue_drain_interval,
      },
    }
  }

  /// Parse a named cache target from a `[cache.<name>]` sub-table.
  fn parse_cache_target(
    name: &str,
    table: &toml_edit::Table,
  ) -> xi_core::args::CacheTarget {
    let push_url = table
      .get("push_url")
      .and_then(toml_edit::Item::as_str)
      .map(String::from);

    let signing_key = table
      .get("signing_key")
      .and_then(toml_edit::Item::as_str)
      .map(String::from);

    let push_command = table
      .get("push_command")
      .and_then(toml_edit::Item::as_array)
      .map(|arr| {
        arr
          .iter()
          .filter_map(toml_edit::Value::as_str)
          .map(String::from)
          .collect()
      })
      .unwrap_or_default();

    xi_core::args::CacheTarget {
      name: name.to_string(),
      push_url,
      signing_key,
      push_command,
    }
  }

  /// Save the document, creating parent directories as needed.
  ///
  /// # Errors
  ///
  /// Returns an error when the parent directory cannot be created or the file
  /// cannot be written.
  pub fn save(&self) -> Result<()> {
    write_private(&self.path, self.document.to_string().as_bytes())
  }
}

/// Resolve the path to xi configuration.
///
/// # Errors
///
/// Returns an error when `XI_CONFIG` is empty or no home directory can be
/// determined for the fallback path.
pub fn default_config_path() -> Result<PathBuf> {
  if let Some(path) = env::var_os(CONFIG_ENV) {
    if path.is_empty() {
      bail!("{CONFIG_ENV} is set but empty");
    }

    return Ok(PathBuf::from(path));
  }

  if let Some(config_home) = non_empty_var("XDG_CONFIG_HOME") {
    return Ok(PathBuf::from(config_home).join("xi").join(CONFIG_FILE));
  }

  if let Some(home) = non_empty_var("HOME") {
    return Ok(
      PathBuf::from(home)
        .join(".config")
        .join("xi")
        .join(CONFIG_FILE),
    );
  }

  bail!("could not determine xi configuration path; set {CONFIG_ENV}")
}

fn reject_unknown(
  section: &str,
  table: &toml_edit::Table,
  known: &[&str],
) -> Result<()> {
  let unknown: Vec<&str> = table
    .iter()
    .map(|(k, _)| k)
    .filter(|k| !known.contains(k))
    .collect();
  if unknown.is_empty() {
    return Ok(());
  }
  let label = if section.is_empty() {
    "top level".to_string()
  } else {
    format!("[{section}]")
  };
  bail!("unknown key(s) in {label}: {}", unknown.join(", "));
}

fn parse_document(path: &Path, raw: &str) -> Result<DocumentMut> {
  raw.parse::<DocumentMut>().with_context(|| {
    format!("failed to parse xi configuration at {}", path.display())
  })
}

fn non_empty_var(name: &str) -> Option<std::ffi::OsString> {
  env::var_os(name).filter(|value| !value.is_empty())
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
  if let Some(parent) = path.parent() {
    create_config_dir(parent)?;
  }

  let mut options = fs::OpenOptions::new();
  options.create(true).write(true).truncate(true).mode(0o600);

  let mut file = options
    .open(path)
    .with_context(|| format!("failed to open {}", path.display()))?;
  file
    .write_all(contents)
    .with_context(|| format!("failed to write {}", path.display()))?;

  set_user_only_file(path)?;
  Ok(())
}

fn create_config_dir(path: &Path) -> Result<()> {
  let mut builder = fs::DirBuilder::new();
  builder.recursive(true).mode(0o700);
  builder
    .create(path)
    .with_context(|| format!("failed to create {}", path.display()))
}

fn set_user_only_file(path: &Path) -> Result<()> {
  fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(
    || format!("failed to set private permissions on {}", path.display()),
  )
}

#[cfg(test)]
mod tests {
  use std::{env, fs, os::unix::fs::PermissionsExt};

  use color_eyre::Result;
  use serial_test::serial;
  use tempfile::tempdir;

  use super::{ConfigStore, default_config_path};

  struct EnvGuard {
    key: &'static str,
    value: Option<std::ffi::OsString>,
  }

  impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
      let guard = Self {
        key,
        value: env::var_os(key),
      };
      unsafe {
        env::set_var(key, value);
      }
      guard
    }

    fn remove(key: &'static str) -> Self {
      let guard = Self {
        key,
        value: env::var_os(key),
      };
      unsafe {
        env::remove_var(key);
      }
      guard
    }
  }

  impl Drop for EnvGuard {
    fn drop(&mut self) {
      unsafe {
        if let Some(value) = &self.value {
          env::set_var(self.key, value);
        } else {
          env::remove_var(self.key);
        }
      }
    }
  }

  #[test]
  fn missing_file_loads_as_default_config() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml");

    let store = ConfigStore::load_from(&path)?;

    let _config = store.config()?;
    assert!(!path.exists());
    Ok(())
  }

  #[test]
  fn save_preserves_comments_and_unknown_fields() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml");
    fs::write(&path, "# keep me\n[unknown]\nvalue = 1\n")?;

    let store = ConfigStore::load_from(&path)?;
    store.save()?;

    let written = fs::read_to_string(&path)?;
    assert!(written.contains("# keep me"));
    assert!(written.contains("[unknown]"));
    assert!(written.contains("value = 1"));
    Ok(())
  }

  #[test]
  fn save_creates_private_file() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("xi").join("config.toml");
    let store = ConfigStore::load_from(&path)?;
    store.save()?;

    let mode = fs::metadata(&path)?.permissions().mode();
    assert_eq!(0, mode & 0o077);
    Ok(())
  }

  #[test]
  #[serial]
  fn xi_config_overrides_default_path() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("custom.toml");
    let _config = EnvGuard::set("XI_CONFIG", &path);

    assert_eq!(path, default_config_path()?);
    Ok(())
  }

  #[test]
  #[serial]
  fn xdg_config_home_falls_back_when_no_override_exists() -> Result<()> {
    let dir = tempdir()?;
    let _config = EnvGuard::remove("XI_CONFIG");
    let _xdg = EnvGuard::set("XDG_CONFIG_HOME", dir.path());

    assert_eq!(
      dir.path().join("xi").join("config.toml"),
      default_config_path()?
    );
    Ok(())
  }

  #[test]
  fn build_config_defaults_when_missing() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml");

    let store = ConfigStore::load_from(&path)?;
    let config = store.config()?;

    assert!(config.build.nom);
    assert!(!config.build.show_trace);
    assert!(!config.build.keep_going);
    assert!(!config.build.impure);
    assert!(!config.build.accept_flake_config);
    assert!(!config.build.offline);
    assert!(config.build.max_jobs.is_none());
    assert_eq!(config.build.connect_timeout, Some(5));
    Ok(())
  }

  #[test]
  fn build_config_parses_all_fields() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml");
    fs::write(
      &path,
      "[build]\n\
       nom = false\n\
       show_trace = true\n\
       keep_going = true\n\
       impure = true\n\
       accept_flake_config = true\n\
       offline = true\n\
       max_jobs = 8\n\
       connect_timeout = 10\n",
    )?;

    let store = ConfigStore::load_from(&path)?;
    let config = store.config()?;

    assert!(!config.build.nom);
    assert!(config.build.show_trace);
    assert!(config.build.keep_going);
    assert!(config.build.impure);
    assert!(config.build.accept_flake_config);
    assert!(config.build.offline);
    assert_eq!(config.build.max_jobs, Some(8));
    assert_eq!(config.build.connect_timeout, Some(10));
    Ok(())
  }

  #[test]
  fn connect_timeout_zero_disables() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml");
    fs::write(&path, "[build]\nconnect_timeout = 0\n")?;

    let store = ConfigStore::load_from(&path)?;
    let config = store.config()?;
    assert_eq!(config.build.connect_timeout, None);
    Ok(())
  }

  #[test]
  fn rejects_unknown_top_level_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[unknown]\nvalue = 1\n").unwrap();

    let store = ConfigStore::load_from(&path).unwrap();
    let err = store.config().unwrap_err();
    assert!(
      err.to_string().contains("unknown key(s) in top level"),
      "unexpected error: {err}"
    );
  }

  #[test]
  fn rejects_flat_cache_keys() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
      &path,
      "[cache]\npush_url = \"s3://bucket\"\nsigning_key = \"key\"\n",
    )
    .unwrap();

    let store = ConfigStore::load_from(&path).unwrap();
    let err = store.config().unwrap_err();
    let msg = err.to_string();
    assert!(
      msg.contains("unknown key(s) in [cache]"),
      "unexpected: {msg}"
    );
    assert!(msg.contains("push_url"), "should mention push_url: {msg}");
    assert!(
      msg.contains("sub-tables"),
      "should hint about sub-tables: {msg}"
    );
  }

  #[test]
  fn rejects_unknown_cache_target_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
      &path,
      "[cache.my-s3]\npush_url = \"s3://bucket\"\nbogus = true\n",
    )
    .unwrap();

    let store = ConfigStore::load_from(&path).unwrap();
    let err = store.config().unwrap_err();
    let msg = err.to_string();
    assert!(
      msg.contains("unknown key(s) in [cache.my-s3]"),
      "unexpected: {msg}"
    );
  }

  #[test]
  fn rejects_unknown_build_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[build]\ntypo = true\n").unwrap();

    let store = ConfigStore::load_from(&path).unwrap();
    let err = store.config().unwrap_err();
    assert!(
      err.to_string().contains("unknown key(s) in [build]"),
      "unexpected: {err}"
    );
  }

  #[test]
  fn rejects_unknown_develop_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[develop]\nfoo = 42\n").unwrap();

    let store = ConfigStore::load_from(&path).unwrap();
    let err = store.config().unwrap_err();
    assert!(
      err.to_string().contains("unknown key(s) in [develop]"),
      "unexpected: {err}"
    );
  }

  #[test]
  fn accepts_valid_full_config() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml");
    fs::write(
      &path,
      "[cache]\n\
       async_push = true\n\
       \n\
       [cache.my-s3]\n\
       push_url = \"s3://bucket\"\n\
       signing_key = \"/path/to/key\"\n\
       \n\
       [build]\n\
       nom = false\n\
       \n\
       [develop]\n\
       eval_interval = 10\n",
    )?;

    let store = ConfigStore::load_from(&path)?;
    let config = store.config()?;
    assert!(config.cache.async_push);
    assert_eq!(config.cache.targets.len(), 1);
    assert_eq!(config.cache.targets[0].name, "my-s3");
    Ok(())
  }
}
