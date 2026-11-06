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

use std::path::Path;

use color_eyre::{Result, eyre::eyre};

/// Every key the `[push]` table accepts, besides the `hosts` sub-table.
pub const PUSH_KEYS: &[&str] = &["remote-build", "retry-attempts"];

/// Sub-tables the `[push]` table accepts, alongside [`PUSH_KEYS`].
pub const PUSH_TABLE_KEYS: &[&str] = &["hosts"];

/// Every key a `[[push.hosts]]` entry accepts.
pub const PUSH_HOST_KEYS: &[&str] =
  &["name", "ssh", "remote-build", "retry-attempts"];

/// One `[[push.hosts]]` entry: an inventory name and the SSH target it
/// resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushHost {
  /// The name `xi push <name>` matches against.
  pub name:         String,
  /// `[user@]hostname[:port]` to connect to for this host.
  pub ssh:          String,
  /// Per-host override of the `remote-build` rung.
  pub remote_build: Option<bool>,
  /// Per-host override of the `retry-attempts` rung.
  pub retry_attempts: Option<u32>,
}

/// The parsed `[push]` table: the `remote-build` default and the flat
/// host inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushConfig {
  /// `.xi.toml [push] remote-build`, the middle rung of the
  /// build-location ladder.
  pub remote_build: Option<bool>,
  /// `.xi.toml [push] retry-attempts`, the table-level rung of the
  /// retry-attempts ladder.
  pub retry_attempts: Option<u32>,
  /// `[[push.hosts]]` entries, in file order.
  pub hosts:        Vec<PushHost>,
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

    let retry_attempts = match table.get("retry-attempts") {
      None => None,
      Some(item) => Some(parse_retry_attempts(path, item, "[push]")?),
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
      retry_attempts,
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
  let retry_attempts = match entry.get("retry-attempts") {
    None => None,
    Some(item) => Some(parse_retry_attempts(
      path,
      item,
      &format!("[[push.hosts]] entry '{name}'"),
    )?),
  };

  Ok(PushHost {
    name,
    ssh,
    remote_build,
    retry_attempts,
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
      "{}: 'retry-attempts' in {context} must be a non-negative integer \
       that fits in a u32",
      path.display()
    )
  })
}

/// Resolve the build-location ladder for one target: `--remote-build` >
/// `.xi.toml [push]` (a per-host `[[push.hosts]]` override first, then
/// the table-level default) > the Nix-module-provided default > `false`.
///
/// `false` (closure: build here, copy the closure in) is the compiled-in
/// floor every rung can override; `true` is `--mode sources`. The
/// caller still has the final word through an explicit `--mode` flag -
/// see [`super::args::PushArgs::effective_mode`] - which this function
/// does not know about: it only resolves what the *ladder itself* says,
/// not whether something further up overrides the ladder outright.
#[must_use]
pub fn resolve_remote_build(
  cli: Option<bool>,
  host_name: &str,
  toml_config: &PushConfig,
  nix_module_remote_build: Option<bool>,
) -> bool {
  if let Some(flag) = cli {
    return flag;
  }
  if let Some(per_host) = toml_config
    .host(host_name)
    .and_then(|host| host.remote_build)
  {
    return per_host;
  }
  if let Some(table_default) = toml_config.remote_build {
    return table_default;
  }
  nix_module_remote_build.unwrap_or(false)
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
  use tempfile::TempDir;

  use super::*;
  use crate::{
    push::{discover::TargetSource, engine::TargetAddr},
    target::ProfileKind,
  };

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
       \"deploy@10.0.0.2:2222\"\nremote-build = true\n",
    );
    assert_eq!(config.hosts.len(), 2);
    assert_eq!(config.hosts[0].name, "web1");
    assert_eq!(config.hosts[0].ssh, "deploy@10.0.0.1");
    assert_eq!(config.hosts[0].remote_build, None);
    assert_eq!(config.hosts[1].ssh, "deploy@10.0.0.2:2222");
    assert_eq!(config.hosts[1].remote_build, Some(true));
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
  fn with_nothing_configured_the_nix_module_default_answers() {
    let toml = PushConfig::default();
    assert!(resolve_remote_build(None, "web1", &toml, Some(true)));
    assert!(!resolve_remote_build(None, "web1", &toml, Some(false)));
  }

  #[test]
  fn with_every_rung_silent_the_floor_is_false() {
    let toml = PushConfig::default();
    assert!(!resolve_remote_build(None, "web1", &toml, None));
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
    let err =
      PushConfig::from_toml_file(&path).expect_err("not an integer");
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
      "[[push.hosts]]\nname = \"web1\"\nssh = \"deploy@10.0.0.1\"\nretry-attempts \
       = 7\n",
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
}
