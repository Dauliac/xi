//! Target discovery and backend detection.
//!
//! Discovery reads the flake output taxonomy
//! ([`xi_core::flake_output::FlakeOutput`]) rather than a hardcoded list
//! of attribute names, so a target declared any of the six ways the
//! house taxonomy knows about is found by one pass.
//!
//! Detection is deliberately separate from discovery and takes the
//! *filtered* target set: the spec's rule is "`deploy-rs` if ANY
//! requested target has a `deploy.nodes` stanza", which cannot be decided
//! before the operator's positional arguments have been applied.

use std::collections::BTreeMap;

use color_eyre::{
  Result,
  eyre::{bail, eyre},
};
use tracing::debug;
use xi_core::flake_output::FlakeOutput;

use super::{
  args::PushBackend,
  engine::{Nix, TargetAddr},
};
use crate::target::{DeployRsConfig, ProfileKind};

/// Which flake output a target was discovered through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetSource {
  /// deploy-rs `deploy.nodes.<n>`. Highest precedence: an explicit
  /// deploy stanza is the operator saying how this host is deployed.
  Deploy,
  /// Colmena `colmenaHive.nodes.<n>`.
  ColmenaHive,
  /// `nixosConfigurations.<n>`.
  NixosConfigurations,
  /// `darwinConfigurations.<n>`.
  DarwinConfigurations,
  /// `systemConfigs.<n>` (system-manager).
  SystemConfigs,
  /// `homeConfigurations.<n>`.
  HomeConfigurations,
}

impl TargetSource {
  /// Every source, in precedence order (most specific first).
  pub const ALL: &'static [Self] = &[
    Self::Deploy,
    Self::ColmenaHive,
    Self::NixosConfigurations,
    Self::DarwinConfigurations,
    Self::SystemConfigs,
    Self::HomeConfigurations,
  ];

  /// The taxonomy variant this source reads.
  #[must_use]
  pub const fn output(self) -> FlakeOutput {
    match self {
      Self::Deploy => FlakeOutput::Deploy,
      Self::ColmenaHive => FlakeOutput::ColmenaHive,
      Self::NixosConfigurations => FlakeOutput::NixosConfigurations,
      Self::DarwinConfigurations => FlakeOutput::DarwinConfigurations,
      Self::SystemConfigs => FlakeOutput::SystemConfigs,
      Self::HomeConfigurations => FlakeOutput::HomeConfigurations,
    }
  }

  /// What kind of activation a target from this source needs.
  #[must_use]
  pub fn profile_kind(self) -> ProfileKind {
    match self {
      // A deploy stanza or a colmena hive node is a NixOS system unless
      // its own per-profile metadata says otherwise, which is read
      // separately when the stanza is parsed.
      Self::Deploy | Self::ColmenaHive | Self::NixosConfigurations => {
        ProfileKind::NixOS
      },
      Self::DarwinConfigurations => ProfileKind::Darwin,
      // system-manager has no `<tool> switch --flake <ref>#<name>`
      // shape, so its activation is the same bespoke build-toplevel /
      // profile-set / activate sequence `xi system switch` runs
      // locally (`crates/xi/src/system/mod.rs`), rather than one of the
      // other named kinds. The build-toplevel step already happened by
      // the time this command runs (`--mode closure` builds and copies
      // the closure before activating), so only the profile-set and
      // activate steps remain; `{store_path}` is a literal placeholder
      // `activation_script` substitutes once the real store path is
      // known.
      Self::SystemConfigs => {
        ProfileKind::Custom {
          activate_command: system_configs_activate_command(),
        }
      },
      Self::HomeConfigurations => ProfileKind::HomeManager,
    }
  }
}

/// Literal placeholder a `ProfileKind::Custom` activate-command template
/// carries in place of the real store path, which is not known until
/// push time. [`crate::push::orchestrate`]'s `activation_script`
/// substitutes it once the build has run.
pub(crate) const STORE_PATH_PLACEHOLDER: &str = "{store_path}";

/// The `ProfileKind::Custom` activate-command template a `SystemConfigs`
/// target carries, mirroring `xi system switch`'s own register
/// (`nix build --no-link --profile <path> <store-path>`) and activate
/// (`<store-path>/bin/activate`) steps.
///
/// Carries [`STORE_PATH_PLACEHOLDER`] where the two other `ProfileKind`s'
/// activation commands use the real `store_path`, since `Custom`'s
/// command is a fixed string set at discovery time, before any build has
/// run.
fn system_configs_activate_command() -> String {
  format!(
    "nix build --no-link --profile {SYSTEM_MANAGER_PROFILE} \
     {STORE_PATH_PLACEHOLDER} && {STORE_PATH_PLACEHOLDER}/bin/activate"
  )
}

/// One target a push can address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
  /// How the transport reaches it.
  pub addr:         TargetAddr,
  /// Which output it came from.
  pub source:       TargetSource,
  /// Tags for `--on @tag` filtering. Only colmena supplies these.
  pub tags:         Vec<String>,
  /// Installable to build for `--mode closure`.
  pub installable:  String,
  /// Kind of activation to run.
  pub kind:         ProfileKind,
  /// Nix profile the generation is registered in.
  pub profile_path: String,
  /// The rebuild tool preflight expects to find on the target.
  pub rebuild_tool: &'static str,
}

/// System profile every host-wide generation is registered in.
pub const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";

/// Per-user profile a Home Manager generation is registered in.
pub const HOME_PROFILE: &str =
  "/nix/var/nix/profiles/per-user/$USER/home-manager";

impl PushTarget {
  /// Build a target from a flake attribute name.
  fn from_attr(flake_ref: &str, name: &str, source: TargetSource) -> Self {
    let kind = source.profile_kind();
    // Without a `deployment.targetHost` to read, the attribute name is
    // the only hostname the flake offers. That is also what the existing
    // `builtin` deploy backend assumes, so the two cannot disagree.
    let installable = match source {
      TargetSource::HomeConfigurations => {
        format!("{flake_ref}#homeConfigurations.{name}.activationPackage")
      },
      TargetSource::DarwinConfigurations => {
        format!(
          "{flake_ref}#darwinConfigurations.{name}.config.system.build.\
           toplevel"
        )
      },
      // Unlike `nixosConfigurations`/`darwinConfigurations`, a
      // `systemConfigs.<name>` entry evaluates directly to the toplevel
      // derivation (mirrors `resolve_toplevel`'s `FlakeOutput::SystemConfigs`
      // call in `xi system switch`, which passes an empty `build_path`).
      TargetSource::SystemConfigs => {
        format!("{flake_ref}#systemConfigs.{name}")
      },
      TargetSource::ColmenaHive => {
        format!(
          "{flake_ref}#colmenaHive.nodes.{name}.config.system.build.toplevel"
        )
      },
      TargetSource::Deploy | TargetSource::NixosConfigurations => {
        format!(
          "{flake_ref}#nixosConfigurations.{name}.config.system.build.toplevel"
        )
      },
    };

    Self {
      addr: TargetAddr {
        name:     name.to_owned(),
        ssh_host: name.to_owned(),
        ssh_opts: Vec::new(),
      },
      source,
      tags: Vec::new(),
      installable,
      profile_path: profile_for(&kind).to_owned(),
      rebuild_tool: rebuild_tool_for(&kind),
      kind,
    }
  }
}

/// System profile system-manager registers a host-wide generation in,
/// parallel to [`SYSTEM_PROFILE`] (`crates/xi/src/system/mod.rs`'s own
/// `SYSTEM_PROFILE` constant for the same path).
pub const SYSTEM_MANAGER_PROFILE: &str =
  "/nix/var/nix/profiles/system-manager-profiles/system-manager";

/// The Nix profile a generation of this kind is registered in.
#[must_use]
pub const fn profile_for(kind: &ProfileKind) -> &'static str {
  match kind {
    ProfileKind::HomeManager => HOME_PROFILE,
    ProfileKind::Custom { .. } => SYSTEM_MANAGER_PROFILE,
    _ => SYSTEM_PROFILE,
  }
}

/// The rebuild tool preflight expects on a target of this kind.
///
/// `ProfileKind::Custom` targets (system-manager today) have no
/// `<tool> switch` CLI to probe for; `nix` itself is the one binary the
/// build-toplevel/profile-set/activate sequence actually needs.
#[must_use]
pub const fn rebuild_tool_for(kind: &ProfileKind) -> &'static str {
  match kind {
    ProfileKind::Darwin => "darwin-rebuild",
    ProfileKind::HomeManager => "home-manager",
    ProfileKind::Custom { .. } => "nix",
    _ => "nixos-rebuild",
  }
}

/// Discover every target the flake declares, preferring the most
/// specific source when the same name appears more than once.
///
/// Absence of an output is not an error: a flake with only
/// `nixosConfigurations` is the common case, and probing the other four
/// has to be cheap and quiet.
///
/// # Errors
///
/// Propagates evaluation errors from [`Nix::eval_json`] and malformed
/// `deploy` output.
pub fn discover(nix: &dyn Nix, flake_ref: &str) -> Result<Vec<PushTarget>> {
  // Keyed by name so a later (lower-precedence) source cannot displace
  // an earlier one. `TargetSource::ALL` is in precedence order, which is
  // what makes SC-P-008 ("prefer the explicit deploy stanza") fall out
  // of the iteration order rather than needing a special case.
  let mut by_name: BTreeMap<String, PushTarget> = BTreeMap::new();

  for source in TargetSource::ALL.iter().copied() {
    for target in discover_one(nix, flake_ref, source)? {
      by_name.entry(target.addr.name.clone()).or_insert(target);
    }
  }

  Ok(by_name.into_values().collect())
}

/// Discover the targets one output declares.
fn discover_one(
  nix: &dyn Nix,
  flake_ref: &str,
  source: TargetSource,
) -> Result<Vec<PushTarget>> {
  let attr = format!("{flake_ref}#{}", source.output().as_str());

  match source {
    TargetSource::Deploy => {
      let Some(value) = nix.eval_json(&attr, None)? else {
        return Ok(Vec::new());
      };
      let config: DeployRsConfig = serde_json::from_value(value)
        .map_err(|e| eyre!("{attr} is not a deploy-rs configuration: {e}"))?;
      Ok(
        config
          .into_targets()
          .into_iter()
          .map(|target| {
            let kind = target
              .profiles
              .first()
              .map_or(ProfileKind::NixOS, |profile| profile.kind.clone());
            let installable = target.profiles.first().map_or_else(
              || {
                format!(
                  "{flake_ref}#nixosConfigurations.{}.config.system.build.\
                   toplevel",
                  target.name
                )
              },
              |profile| profile.path.clone(),
            );
            let profile_path = target
              .profiles
              .first()
              .and_then(|profile| profile.profile_path.clone())
              .unwrap_or_else(|| profile_for(&kind).to_owned());
            PushTarget {
              addr: TargetAddr {
                name:     target.name,
                ssh_host: target.ssh_user.map_or_else(
                  || target.hostname.clone(),
                  |user| format!("{user}@{}", target.hostname),
                ),
                ssh_opts: target.ssh_opts,
              },
              source,
              tags: target.tags,
              installable,
              rebuild_tool: rebuild_tool_for(&kind),
              kind,
              profile_path,
            }
          })
          .collect(),
      )
    },

    TargetSource::ColmenaHive => {
      // A hive is not plain data, so `attrNames` on it can fail for
      // reasons unrelated to the output existing. Falling back to a
      // forced `true` keeps "colmenaHive is present" decidable, which
      // is all backend detection needs; colmena itself enumerates the
      // nodes when it runs.
      let names = nix
        .eval_json(&attr, Some("hive: builtins.attrNames (hive.nodes or {})"))?
        .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok());
      if let Some(names) = names {
        Ok(
          names
            .iter()
            .map(|name| PushTarget::from_attr(flake_ref, name, source))
            .collect(),
        )
      } else {
        let present = nix.eval_json(&attr, Some("_hive: true"))?.is_some();
        debug!(attr, present, "colmenaHive probed without node names");
        Ok(Vec::new())
      }
    },

    TargetSource::NixosConfigurations
    | TargetSource::DarwinConfigurations
    | TargetSource::SystemConfigs
    | TargetSource::HomeConfigurations => {
      let Some(value) =
        nix.eval_json(&attr, Some("set: builtins.attrNames set"))?
      else {
        return Ok(Vec::new());
      };
      let names: Vec<String> = serde_json::from_value(value)
        .map_err(|e| eyre!("{attr} did not evaluate to a name list: {e}"))?;
      Ok(
        names
          .iter()
          .map(|name| PushTarget::from_attr(flake_ref, name, source))
          .collect(),
      )
    },
  }
}

/// Whether the flake has a `colmenaHive` output at all.
///
/// Detection needs this separately from discovery because a hive whose
/// `nodes` cannot be enumerated still selects the colmena backend.
///
/// # Errors
///
/// Propagates evaluation errors.
pub fn has_colmena_hive(nix: &dyn Nix, flake_ref: &str) -> Result<bool> {
  let attr = format!("{flake_ref}#{}", FlakeOutput::ColmenaHive.as_str());
  Ok(nix.eval_json(&attr, Some("_hive: true"))?.is_some())
}

/// The backend a push will actually use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedBackend {
  /// Delegate to the `deploy` CLI path in `xi-deploy`.
  DeployRs,
  /// Delegate to the `colmena` CLI.
  Colmena,
  /// Run xi's own SSH pipeline.
  Builtin,
}

impl ResolvedBackend {
  /// Human-readable name, matching `xi deploy --backend` spellings.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::DeployRs => "deploy-rs",
      Self::Colmena => "colmena",
      Self::Builtin => "builtin",
    }
  }
}

/// Pick the backend for `targets`.
///
/// An explicit `--backend` skips detection entirely, per spec; `auto`
/// walks the three rules in order. `colmena_hive_present` is passed in
/// rather than probed here so this stays a pure decision function.
#[must_use]
pub fn detect_backend(
  requested: PushBackend,
  targets: &[PushTarget],
  colmena_hive_present: bool,
) -> ResolvedBackend {
  match requested {
    PushBackend::DeployRs => return ResolvedBackend::DeployRs,
    PushBackend::Colmena => return ResolvedBackend::Colmena,
    PushBackend::Builtin => return ResolvedBackend::Builtin,
    PushBackend::Auto => {},
  }

  if targets
    .iter()
    .any(|target| target.source == TargetSource::Deploy)
  {
    return ResolvedBackend::DeployRs;
  }
  if colmena_hive_present
    || targets
      .iter()
      .any(|target| target.source == TargetSource::ColmenaHive)
  {
    return ResolvedBackend::Colmena;
  }
  ResolvedBackend::Builtin
}

/// Narrow `targets` to the ones the operator asked for.
///
/// An empty positional list and no `--on` means every discovered target,
/// which is the documented "push the fleet" spelling.
///
/// # Errors
///
/// Returns an error naming the available targets when the selection
/// matches nothing, and an error naming the bad pattern when `--on` is
/// not a valid regex.
pub fn select<'a>(
  targets: &'a [PushTarget],
  names: &[String],
  on: Option<&str>,
) -> Result<Vec<&'a PushTarget>> {
  let mut selected: Vec<&PushTarget> = if names.is_empty() {
    targets.iter().collect()
  } else {
    targets
      .iter()
      .filter(|target| names.contains(&target.addr.name))
      .collect()
  };

  if let Some(filter) = on {
    selected = apply_on_filter(selected, filter)?;
  }

  if selected.is_empty() {
    let available: Vec<&str> = targets
      .iter()
      .map(|target| target.addr.name.as_str())
      .collect();
    if available.is_empty() {
      bail!(
        "no push targets found. Looked at {}.",
        TargetSource::ALL
          .iter()
          .map(|source| source.output().as_str())
          .collect::<Vec<_>>()
          .join(", ")
      );
    }
    bail!("no target matched. Available: {}", available.join(", "));
  }

  Ok(selected)
}

/// Apply the `--on` filter: `@tag` matches tags, anything else is a regex
/// over target names.
fn apply_on_filter<'a>(
  targets: Vec<&'a PushTarget>,
  filter: &str,
) -> Result<Vec<&'a PushTarget>> {
  if let Some(tag) = filter.strip_prefix('@') {
    if targets.iter().all(|target| target.tags.is_empty()) {
      bail!(
        "--on @{tag} filters by tag, but no discovered target carries tags. \
         Tags come from a colmena hive; use a name or a regex instead."
      );
    }
    return Ok(
      targets
        .into_iter()
        .filter(|target| target.tags.iter().any(|t| t == tag))
        .collect(),
    );
  }

  let pattern = regex::Regex::new(filter)
    .map_err(|e| eyre!("--on '{filter}' is not a valid regex: {e}"))?;
  Ok(
    targets
      .into_iter()
      .filter(|target| pattern.is_match(&target.addr.name))
      .collect(),
  )
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  fn target(name: &str, source: TargetSource) -> PushTarget {
    PushTarget::from_attr(".", name, source)
  }

  #[test]
  fn an_explicit_backend_skips_detection() {
    let targets = vec![target("a", TargetSource::Deploy)];
    assert_eq!(
      detect_backend(PushBackend::Builtin, &targets, true),
      ResolvedBackend::Builtin
    );
  }

  #[test]
  fn any_deploy_stanza_selects_deploy_rs() {
    let targets = vec![
      target("a", TargetSource::Deploy),
      target("b", TargetSource::NixosConfigurations),
    ];
    assert_eq!(
      detect_backend(PushBackend::Auto, &targets, false),
      ResolvedBackend::DeployRs
    );
  }

  #[test]
  fn a_hive_without_deploy_selects_colmena() {
    let targets = vec![target("b", TargetSource::NixosConfigurations)];
    assert_eq!(
      detect_backend(PushBackend::Auto, &targets, true),
      ResolvedBackend::Colmena
    );
  }

  #[test]
  fn plain_nixos_configurations_fall_back_to_builtin() {
    let targets = vec![target("b", TargetSource::NixosConfigurations)];
    assert_eq!(
      detect_backend(PushBackend::Auto, &targets, false),
      ResolvedBackend::Builtin
    );
  }

  #[test]
  fn a_deploy_stanza_wins_over_a_bare_nixos_configuration() {
    let mut by_name: BTreeMap<String, PushTarget> = BTreeMap::new();
    for source in TargetSource::ALL.iter().copied() {
      by_name
        .entry("web".to_owned())
        .or_insert_with(|| target("web", source));
    }
    assert_eq!(
      by_name.get("web").expect("web").source,
      TargetSource::Deploy
    );
  }

  #[test]
  fn selection_with_no_names_takes_everything() {
    let targets = vec![
      target("a", TargetSource::NixosConfigurations),
      target("b", TargetSource::NixosConfigurations),
    ];
    let selected = select(&targets, &[], None).expect("selects");
    assert_eq!(selected.len(), 2);
  }

  #[test]
  fn a_regex_on_filter_narrows_by_name() {
    let targets = vec![
      target("web1", TargetSource::NixosConfigurations),
      target("db1", TargetSource::NixosConfigurations),
    ];
    let selected = select(&targets, &[], Some("^web")).expect("selects");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].addr.name, "web1");
  }

  #[test]
  fn a_tag_filter_explains_itself_when_nothing_has_tags() {
    let targets = vec![target("web1", TargetSource::NixosConfigurations)];
    let err = select(&targets, &[], Some("@prod")).expect_err("no tags");
    assert!(format!("{err}").contains("colmena hive"), "got: {err}");
  }

  #[test]
  fn an_unmatched_name_lists_what_is_available() {
    let targets = vec![target("web1", TargetSource::NixosConfigurations)];
    let err =
      select(&targets, &["nope".to_owned()], None).expect_err("no such target");
    assert!(format!("{err}").contains("web1"), "got: {err}");
  }

  #[test]
  fn home_configurations_build_the_activation_package() {
    let home = target("me", TargetSource::HomeConfigurations);
    assert!(
      home.installable.ends_with(".activationPackage"),
      "got: {}",
      home.installable
    );
    assert_eq!(home.rebuild_tool, "home-manager");
  }

  #[test]
  fn darwin_configurations_expect_darwin_rebuild() {
    let darwin = target("mac", TargetSource::DarwinConfigurations);
    assert_eq!(darwin.rebuild_tool, "darwin-rebuild");
    assert_eq!(darwin.kind, ProfileKind::Darwin);
  }

  #[test]
  fn system_configs_build_the_toplevel_directly() {
    let web3 = target("web3", TargetSource::SystemConfigs);
    assert_eq!(web3.installable, ".#systemConfigs.web3");
    assert_eq!(web3.rebuild_tool, "nix");
    assert_eq!(web3.profile_path, SYSTEM_MANAGER_PROFILE);
  }

  /// SC-MD-006 / xi-slej.3.2: the generated `activate_command` carries the
  /// same build-toplevel / profile-set / activate sequence `xi system
  /// switch` runs locally (`crates/xi/src/system/mod.rs`), not a
  /// `nixos-rebuild`- or `darwin-rebuild`-shaped command. The
  /// build-toplevel step already ran by the time this command executes
  /// (`--mode closure` builds and copies the closure first), so only the
  /// profile-set and activate steps are left in the template; `{store_path}`
  /// is a literal placeholder substituted once the real store path is
  /// known.
  #[test]
  fn system_configs_activate_command_runs_the_build_toplevel_sequence() {
    let web3 = target("web3", TargetSource::SystemConfigs);
    let ProfileKind::Custom { activate_command } = web3.kind else {
      panic!("expected ProfileKind::Custom, got {:?}", web3.kind);
    };
    assert_eq!(
      activate_command,
      format!(
        "nix build --no-link --profile {SYSTEM_MANAGER_PROFILE} \
         {{store_path}} && {{store_path}}/bin/activate"
      )
    );
    assert!(!activate_command.contains("nixos-rebuild"));
    assert!(!activate_command.contains("darwin-rebuild"));
  }

  /// Answers only a `systemConfigs` output, mirroring SC-MD-006's fixture
  /// flake declaring `systemConfigs.web3`.
  struct SystemConfigsFixtureNix;

  impl crate::push::engine::Nix for SystemConfigsFixtureNix {
    fn eval_json(
      &self,
      installable: &str,
      _apply: Option<&str>,
    ) -> Result<Option<serde_json::Value>> {
      if installable == ".#systemConfigs" {
        Ok(Some(serde_json::json!(["web3"])))
      } else {
        Ok(None)
      }
    }

    fn build(&self, installable: &str) -> Result<String> {
      Ok(format!("/nix/store/fake-{installable}"))
    }
  }

  #[test]
  fn a_system_configs_output_is_discovered_as_a_push_target() {
    let targets = discover(&SystemConfigsFixtureNix, ".").expect("discovers");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].addr.name, "web3");
    assert_eq!(targets[0].source, TargetSource::SystemConfigs);
    assert!(matches!(targets[0].kind, ProfileKind::Custom { .. }));
  }

  #[test]
  fn every_source_maps_to_a_taxonomy_variant() {
    for source in TargetSource::ALL.iter().copied() {
      let name = source.output().as_str();
      assert!(
        FlakeOutput::from_nix_name(name).is_some(),
        "{name} is not a known taxonomy variant"
      );
    }
  }
}
