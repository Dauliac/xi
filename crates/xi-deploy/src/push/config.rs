//! The `[push]` table of `.xi.toml`: the declarative inventory
//! (`[[push.hosts]]`) and the `remote-build`/`retry-attempts` config
//! rungs.
//!
//! Read from the same place [`super::context::resolve_context`] reads
//! the rest of `.xi.toml` from: the current directory, literally
//! (`xi_core::sources::args::resolve_layers` does the same for
//! `[sources]`). Unlike `[sources]`, `[push]` is not part of that
//! capability's precedence ladder - it is parsed here, in the push
//! crate, because nothing outside `xi push` ever needs an inventory
//! entry or the `remote-build`/`retry-attempts` defaults.
//!
//! The inventory describes *other* machines (the proposal's own
//! distinction from every other `[sources.*]` key, which describes the
//! machine it is read on), so it is read once, from the operator's own
//! `.xi.toml`, with no per-scope ladder of its own: there is no
//! "Nix-module-written" rung for an inventory, because a target has no
//! reason to know its own entry in somebody else's host list.

use std::{path::Path, str::FromStr};

use color_eyre::{Result, eyre::eyre};

use super::args::{PushBackend, SourcesTransport};

/// Every key the `[push]` table accepts, besides the `hosts` sub-table.
pub const PUSH_KEYS: &[&str] = &[
  "remote-build",
  "backend",
  "retry-attempts",
  "sources-transport",
];

/// Sub-tables the `[push]` table accepts, alongside [`PUSH_KEYS`].
pub const PUSH_TABLE_KEYS: &[&str] = &["hosts"];

/// Every key a `[[push.hosts]]` entry accepts.
pub const PUSH_HOST_KEYS: &[&str] = &[
  "name",
  "ssh",
  "remote-build",
  "backend",
  "retry-attempts",
  "sources-transport",
];

/// One `[[push.hosts]]` entry: an inventory name and the SSH target it
/// resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushHost {
  /// The name `xi push <name>` matches against.
  pub name:              String,
  /// `[user@]hostname[:port]` to connect to for this host.
  pub ssh:               String,
  /// Per-host override of the `remote-build` rung.
  pub remote_build:      Option<bool>,
  /// Per-host override of the `backend` rung.
  pub backend:           Option<PushBackend>,
  /// Per-host override of the `retry-attempts` rung.
  pub retry_attempts:    Option<u32>,
  /// Per-host override of the `sources-transport` rung.
  pub sources_transport: Option<SourcesTransport>,
}

/// The parsed `[push]` table: the `remote-build` and `backend` defaults
/// and the flat host inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushConfig {
  /// `.xi.toml [push] remote-build`, the middle rung of the
  /// build-location ladder.
  pub remote_build:      Option<bool>,
  /// `.xi.toml [push] backend`, the middle rung of the backend ladder.
  pub backend:           Option<PushBackend>,
  /// `.xi.toml [push] retry-attempts`, the table-level rung of the
  /// retry-attempts ladder.
  pub retry_attempts:    Option<u32>,
  /// `.xi.toml [push] sources-transport`, the table-level rung of the
  /// sources-transport ladder.
  pub sources_transport: Option<SourcesTransport>,
  /// `[[push.hosts]]` entries, in file order.
  pub hosts:             Vec<PushHost>,
}

impl PushConfig {
  /// True when this table set nothing at all.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    *self == Self::default()
  }

  /// Look up `name` in [`Self::hosts`].
  #[must_use]
  pub fn host(&self, name: &str) -> Option<&PushHost> {
    self.hosts.iter().find(|host| host.name == name)
  }

  /// Parse the `[push]` table out of a `.xi.toml` document.
  ///
  /// A document with no `[push]` table is not an error: it yields an
  /// empty config, the same way a `.xi.toml` predating this capability
  /// has no `[sources]` table either.
  ///
  /// # Errors
  ///
  /// Returns an error naming `path` and the offending key for a TOML
  /// syntax error, an unknown key anywhere in `[push]` or a
  /// `[[push.hosts]]` entry, a wrongly-typed value, or a host entry
  /// missing `name` or `ssh`.
  pub fn from_toml_str(path: &Path, raw: &str) -> Result<Self> {
    let doc = toml_edit::Document::parse(raw)
      .map_err(|e| eyre!("{}: {e}", path.display()))?;

    let Some(table) = doc.get("push").and_then(toml_edit::Item::as_table)
    else {
      return Ok(Self::default());
    };

    for (key, _) in table {
      if !PUSH_KEYS.contains(&key) && !PUSH_TABLE_KEYS.contains(&key) {
        return Err(eyre!("{}: unknown key '{key}' in [push]", path.display()));
      }
    }

    let remote_build = match table.get("remote-build") {
      None => None,
      Some(item) => {
        Some(item.as_bool().ok_or_else(|| {
          eyre!(
            "{}: 'remote-build' in [push] must be a boolean",
            path.display()
          )
        })?)
      },
    };

    let backend = match table.get("backend") {
      None => None,
      Some(item) => {
        let raw = item.as_str().ok_or_else(|| {
          eyre!("{}: 'backend' in [push] must be a string", path.display())
        })?;
        Some(PushBackend::from_config_str(raw).ok_or_else(|| {
          eyre!(
            "{}: 'backend' in [push] must be one of {}, got '{raw}'",
            path.display(),
            PushBackend::ALL.join(", ")
          )
        })?)
      },
    };

    let retry_attempts = match table.get("retry-attempts") {
      None => None,
      Some(item) => Some(parse_retry_attempts(path, item, "[push]")?),
    };

    let sources_transport = match table.get("sources-transport") {
      None => None,
      Some(item) => {
        let raw = item.as_str().ok_or_else(|| {
          eyre!(
            "{}: 'sources-transport' in [push] must be a string",
            path.display()
          )
        })?;
        Some(
          SourcesTransport::from_str(raw)
            .map_err(|e| eyre!("{}: {e}", path.display()))?,
        )
      },
    };

    let hosts = match table.get("hosts") {
      None => Vec::new(),
      Some(item) => {
        let array = item.as_array_of_tables().ok_or_else(|| {
          eyre!(
            "{}: [push.hosts] must be declared as [[push.hosts]] entries",
            path.display()
          )
        })?;
        array
          .into_iter()
          .map(|entry| parse_host(path, entry))
          .collect::<Result<Vec<_>>>()?
      },
    };

    Ok(Self {
      remote_build,
      backend,
      retry_attempts,
      sources_transport,
      hosts,
    })
  }

  /// Parse the `[push]` table out of a config file on disk.
  ///
  /// A file that does not exist yields an empty config.
  ///
  /// # Errors
  ///
  /// Propagates [`Self::from_toml_str`]'s errors for a file that exists
  /// but is malformed.
  pub fn from_toml_file(path: &Path) -> Result<Self> {
    match std::fs::read_to_string(path) {
      Ok(raw) => Self::from_toml_str(path, &raw),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
      Err(e) => Err(eyre!("{}: {e}", path.display())),
    }
  }
}

/// Parse one `[[push.hosts]]` entry.
fn parse_host(path: &Path, entry: &toml_edit::Table) -> Result<PushHost> {
  for (key, _) in entry {
    if !PUSH_HOST_KEYS.contains(&key) {
      return Err(eyre!(
        "{}: unknown key '{key}' in a [[push.hosts]] entry",
        path.display()
      ));
    }
  }

  let name = entry
    .get("name")
    .and_then(toml_edit::Item::as_str)
    .ok_or_else(|| {
      eyre!(
        "{}: every [[push.hosts]] entry needs a string 'name'",
        path.display()
      )
    })?
    .to_owned();
  let ssh = entry
    .get("ssh")
    .and_then(toml_edit::Item::as_str)
    .ok_or_else(|| {
      eyre!(
        "{}: [[push.hosts]] entry '{name}' needs a string 'ssh'",
        path.display()
      )
    })?
    .to_owned();
  let remote_build = match entry.get("remote-build") {
    None => None,
    Some(item) => {
      Some(item.as_bool().ok_or_else(|| {
        eyre!(
          "{}: 'remote-build' in [[push.hosts]] entry '{name}' must be a \
           boolean",
          path.display()
        )
      })?)
    },
  };

  let backend = match entry.get("backend") {
    None => None,
    Some(item) => {
      let raw = item.as_str().ok_or_else(|| {
        eyre!(
          "{}: 'backend' in [[push.hosts]] entry '{name}' must be a string",
          path.display()
        )
      })?;
      Some(PushBackend::from_config_str(raw).ok_or_else(|| {
        eyre!(
          "{}: 'backend' in [[push.hosts]] entry '{name}' must be one of {}, \
           got '{raw}'",
          path.display(),
          PushBackend::ALL.join(", ")
        )
      })?)
    },
  };
  let retry_attempts = match entry.get("retry-attempts") {
    None => None,
    Some(item) => {
      Some(parse_retry_attempts(
        path,
        item,
        &format!("[[push.hosts]] entry '{name}'"),
      )?)
    },
  };

  let sources_transport = match entry.get("sources-transport") {
    None => None,
    Some(item) => {
      let raw = item.as_str().ok_or_else(|| {
        eyre!(
          "{}: 'sources-transport' in [[push.hosts]] entry '{name}' must be a \
           string",
          path.display()
        )
      })?;
      Some(
        SourcesTransport::from_str(raw)
          .map_err(|e| eyre!("{}: {e}", path.display()))?,
      )
    },
  };

  Ok(PushHost {
    name,
    ssh,
    remote_build,
    backend,
    retry_attempts,
    sources_transport,
  })
}

/// Parse a `retry-attempts` value, shared by the `[push]` table and a
/// `[[push.hosts]]` entry.
///
/// `context` names where the value came from, for the error message -
/// `"[push]"` or `"[[push.hosts]] entry '<name>'"`.
fn parse_retry_attempts(
  path: &Path,
  item: &toml_edit::Item,
  context: &str,
) -> Result<u32> {
  let value = item.as_integer().ok_or_else(|| {
    eyre!(
      "{}: 'retry-attempts' in {context} must be an integer",
      path.display()
    )
  })?;
  u32::try_from(value).map_err(|_| {
    eyre!(
      "{}: 'retry-attempts' in {context} must be a non-negative integer that \
       fits in a u32",
      path.display()
    )
  })
}

/// Environment variable gating the build-location floor flip's
/// one-release opt-in: unset (or anything but `1`/`true`), the floor
/// keeps its pre-flip answer; set, the floor is free to resolve to its
/// new, context-sensitive default. See
/// [`build_floor_remote_opt_in`] and the "build-location floor flip"
/// capability this stages.
pub const BUILD_FLOOR_REMOTE_ENV: &str = "XI_BUILD_FLOOR_REMOTE";

/// Whether the one-release opt-in for the build-location floor flip is
/// active.
///
/// This only answers whether the flip is allowed to take effect yet -
/// it does not itself decide what the floor resolves to. Follows the
/// same boolean-env-var convention used elsewhere in the project (e.g.
/// `XI_UNWRAP` in `xi::proxy`): `"1"` or `"true"` is on, anything else
/// (including unset) is off.
#[must_use]
pub fn build_floor_remote_opt_in() -> bool {
  std::env::var(BUILD_FLOOR_REMOTE_ENV)
    .is_ok_and(|value| value == "1" || value == "true")
}

/// Resolve one value through the five-rung precedence shared by every
/// declarative-first `[push]` setting: an explicit flag, a per-host
/// override, a table-level default, a Nix-module-provided default, and
/// finally a compiled-in floor.
///
/// This is the shape behind [`resolve_remote_build`] and, in a sibling
/// ladder, backend selection - one generalized helper rather than a
/// hand-rolled copy of the same precedence per setting.
#[must_use]
pub fn resolve_ladder<T>(
  cli: Option<T>,
  per_host: Option<T>,
  table_default: Option<T>,
  module_default: Option<T>,
  floor: T,
) -> T {
  cli
    .or(per_host)
    .or(table_default)
    .or(module_default)
    .unwrap_or(floor)
}

/// Resolve the build-location ladder for one target: `--remote-build` >
/// `.xi.toml [push]` (a per-host `[[push.hosts]]` override first, then
/// the table-level default) > the Nix-module-provided default > the
/// compiled-in floor.
///
/// Every `xi push` target is, by construction, some other machine (see
/// this module's own doc comment), so the floor always answers for a
/// genuinely remote target - there is no local-machine case to special
/// case here.
///
/// The floor is `false` (closure: build here, copy the closure in)
/// unless [`build_floor_remote_opt_in`] is active, in which case it is
/// `true` (`--mode sources`: the target builds itself) - the
/// build-location floor flip, staged behind a one-release opt-in because
/// it is the one user-visible default change in this effort. Every
/// higher rung still overrides the floor unchanged either way.
///
/// The caller still has the final word through an explicit `--mode` flag
/// (see [`super::args::PushArgs::effective_mode`]), which this function
/// does not know about: it only resolves what the *ladder itself* says,
/// not whether something further up overrides the ladder outright.
#[must_use]
pub fn resolve_remote_build(
  cli: Option<bool>,
  host_name: &str,
  toml_config: &PushConfig,
  nix_module_remote_build: Option<bool>,
) -> bool {
  let floor = build_floor_remote_opt_in();
  resolve_ladder(
    cli,
    toml_config
      .host(host_name)
      .and_then(|host| host.remote_build),
    toml_config.remote_build,
    nix_module_remote_build,
    floor,
  )
}

/// Resolve the backend ladder for one target: `--backend` >
/// `.xi.toml [push]` (a per-host `[[push.hosts]]` override first, then
/// the table-level default) > the Nix-module-provided default >
/// `detect_backend`'s auto-detect floor.
///
/// The floor is always [`PushBackend::Auto`]: this function does not
/// probe the flake itself, it only decides which configured rung (if
/// any) preempts that probe. A caller passing `PushBackend::Auto` as
/// `cli` is saying "no explicit `--backend` flag was given" - the same
/// convention `--backend`'s own `clap` default already uses - so it is
/// treated as absent rather than as an explicit override.
#[must_use]
pub fn resolve_backend(
  cli: PushBackend,
  host_name: &str,
  toml_config: &PushConfig,
  nix_module_backend: Option<PushBackend>,
) -> PushBackend {
  let cli = (cli != PushBackend::Auto).then_some(cli);
  resolve_ladder(
    cli,
    toml_config.host(host_name).and_then(|host| host.backend),
    toml_config.backend,
    nix_module_backend,
    PushBackend::Auto,
  )
}

/// Resolve the retry-attempts ladder for one target: `--retry` >
/// `.xi.toml [push]` (a per-host `[[push.hosts]]` override first, then
/// the table-level default) > the compiled floor of `3`.
///
/// Mirrors [`resolve_remote_build`]'s precedence shape exactly, minus the
/// Nix-module rung: retry attempts have no module-provided default to
/// consult.
#[must_use]
pub fn resolve_retry_attempts(
  cli: Option<u32>,
  host_name: &str,
  toml_config: &PushConfig,
) -> u32 {
  if let Some(attempts) = cli {
    return attempts;
  }
  if let Some(per_host) = toml_config
    .host(host_name)
    .and_then(|host| host.retry_attempts)
  {
    return per_host;
  }
  if let Some(table_default) = toml_config.retry_attempts {
    return table_default;
  }
  3
}

/// Resolve the sources-transport ladder for one target: `--sources-transport`
/// outranks `.xi.toml [push]` (a per-host `[[push.hosts]]` override first,
/// then the table-level default), which outranks the compiled floor
/// `operator-copy`.
///
/// Shaped identically to [`resolve_remote_build`], minus a Nix-module
/// rung: nothing writes a Nix-module default for this setting, so the
/// floor is the last rung rather than a fourth one. `target-pull` is
/// never guessed - an operator opts a target into it explicitly once
/// they have verified it can reach the configured remote directly.
#[must_use]
pub fn resolve_sources_transport(
  cli: Option<SourcesTransport>,
  host_name: &str,
  toml_config: &PushConfig,
) -> SourcesTransport {
  if let Some(flag) = cli {
    return flag;
  }
  if let Some(per_host) = toml_config
    .host(host_name)
    .and_then(|host| host.sources_transport)
  {
    return per_host;
  }
  if let Some(table_default) = toml_config.sources_transport {
    return table_default;
  }
  SourcesTransport::OperatorCopy
}

/// Rewrite `targets`' SSH address from a matching `[[push.hosts]]` entry.
///
/// A target whose name is not in `inventory` is untouched: that is the
/// proposal's "a positional that is not a known inventory name is still
/// read as a raw SSH target directly" rule - `discover`'s own default
/// (`ssh_host == name`) already *is* "read as a raw SSH target", so
/// there is nothing more for this function to do in that case.
#[must_use]
pub fn apply_inventory(
  mut targets: Vec<super::discover::PushTarget>,
  inventory: &PushConfig,
) -> Vec<super::discover::PushTarget> {
  for target in &mut targets {
    if let Some(host) = inventory.host(&target.addr.name) {
      target.addr.ssh_host = host.ssh.clone();
    }
  }
  targets
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::env;

  use serial_test::serial;
  use tempfile::TempDir;

  use super::*;
  use crate::{
    push::{discover::TargetSource, engine::TargetAddr},
    target::ProfileKind,
  };

  /// Sets an environment variable for the lifetime of the guard,
  /// restoring (or removing) it on drop - safe for `#[serial]` tests
  /// that read process-global env vars.
  struct EnvGuard {
    key:   &'static str,
    value: Option<std::ffi::OsString>,
  }

  impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
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

  fn toml_config(body: &str) -> PushConfig {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, body).expect("write");
    PushConfig::from_toml_file(&path).expect("parse")
  }

  #[test]
  fn an_absent_push_table_is_empty() {
    assert!(
      toml_config("[sources]\nremote = \"https://example\"\n").is_empty()
    );
  }

  #[test]
  fn a_missing_file_is_empty() {
    let config = PushConfig::from_toml_file(Path::new(
      "/nonexistent/path/that/does/not/exist/.xi.toml",
    ))
    .expect("missing file is not an error");
    assert!(config.is_empty());
  }

  #[test]
  fn remote_build_parses_as_a_boolean() {
    let config = toml_config("[push]\nremote-build = true\n");
    assert_eq!(config.remote_build, Some(true));
  }

  #[test]
  fn backend_parses_as_an_enum_string() {
    let config = toml_config("[push]\nbackend = \"colmena\"\n");
    assert_eq!(config.backend, Some(PushBackend::Colmena));
  }

  #[test]
  fn an_invalid_backend_value_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[push]\nbackend = \"openstack\"\n").expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("invalid backend");
    assert!(format!("{err}").contains("backend"), "got: {err}");
  }

  #[test]
  fn an_unknown_push_key_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[push]\ntypo-key = true\n").expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("unknown key");
    assert!(format!("{err}").contains("typo-key"), "got: {err}");
  }

  #[test]
  fn a_host_array_of_tables_parses_in_order() {
    let config = toml_config(
      "[[push.hosts]]\nname = \"web1\"\nssh = \
       \"deploy@10.0.0.1\"\n\n[[push.hosts]]\nname = \"web2\"\nssh = \
       \"deploy@10.0.0.2:2222\"\nremote-build = true\nbackend = \
       \"deploy-rs\"\n",
    );
    assert_eq!(config.hosts.len(), 2);
    assert_eq!(config.hosts[0].name, "web1");
    assert_eq!(config.hosts[0].ssh, "deploy@10.0.0.1");
    assert_eq!(config.hosts[0].remote_build, None);
    assert_eq!(config.hosts[0].backend, None);
    assert_eq!(config.hosts[1].ssh, "deploy@10.0.0.2:2222");
    assert_eq!(config.hosts[1].remote_build, Some(true));
    assert_eq!(config.hosts[1].backend, Some(PushBackend::DeployRs));
  }

  #[test]
  fn an_invalid_host_backend_value_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(
      &path,
      "[[push.hosts]]\nname = \"web1\"\nssh = \"a\"\nbackend = \"openstack\"\n",
    )
    .expect("write");
    let err =
      PushConfig::from_toml_file(&path).expect_err("invalid host backend");
    assert!(format!("{err}").contains("backend"), "got: {err}");
  }

  #[test]
  fn a_host_entry_without_a_name_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[[push.hosts]]\nssh = \"deploy@10.0.0.1\"\n")
      .expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("missing name");
    assert!(format!("{err}").contains("name"), "got: {err}");
  }

  #[test]
  fn a_host_entry_without_ssh_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[[push.hosts]]\nname = \"web1\"\n").expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("missing ssh");
    assert!(format!("{err}").contains("ssh"), "got: {err}");
  }

  #[test]
  fn an_unknown_host_key_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(
      &path,
      "[[push.hosts]]\nname = \"web1\"\nssh = \"a\"\ntypo = 1\n",
    )
    .expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("unknown host key");
    assert!(format!("{err}").contains("typo"), "got: {err}");
  }

  fn target(name: &str) -> super::super::discover::PushTarget {
    super::super::discover::PushTarget {
      addr:         TargetAddr {
        name:     name.to_owned(),
        ssh_host: name.to_owned(),
        ssh_opts: Vec::new(),
      },
      source:       TargetSource::NixosConfigurations,
      tags:         Vec::new(),
      installable:  String::new(),
      kind:         ProfileKind::NixOS,
      profile_path: String::new(),
      rebuild_tool: "nixos-rebuild",
    }
  }

  #[test]
  fn an_inventory_match_overrides_the_ssh_host() {
    let config = toml_config(
      "[[push.hosts]]\nname = \"web1\"\nssh = \"deploy@10.0.0.1\"\n",
    );
    let targets = apply_inventory(vec![target("web1")], &config);
    assert_eq!(targets[0].addr.ssh_host, "deploy@10.0.0.1");
  }

  #[test]
  fn an_unknown_name_falls_through_as_a_raw_ssh_target() {
    let config = toml_config(
      "[[push.hosts]]\nname = \"web1\"\nssh = \"deploy@10.0.0.1\"\n",
    );
    let targets = apply_inventory(vec![target("web2")], &config);
    // No inventory entry for "web2": the address discovery already
    // assigned (the bare name, per `PushTarget::from_attr`) is left
    // alone - this *is* "read as a raw ssh target directly".
    assert_eq!(targets[0].addr.ssh_host, "web2");
  }

  #[test]
  fn an_empty_inventory_touches_nothing() {
    let targets = apply_inventory(vec![target("web1")], &PushConfig::default());
    assert_eq!(targets[0].addr.ssh_host, "web1");
  }

  #[test]
  fn host_lookup_by_name_finds_the_right_entry() {
    let config = toml_config(
      "[[push.hosts]]\nname = \"a\"\nssh = \"x\"\n\n[[push.hosts]]\nname = \
       \"b\"\nssh = \"y\"\n",
    );
    assert_eq!(config.host("b").expect("found").ssh, "y");
    assert!(config.host("c").is_none());
  }

  // -- resolve_remote_build: the three-rung build-location ladder --------

  #[test]
  fn the_cli_flag_wins_over_every_other_rung() {
    let toml = toml_config("[push]\nremote-build = false\n");
    assert!(resolve_remote_build(Some(true), "web1", &toml, Some(false)));
    assert!(!resolve_remote_build(
      Some(false),
      "web1",
      &toml,
      Some(true)
    ));
  }

  #[test]
  fn without_a_flag_the_toml_table_default_answers() {
    let toml = toml_config("[push]\nremote-build = true\n");
    assert!(resolve_remote_build(None, "web1", &toml, Some(false)));
  }

  #[test]
  fn a_per_host_override_outranks_the_table_level_default() {
    let toml = toml_config(
      "[push]\nremote-build = false\n\n[[push.hosts]]\nname = \"web1\"\nssh = \
       \"deploy@web1\"\nremote-build = true\n",
    );
    assert!(resolve_remote_build(None, "web1", &toml, Some(false)));
    // A different host falls through to the table-level default.
    assert!(!resolve_remote_build(None, "web2", &toml, Some(true)));
  }

  #[test]
  #[serial]
  fn with_nothing_configured_the_nix_module_default_answers() {
    let toml = PushConfig::default();
    for env_value in [None, Some("1")] {
      let _guard = match env_value {
        Some(value) => EnvGuard::set(BUILD_FLOOR_REMOTE_ENV, value),
        None => EnvGuard::remove(BUILD_FLOOR_REMOTE_ENV),
      };
      // The Nix-module rung outranks the floor either way: the opt-in
      // gate must not change this answer.
      assert!(resolve_remote_build(None, "web1", &toml, Some(true)));
      assert!(!resolve_remote_build(None, "web1", &toml, Some(false)));
    }
  }

  #[test]
  #[serial]
  fn with_every_rung_silent_the_floor_matches_the_opt_in_gate() {
    let toml = PushConfig::default();

    let _guard = EnvGuard::remove(BUILD_FLOOR_REMOTE_ENV);
    assert!(!resolve_remote_build(None, "web1", &toml, None));
    drop(_guard);

    let _guard = EnvGuard::set(BUILD_FLOOR_REMOTE_ENV, "1");
    assert!(resolve_remote_build(None, "web1", &toml, None));
  }

  // -- resolve_remote_build: the floor flip, behind its opt-in -----------

  #[test]
  #[serial]
  fn with_the_opt_in_off_an_unconfigured_target_still_floors_to_false() {
    let _guard = EnvGuard::remove(BUILD_FLOOR_REMOTE_ENV);
    let toml = PushConfig::default();
    assert!(!resolve_remote_build(None, "web1", &toml, None));
  }

  #[test]
  #[serial]
  fn with_the_opt_in_on_an_unconfigured_target_floors_to_remote_build() {
    let _guard = EnvGuard::set(BUILD_FLOOR_REMOTE_ENV, "1");
    let toml = PushConfig::default();
    assert!(resolve_remote_build(None, "web1", &toml, None));
  }

  #[test]
  #[serial]
  fn with_the_opt_in_on_a_table_level_override_still_wins_over_the_floor() {
    let _guard = EnvGuard::set(BUILD_FLOOR_REMOTE_ENV, "1");
    let toml = toml_config("[push]\nremote-build = false\n");
    assert!(!resolve_remote_build(None, "web1", &toml, None));
  }

  // -- resolve_backend: the five-rung backend ladder ---------------------

  #[test]
  fn the_cli_backend_flag_wins_over_every_other_rung() {
    let toml = toml_config("[push]\nbackend = \"colmena\"\n");
    assert_eq!(
      resolve_backend(
        PushBackend::Builtin,
        "web1",
        &toml,
        Some(PushBackend::DeployRs)
      ),
      PushBackend::Builtin
    );
  }

  #[test]
  fn without_a_cli_flag_the_toml_table_backend_default_answers() {
    let toml = toml_config("[push]\nbackend = \"colmena\"\n");
    assert_eq!(
      resolve_backend(
        PushBackend::Auto,
        "web1",
        &toml,
        Some(PushBackend::Builtin)
      ),
      PushBackend::Colmena
    );
  }

  #[test]
  fn a_per_host_backend_override_outranks_the_table_level_default() {
    let toml = toml_config(
      "[push]\nbackend = \"colmena\"\n\n[[push.hosts]]\nname = \"web1\"\nssh \
       = \"deploy@web1\"\nbackend = \"deploy-rs\"\n",
    );
    assert_eq!(
      resolve_backend(
        PushBackend::Auto,
        "web1",
        &toml,
        Some(PushBackend::Builtin)
      ),
      PushBackend::DeployRs
    );
    // A different host falls through to the table-level default.
    assert_eq!(
      resolve_backend(
        PushBackend::Auto,
        "web2",
        &toml,
        Some(PushBackend::Builtin)
      ),
      PushBackend::Colmena
    );
  }

  #[test]
  fn with_nothing_configured_the_nix_module_backend_default_answers() {
    let toml = PushConfig::default();
    assert_eq!(
      resolve_backend(
        PushBackend::Auto,
        "web1",
        &toml,
        Some(PushBackend::Colmena)
      ),
      PushBackend::Colmena
    );
  }

  #[test]
  fn with_every_rung_silent_the_floor_is_auto() {
    let toml = PushConfig::default();
    assert_eq!(
      resolve_backend(PushBackend::Auto, "web1", &toml, None),
      PushBackend::Auto
    );
  }

  // -- build_floor_remote_opt_in: the floor flip's one-release gate ------

  #[test]
  #[serial]
  fn unset_the_opt_in_is_off() {
    let _guard = EnvGuard::remove(BUILD_FLOOR_REMOTE_ENV);
    assert!(!build_floor_remote_opt_in());
  }

  #[test]
  #[serial]
  fn set_to_1_the_opt_in_is_on() {
    let _guard = EnvGuard::set(BUILD_FLOOR_REMOTE_ENV, "1");
    assert!(build_floor_remote_opt_in());
  }

  #[test]
  #[serial]
  fn set_to_true_the_opt_in_is_on() {
    let _guard = EnvGuard::set(BUILD_FLOOR_REMOTE_ENV, "true");
    assert!(build_floor_remote_opt_in());
  }

  #[test]
  #[serial]
  fn set_to_anything_else_the_opt_in_is_off() {
    let _guard = EnvGuard::set(BUILD_FLOOR_REMOTE_ENV, "0");
    assert!(!build_floor_remote_opt_in());
  }

  // -- retry-attempts parsing ----------------------------------------------

  #[test]
  fn retry_attempts_parses_as_an_integer() {
    let config = toml_config("[push]\nretry-attempts = 5\n");
    assert_eq!(config.retry_attempts, Some(5));
  }

  #[test]
  fn a_non_integer_retry_attempts_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[push]\nretry-attempts = \"five\"\n")
      .expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("not an integer");
    assert!(format!("{err}").contains("retry-attempts"), "got: {err}");
  }

  #[test]
  fn a_negative_retry_attempts_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[push]\nretry-attempts = -1\n").expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("negative");
    assert!(format!("{err}").contains("retry-attempts"), "got: {err}");
  }

  #[test]
  fn a_host_level_retry_attempts_parses_in_order() {
    let config = toml_config(
      "[[push.hosts]]\nname = \"web1\"\nssh = \
       \"deploy@10.0.0.1\"\nretry-attempts = 7\n",
    );
    assert_eq!(config.hosts[0].retry_attempts, Some(7));
  }

  // -- resolve_retry_attempts: the three-rung retry-attempts ladder -------

  #[test]
  fn the_cli_flag_wins_over_every_other_retry_rung() {
    let toml = toml_config("[push]\nretry-attempts = 5\n");
    assert_eq!(resolve_retry_attempts(Some(0), "web1", &toml), 0);
    assert_eq!(resolve_retry_attempts(Some(9), "web1", &toml), 9);
  }

  #[test]
  fn without_a_flag_the_toml_table_default_answers_for_retry() {
    let toml = toml_config("[push]\nretry-attempts = 5\n");
    assert_eq!(resolve_retry_attempts(None, "web1", &toml), 5);
  }

  #[test]
  fn a_per_host_retry_override_outranks_the_table_level_default() {
    let toml = toml_config(
      "[push]\nretry-attempts = 5\n\n[[push.hosts]]\nname = \"web1\"\nssh = \
       \"deploy@web1\"\nretry-attempts = 9\n",
    );
    assert_eq!(resolve_retry_attempts(None, "web1", &toml), 9);
    // A different host falls through to the table-level default.
    assert_eq!(resolve_retry_attempts(None, "web2", &toml), 5);
  }

  #[test]
  fn with_every_retry_rung_silent_the_floor_is_three() {
    let toml = PushConfig::default();
    assert_eq!(resolve_retry_attempts(None, "web1", &toml), 3);
  }

  // -- [push] sources-transport: TOML parsing -----------------------------

  #[test]
  fn sources_transport_parses_as_a_string() {
    let config = toml_config("[push]\nsources-transport = \"target-pull\"\n");
    assert_eq!(config.sources_transport, Some(SourcesTransport::TargetPull));
  }

  #[test]
  fn an_unknown_sources_transport_value_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[push]\nsources-transport = \"rsync\"\n")
      .expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("unknown value");
    assert!(format!("{err}").contains("rsync"), "got: {err}");
  }

  #[test]
  fn a_non_string_sources_transport_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(&path, "[push]\nsources-transport = true\n").expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("wrong type");
    assert!(format!("{err}").contains("sources-transport"), "got: {err}");
  }

  #[test]
  fn a_host_level_sources_transport_override_parses() {
    let config = toml_config(
      "[[push.hosts]]\nname = \"web1\"\nssh = \
       \"deploy@web1\"\nsources-transport = \"target-pull\"\n",
    );
    assert_eq!(
      config.hosts[0].sources_transport,
      Some(SourcesTransport::TargetPull)
    );
  }

  #[test]
  fn an_unknown_host_level_sources_transport_value_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(".xi.toml");
    std::fs::write(
      &path,
      "[[push.hosts]]\nname = \"web1\"\nssh = \"a\"\nsources-transport = \
       \"rsync\"\n",
    )
    .expect("write");
    let err = PushConfig::from_toml_file(&path).expect_err("unknown value");
    assert!(format!("{err}").contains("rsync"), "got: {err}");
  }

  // -- resolve_sources_transport: the sources-transport ladder ------------

  #[test]
  fn the_cli_flag_wins_sources_transport_over_every_other_rung() {
    let toml = toml_config("[push]\nsources-transport = \"target-pull\"\n");
    assert_eq!(
      resolve_sources_transport(
        Some(SourcesTransport::OperatorCopy),
        "web1",
        &toml
      ),
      SourcesTransport::OperatorCopy
    );
  }

  #[test]
  fn without_a_flag_the_toml_table_level_sources_transport_default_answers() {
    let toml = toml_config("[push]\nsources-transport = \"target-pull\"\n");
    assert_eq!(
      resolve_sources_transport(None, "web1", &toml),
      SourcesTransport::TargetPull
    );
  }

  #[test]
  fn a_per_host_sources_transport_override_outranks_the_table_level_default() {
    let toml = toml_config(
      "[push]\nsources-transport = \"operator-copy\"\n\n[[push.hosts]]\nname \
       = \"web1\"\nssh = \"deploy@web1\"\nsources-transport = \
       \"target-pull\"\n",
    );
    assert_eq!(
      resolve_sources_transport(None, "web1", &toml),
      SourcesTransport::TargetPull
    );
    // A different host falls through to the table-level default.
    assert_eq!(
      resolve_sources_transport(None, "web2", &toml),
      SourcesTransport::OperatorCopy
    );
  }

  #[test]
  fn with_every_sources_transport_rung_silent_the_floor_is_operator_copy() {
    let toml = PushConfig::default();
    assert_eq!(
      resolve_sources_transport(None, "web1", &toml),
      SourcesTransport::OperatorCopy
    );
  }
}
