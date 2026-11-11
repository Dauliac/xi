use std::{
  collections::HashMap,
  path::{Path, PathBuf},
};

use tracing::{debug, warn};
use xi_core::sources::config::{ConfigError, SourcesConfig};

/// Project-level configuration from `.xi.toml`.
#[derive(Debug, Default)]
pub struct ProjectConfig {
  pub build:    ProjectBuildConfig,
  pub ci:       ProjectCiConfig,
  pub consumer: ProjectConsumerConfig,
  pub doctor:   ProjectDoctorConfig,
  pub test:     ProjectTestConfig,
  pub fmt:      ProjectFmtConfig,
  pub sources:  SourcesConfig,
}

/// Build settings from the `[build]` section of `.xi.toml` or
/// `.xi.local.toml`.
#[derive(Debug, Default)]
pub struct ProjectBuildConfig {
  /// Flake input overrides, each tagged with the file it was written in so
  /// a relative value resolves against that file's own directory rather
  /// than the process working directory.
  pub override_input: Vec<xi_core::override_input::Entry>,
}

/// Formatter configuration.
#[derive(Debug, Default)]
pub struct ProjectFmtConfig {
  /// Formatter backend: "auto", "flake", or any command on PATH.
  pub backend: crate::args::FmtBackend,
}

/// CI-specific project configuration.
#[derive(Debug, Default)]
pub struct ProjectCiConfig {
  /// Build backend preference (auto, devour-flake, nix-fast-build).
  pub backend:       crate::args::CiBackend,
  /// Extra flake output paths to discover and build/eval.
  pub extra_outputs: Vec<String>,
}

/// Test configuration.
#[derive(Debug)]
pub struct ProjectTestConfig {
  /// Which backends to run. Empty = auto-detect all.
  pub backends:          Vec<String>,
  /// Flake attribute for `runTests` backend (per-system).
  pub run_tests_attr:    String,
  /// Glob filter on check derivation names.
  pub checks_filter:     String,
  /// Test directory for nix-unit standalone CLI.
  pub nix_unit_test_dir: String,
  /// Test directory for nixt.
  pub nixt_test_dir:     String,
  /// Custom command-based backends.
  pub custom:            Vec<CustomTestBackend>,
}

/// A user-defined test backend that shells out to a command.
#[derive(Debug, Clone)]
pub struct CustomTestBackend {
  pub name:    String,
  pub command: String,
  pub args:    Vec<String>,
}

impl Default for ProjectTestConfig {
  fn default() -> Self {
    Self {
      backends:          Vec::new(),
      run_tests_attr:    "tests".to_string(),
      checks_filter:     String::new(),
      nix_unit_test_dir: "tests/".to_string(),
      nixt_test_dir:     "tests/".to_string(),
      custom:            Vec::new(),
    }
  }
}

/// Per-rule override for a single `xi doctor` check, keyed by the check's
/// stable `CheckResult::code` (e.g. `"dirty-tree"`).
///
/// Parsed from `[doctor.rules]` in `.xi.toml`: a plain `false` maps to
/// `Off`; the strings `"off"`/`"warn"`/`"fail"` map directly. `true` or
/// the string `"on"` is accepted but never stored (it is the default, a
/// no-op -- `rules` only ever holds actual overrides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSetting {
  /// Drop this check's result entirely -- it neither prints nor counts
  /// toward the warning/failure tally.
  Off,
  /// Force this check's result to `Warn` when it fires, even if its own
  /// logic would normally produce `Fail`. Has no effect on an `Ok` result.
  Warn,
  /// Force this check's result to `Fail` when it fires, even if its own
  /// logic would normally produce `Warn`. Has no effect on an `Ok` result.
  Fail,
}

/// Doctor/health-check configuration.
#[derive(Debug, Default)]
pub struct ProjectDoctorConfig {
  /// Maximum input age in days before warning. Default: 30.
  pub max_input_age_days:       Option<u64>,
  /// Whether to warn on nixpkgs forks. Default: true.
  pub require_official_nixpkgs: bool,
  /// Override the list of supported nixpkgs branches.
  pub supported_branches:       Vec<String>,
  /// Disable the structural check group entirely
  /// (`check_flake_description`, `check_lock_present_and_tracked`,
  /// `check_floating_input_refs`, `check_duplicate_inputs`,
  /// `check_dirty_tree`, `check_nixconfig_substituter_pairing`). Default:
  /// false.
  ///
  /// This group is static/metadata-only (`flake.nix`/`flake.lock`/
  /// `nix flake metadata --json`) and the cheapest of the three new groups.
  /// A project that intentionally ships a floating-ref input, duplicate
  /// transitive inputs, or unpaired substituters can opt out here.
  pub disable_structure_checks: bool,
  /// Disable the derivation-metadata check group entirely
  /// (`check_package_meta_completeness`, `check_package_version`,
  /// `check_app_main_program`, `check_package_insecure`). Default: false.
  ///
  /// This group's eval helper (`discover_package_meta` in `lib.rs`) walks
  /// every `packages.<system>.*` attribute in a single `nix eval --apply
  /// --json` call. Per proposal.md's Open Question 1, this is the most
  /// expensive check group added by `xi-flake-doctor-checks`: cheap for
  /// most flakes, but on a flake with many (O(50+)) packages the eval walk
  /// itself can be noticeably slower than the other, purely static,
  /// `flake.lock`/`flake.nix` checks. It still runs on-by-default (the
  /// existing `check_formatter` already pays one unconditional eval call),
  /// but a project that finds the cost not worth it, or that
  /// intentionally ships undocumented internal packages, can opt out here.
  pub disable_metadata_checks:  bool,
  /// Disable the output-hygiene check group entirely
  /// (`check_per_system_output_gaps`, `check_checks_output_present`,
  /// `check_nonstandard_output_keys`). Default: false.
  ///
  /// This group reads one `nix flake show --json` result (fetched once via
  /// `fetch_flake_show_json` in `lib.rs` and shared across all three
  /// checks). A project that intentionally ships asymmetric per-system
  /// outputs, has no `checks` output by design, or deliberately exposes
  /// non-conventional top-level output keys can opt out of the whole group
  /// here, same posture as `disable_metadata_checks`.
  pub disable_output_checks:    bool,
  /// Per-rule overrides, keyed by `CheckResult::code` (e.g.
  /// `"dirty-tree"`, `"package-version"`). Checked in addition to, and
  /// independently of, the three group-level `disable_*_checks` flags
  /// above and `require_official_nixpkgs`: a rule can be off because its
  /// whole group is disabled, or individually, via this map, or both.
  ///
  /// This is the generalized mechanism (one override per check, covering
  /// all ~17 checks uniformly); the group flags and
  /// `require_official_nixpkgs` predate it and stay for backward
  /// compatibility rather than being folded into this map.
  pub rules:                    HashMap<String, RuleSetting>,
}

/// Consumer flake configuration.
///
/// Controls how xi aggregates flake outputs for `--all` / CI builds
/// via the consumer flake (replaces devour-flake).
#[derive(Debug)]
pub struct ProjectConsumerConfig {
  /// Outputs to skip during aggregation.
  /// Default: `["legacyPackages"]`.
  pub exclude_outputs: Vec<String>,
  /// Whether to include system configurations (nixos/darwin/home).
  /// Disable when building on a different system than the target.
  /// Default: true.
  pub include_configs: bool,
}

impl Default for ProjectConsumerConfig {
  fn default() -> Self {
    Self {
      exclude_outputs: vec!["legacyPackages".to_string()],
      include_configs: true,
    }
  }
}

// ---------------------------------------------------------------------------
// Layered multi-file config discovery
//
// Three tiers, each possibly itself multiple files, later files winning
// key-by-key (recursively into sub-tables, so e.g. a global `[doctor]`
// table and a repo-local `[doctor.rules]` table both survive rather than
// one replacing the other wholesale):
//
//   1. global:      sorted *.toml files in the XDG config dir for `xi` (same
//      directory `xi::config::default_config_path()` resolves to; `$XI_CONFIG`
//      overrides to a single explicit file instead of globbing its directory).
//   2. repo-shared:  ./xi.toml, then ./.xi.toml (first found; both do NOT merge
//      with each other -- pick one spelling), then every sorted *.toml in
//      ./.config/xi/.
//   3. repo-local:   ./.xi.local.toml (personal, meant to be gitignored).
//
// This loader is deliberately lenient end to end (unparseable or missing
// files are skipped with a warning, matching this function's existing
// posture for every section) -- it is NOT used for the stricter `[sources]`
// path below, which keeps reading a single repo-shared file only.
// ---------------------------------------------------------------------------

/// Every `*.toml` file directly inside `dir`, sorted by filename for a
/// deterministic merge order. Returns an empty list if `dir` doesn't
/// exist or can't be read.
fn glob_toml_files(dir: &Path) -> Vec<PathBuf> {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return vec![];
  };

  let mut files: Vec<PathBuf> = entries
    .filter_map(Result::ok)
    .map(|e| e.path())
    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
    .collect();
  files.sort();
  files
}

fn non_empty_env(name: &str) -> Option<String> {
  std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Resolve the global config directory the same way
/// `xi::config::default_config_path()` does (duplicated here rather than
/// shared: `xi-flake` cannot depend on the `xi` binary crate, and this is
/// a handful of lines, not worth a cross-crate refactor for).
fn global_config_dir() -> Option<PathBuf> {
  let base = non_empty_env("XDG_CONFIG_HOME")
    .map(PathBuf::from)
    .or_else(|| {
      non_empty_env("HOME").map(|h| PathBuf::from(h).join(".config"))
    })?;
  Some(base.join("xi"))
}

/// Tier 1: global config sources, in merge order.
fn global_config_sources() -> Vec<PathBuf> {
  if let Some(explicit) = non_empty_env("XI_CONFIG") {
    return vec![PathBuf::from(explicit)];
  }
  global_config_dir()
    .map(|dir| glob_toml_files(&dir))
    .unwrap_or_default()
}

/// The single repo-shared config file, if any: `xi.toml` takes
/// precedence over `.xi.toml` when both exist (first found wins, they do
/// not merge with each other).
fn repo_shared_config_file(dir: &Path) -> Option<PathBuf> {
  for name in ["xi.toml", ".xi.toml"] {
    let p = dir.join(name);
    if p.is_file() {
      return Some(p);
    }
  }
  None
}

/// Tier 2: repo-shared config sources, in merge order -- the single
/// `xi.toml`/`.xi.toml` file (if any), then every `.config/xi/*.toml`
/// fragment.
fn repo_shared_config_sources(dir: &Path) -> Vec<PathBuf> {
  let mut sources = Vec::new();
  if let Some(shared) = repo_shared_config_file(dir) {
    sources.push(shared);
  }
  sources.extend(glob_toml_files(&dir.join(".config").join("xi")));
  sources
}

/// Tier 3: the personal, repo-local override file, if present.
fn repo_local_config_source(dir: &Path) -> Option<PathBuf> {
  let p = dir.join(".xi.local.toml");
  p.is_file().then_some(p)
}

/// Every config file that contributes to [`load_project_config`], ordered
/// lowest precedence first.
///
/// Tier 1 the global config, tier 2 the repo-shared files
/// (`xi.toml`/`.xi.toml` plus `.config/xi/*.toml`), tier 3 the gitignored
/// repo-local override.
fn config_source_paths(flake_dir: &Path) -> Vec<PathBuf> {
  let mut sources = global_config_sources();
  sources.extend(repo_shared_config_sources(flake_dir));
  if let Some(local) = repo_local_config_source(flake_dir) {
    sources.push(local);
  }
  sources
}

/// Load project config, applying every `.xi.toml`-family layer in
/// precedence order (see [`config_source_paths`] for the exact tiers).
pub fn load_project_config(flake_dir: Option<&Path>) -> ProjectConfig {
  let dir = flake_dir.unwrap_or_else(|| Path::new("."));
  let mut config = default_config();

  // Every tier, lowest precedence first: the global config, then the
  // repo-shared files (`xi.toml`/`.xi.toml` plus `.config/xi/*.toml`), then
  // the gitignored `.xi.local.toml` carrying machine state that would be
  // wrong on every other machine and in CI.
  //
  // Applied one layer at a time rather than deep-merged into a single
  // document first, because the `[build]` handler below tags each
  // `override_input` entry with the file it came from: a relative path
  // resolves against that file's own directory, so per-layer identity is
  // load-bearing and a merged document would destroy it.
  for config_path in config_source_paths(dir) {
    let file_name = config_path
      .file_name()
      .map(|f| f.to_string_lossy().into_owned())
      .unwrap_or_default();
    let file = file_name.as_str();

    let Ok(raw) = std::fs::read_to_string(&config_path) else {
      continue;
    };

    let doc = match raw.parse::<toml_edit::DocumentMut>() {
      Ok(d) => d,
      Err(e) => {
        warn!("Failed to parse {}: {e}", config_path.display());
        continue;
      },
    };

    apply_project_document(&mut config, &doc, &raw, &config_path, dir, file);
  }

  debug!(?config, "Loaded project config");
  config
}

/// Apply one project file on top of `config`. Called for `.xi.toml` first,
/// then `.xi.local.toml`, so later values win.
fn apply_project_document(
  config: &mut ProjectConfig,
  doc: &toml_edit::DocumentMut,
  raw: &str,
  config_path: &Path,
  dir: &Path,
  file: &str,
) {
  // [ci] section
  if let Some(ci) = doc.get("ci").and_then(|v| v.as_table()) {
    if let Some(backend) = ci.get("backend").and_then(|v| v.as_str()) {
      config.ci.backend = match backend {
        "devour-flake" => crate::args::CiBackend::DevourFlake,
        "nix-fast-build" => crate::args::CiBackend::NixFastBuild,
        _ => crate::args::CiBackend::Auto,
      };
    }
    if let Some(outputs) = ci.get("extra-outputs").and_then(|v| v.as_array()) {
      for item in outputs {
        if let Some(s) = item.as_str() {
          config.ci.extra_outputs.push(s.to_string());
        }
      }
    }
  }

  // [doctor] section
  if let Some(doctor) = doc.get("doctor").and_then(|v| v.as_table()) {
    if let Some(age) = doctor
      .get("max-input-age-days")
      .and_then(toml_edit::Item::as_integer)
      && age > 0
    {
      #[allow(clippy::cast_sign_loss)]
      {
        config.doctor.max_input_age_days = Some(age as u64);
      }
    }

    if let Some(official) = doctor
      .get("require-official-nixpkgs")
      .and_then(toml_edit::Item::as_bool)
    {
      config.doctor.require_official_nixpkgs = official;
    }

    if let Some(branches) =
      doctor.get("supported-branches").and_then(|v| v.as_array())
    {
      config.doctor.supported_branches = branches
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    }

    if let Some(disable) = doctor
      .get("disable-structure-checks")
      .and_then(toml_edit::Item::as_bool)
    {
      config.doctor.disable_structure_checks = disable;
    }

    if let Some(disable) = doctor
      .get("disable-metadata-checks")
      .and_then(toml_edit::Item::as_bool)
    {
      config.doctor.disable_metadata_checks = disable;
    }

    if let Some(disable) = doctor
      .get("disable-output-checks")
      .and_then(toml_edit::Item::as_bool)
    {
      config.doctor.disable_output_checks = disable;
    }

    // [doctor.rules] -- one entry per check code, e.g.
    // `rules.dirty-tree = "off"` or `rules.package-version = false`.
    if let Some(rules) = doctor.get("rules").and_then(|v| v.as_table()) {
      for (code, item) in rules {
        let setting = if let Some(b) = item.as_bool() {
          if b {
            continue; // "on" is the default -- not stored as an override.
          }
          RuleSetting::Off
        } else if let Some(s) = item.as_str() {
          match s {
            "off" => RuleSetting::Off,
            "warn" => RuleSetting::Warn,
            "fail" => RuleSetting::Fail,
            "on" => continue,
            other => {
              warn!(
                "[doctor.rules.{code}] unknown setting '{other}' (expected a \
                 bool, or one of \"off\"/\"warn\"/\"fail\"/\"on\"); ignoring"
              );
              continue;
            },
          }
        } else {
          warn!("[doctor.rules.{code}] must be a bool or a string; ignoring");
          continue;
        };
        config.doctor.rules.insert(code.to_string(), setting);
      }
    }
  }

  // [fmt] section
  if let Some(fmt) = doc.get("fmt").and_then(|v| v.as_table())
    && let Some(backend) = fmt.get("backend").and_then(|v| v.as_str())
  {
    config.fmt.backend = crate::args::FmtBackend(backend.to_string());
  }

  // [test] section
  if let Some(test) = doc.get("test").and_then(|v| v.as_table()) {
    if let Some(backends) = test.get("backends").and_then(|v| v.as_array()) {
      config.test.backends = backends
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    }

    if let Some(rt) = test.get("runTests").and_then(|v| v.as_table())
      && let Some(attr) = rt.get("attr").and_then(|v| v.as_str())
    {
      config.test.run_tests_attr = attr.to_string();
    }

    if let Some(checks) = test.get("checks").and_then(|v| v.as_table())
      && let Some(filter) = checks.get("filter").and_then(|v| v.as_str())
    {
      config.test.checks_filter = filter.to_string();
    }

    if let Some(nu) = test.get("nix-unit").and_then(|v| v.as_table())
      && let Some(dir) = nu.get("test-dir").and_then(|v| v.as_str())
    {
      config.test.nix_unit_test_dir = dir.to_string();
    }

    if let Some(nixt) = test.get("nixt").and_then(|v| v.as_table())
      && let Some(dir) = nixt.get("test-dir").and_then(|v| v.as_str())
    {
      config.test.nixt_test_dir = dir.to_string();
    }

    // [[test.custom]] array of tables
    if let Some(custom) =
      test.get("custom").and_then(|v| v.as_array_of_tables())
    {
      for entry in custom {
        let name = entry.get("name").and_then(|v| v.as_str());
        let command = entry.get("command").and_then(|v| v.as_str());
        if let (Some(name), Some(command)) = (name, command) {
          let args = entry
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
              arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
            })
            .unwrap_or_default();

          config.test.custom.push(CustomTestBackend {
            name: name.to_string(),
            command: command.to_string(),
            args,
          });
        }
      }
    }
  }

  // [consumer] section
  if let Some(consumer) = doc.get("consumer").and_then(|v| v.as_table()) {
    if let Some(exclude) =
      consumer.get("exclude-outputs").and_then(|v| v.as_array())
    {
      config.consumer.exclude_outputs = exclude
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    }

    if let Some(include) = consumer
      .get("include-configs")
      .and_then(toml_edit::Item::as_bool)
    {
      config.consumer.include_configs = include;
    }
  }

  // [sources] section.
  //
  // The schema, the closed key set and the validation live in
  // `xi_core::sources::config` so this project file and the
  // Nix-module-written per-scope `sources.toml` cannot drift apart.
  //
  // This call site is deliberately lenient, matching the rest of this
  // function: a `[sources]` typo must not break `xi build` for someone who
  // never enabled sources. Commands that actually read sources config go
  // through `load_sources_config`, which is strict  -  that is what makes
  // the spec's "fails at config-load time, not at first fetch" hold.
  // Only when the file actually carries a `[sources]` table, otherwise a
  // `.xi.local.toml` without one would reset what `.xi.toml` configured.
  if doc.get("sources").is_some() {
    match SourcesConfig::from_toml_str(config_path, raw) {
      Ok(sources) => config.sources = sources,
      Err(e) => warn!("{e}"),
    }
  }

  // [build] section. New: `ProjectConfig` had no build settings before.
  //
  // Each value is tagged with the file it came from, because a relative
  // path resolves against that file's own directory. Recording the source
  // here is what makes the normalizer correct; it cannot be bolted on later.
  if let Some(build) = doc.get("build").and_then(|v| v.as_table())
    && let Some(item) = build.get("override_input")
  {
    let source = if file == ".xi.local.toml" {
      xi_core::override_input::OverrideSource::ProjectLocalToml {
        dir: dir.to_path_buf(),
      }
    } else {
      xi_core::override_input::OverrideSource::ProjectToml {
        dir: dir.to_path_buf(),
      }
    };

    let pairs: Vec<(String, String)> = match item {
      toml_edit::Item::Value(toml_edit::Value::InlineTable(t)) => {
        t.iter()
          .filter_map(|(k, v)| {
            v.as_str().map(|s| (k.to_string(), s.to_string()))
          })
          .collect()
      },
      toml_edit::Item::Table(t) => {
        t.iter()
          .filter_map(|(k, v)| {
            v.as_str().map(|s| (k.to_string(), s.to_string()))
          })
          .collect()
      },
      _ => Vec::new(),
    };

    for (name, value) in pairs {
      // A later file replaces an earlier one per input name.
      config.build.override_input.retain(|e| e.name != name);
      config
        .build
        .override_input
        .push(xi_core::override_input::Entry::new(
          name,
          value,
          source.clone(),
        ));
    }
  }
}

/// Load and validate the `.xi.toml [sources]` section, strictly.
///
/// Unlike `load_project_config`, a malformed `[sources]` block is an
/// error rather than a warning: an operator who misspells `remote` must
/// find out at config-load time, not when a gitops daemon silently never
/// advances. A missing `.xi.toml`, or one without a `[sources]` table, is
/// not malformed  -  both yield an empty layer.
///
/// # Errors
///
/// Returns [`ConfigError::File`] naming the file and the 1-based line of
/// the offending key.
pub fn load_sources_config(
  flake_dir: Option<&Path>,
) -> Result<SourcesConfig, ConfigError> {
  let dir = flake_dir.unwrap_or_else(|| Path::new("."));
  let path =
    repo_shared_config_file(dir).unwrap_or_else(|| dir.join(".xi.toml"));
  SourcesConfig::from_toml_file(&path)
}

fn default_config() -> ProjectConfig {
  ProjectConfig {
    build:    ProjectBuildConfig::default(),
    ci:       ProjectCiConfig::default(),
    consumer: ProjectConsumerConfig::default(),
    doctor:   ProjectDoctorConfig {
      max_input_age_days:       None,
      require_official_nixpkgs: true,
      supported_branches:       vec![],
      disable_structure_checks: false,
      disable_metadata_checks:  false,
      disable_output_checks:    false,
      rules:                    HashMap::new(),
    },
    test:     ProjectTestConfig::default(),
    fmt:      ProjectFmtConfig::default(),
    sources:  SourcesConfig::default(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn write(dir: &Path, name: &str, contents: &str) {
    if let Some(parent) = dir.join(name).parent() {
      std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(dir.join(name), contents).expect("write fixture file");
  }

  /// Pins `HOME`/`XDG_CONFIG_HOME` to an empty temp dir for the duration
  /// of a test, so `load_project_config`'s global tier reads nothing from
  /// whatever real config happens to exist on the machine running the
  /// test. Restores the previous values on drop.
  struct GlobalConfigGuard {
    empty_dir:  tempfile::TempDir,
    prior_xi:   Option<std::ffi::OsString>,
    prior_xdg:  Option<std::ffi::OsString>,
    prior_home: Option<std::ffi::OsString>,
  }

  impl GlobalConfigGuard {
    fn new() -> Self {
      let empty_dir = tempfile::tempdir().expect("create empty global dir");
      let prior_xi = std::env::var_os("XI_CONFIG");
      let prior_xdg = std::env::var_os("XDG_CONFIG_HOME");
      let prior_home = std::env::var_os("HOME");
      // SAFETY-equivalent note: this mutates process-wide env state, same
      // as `xi::config`'s own `EnvGuard` test helper -- acceptable here
      // because every test using it is `#[test]`, single-process, and
      // restores the prior value on drop.
      unsafe {
        std::env::remove_var("XI_CONFIG");
        std::env::set_var("XDG_CONFIG_HOME", empty_dir.path());
        std::env::remove_var("HOME");
      }
      Self {
        empty_dir,
        prior_xi,
        prior_xdg,
        prior_home,
      }
    }
  }

  impl Drop for GlobalConfigGuard {
    fn drop(&mut self) {
      unsafe {
        match &self.prior_xi {
          Some(v) => std::env::set_var("XI_CONFIG", v),
          None => std::env::remove_var("XI_CONFIG"),
        }
        match &self.prior_xdg {
          Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
          None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match &self.prior_home {
          Some(v) => std::env::set_var("HOME", v),
          None => std::env::remove_var("HOME"),
        }
      }
      let _ = &self.empty_dir; // kept alive until here
    }
  }

  // -- repo-shared file discovery --------------------------------------

  #[test]
  fn repo_shared_prefers_xi_toml_over_dot_xi_toml() {
    let dir = tempfile::tempdir().expect("create tempdir");
    write(dir.path(), "xi.toml", "");
    write(dir.path(), ".xi.toml", "");

    let found = repo_shared_config_file(dir.path()).expect("one file found");
    assert_eq!(found.file_name().and_then(|n| n.to_str()), Some("xi.toml"));
  }

  #[test]
  fn repo_shared_falls_back_to_dot_xi_toml() {
    let dir = tempfile::tempdir().expect("create tempdir");
    write(dir.path(), ".xi.toml", "");

    let found = repo_shared_config_file(dir.path()).expect("one file found");
    assert_eq!(found.file_name().and_then(|n| n.to_str()), Some(".xi.toml"));
  }

  #[test]
  fn repo_shared_sources_include_config_dir_fragments() {
    let dir = tempfile::tempdir().expect("create tempdir");
    write(dir.path(), ".xi.toml", "");
    write(dir.path(), ".config/xi/a.toml", "");
    write(dir.path(), ".config/xi/b.toml", "");
    write(dir.path(), ".config/xi/not-toml.txt", "");

    let sources = repo_shared_config_sources(dir.path());
    let names: Vec<String> = sources
      .iter()
      .filter_map(|p| p.file_name()?.to_str().map(String::from))
      .collect();

    assert_eq!(
      names,
      vec![".xi.toml", "a.toml", "b.toml"],
      "the single shared file comes first, then fragment-dir *.toml files in \
       sorted order, with non-.toml files excluded: {names:?}"
    );
  }

  // -- repo-local override file ----------------------------------------

  #[test]
  fn repo_local_source_absent_when_no_file() {
    let dir = tempfile::tempdir().expect("create tempdir");
    assert!(repo_local_config_source(dir.path()).is_none());
  }

  #[test]
  fn repo_local_source_present_when_file_exists() {
    let dir = tempfile::tempdir().expect("create tempdir");
    write(dir.path(), ".xi.local.toml", "");
    assert!(repo_local_config_source(dir.path()).is_some());
  }

  // -- [doctor.rules] parsing, end to end via load_project_config -------

  #[test]
  fn rules_table_bool_false_maps_to_off() {
    let _guard = GlobalConfigGuard::new();
    let dir = tempfile::tempdir().expect("create tempdir");
    write(
      dir.path(),
      ".xi.toml",
      "[doctor.rules]\nflake-description = false\n",
    );

    let config = load_project_config(Some(dir.path()));
    assert_eq!(
      config.doctor.rules.get("flake-description"),
      Some(&RuleSetting::Off)
    );
  }

  #[test]
  fn rules_table_string_settings_parse() {
    let _guard = GlobalConfigGuard::new();
    let dir = tempfile::tempdir().expect("create tempdir");
    write(
      dir.path(),
      ".xi.toml",
      "[doctor.rules]\na = \"off\"\nb = \"warn\"\nc = \"fail\"\n",
    );

    let config = load_project_config(Some(dir.path()));
    assert_eq!(config.doctor.rules.get("a"), Some(&RuleSetting::Off));
    assert_eq!(config.doctor.rules.get("b"), Some(&RuleSetting::Warn));
    assert_eq!(config.doctor.rules.get("c"), Some(&RuleSetting::Fail));
  }

  #[test]
  fn rules_table_true_and_on_are_not_stored() {
    let _guard = GlobalConfigGuard::new();
    let dir = tempfile::tempdir().expect("create tempdir");
    write(
      dir.path(),
      ".xi.toml",
      "[doctor.rules]\na = true\nb = \"on\"\n",
    );

    let config = load_project_config(Some(dir.path()));
    assert!(
      config.doctor.rules.is_empty(),
      "explicit 'on'/true is the default -- it must not be stored as an \
       override: {:?}",
      config.doctor.rules
    );
  }

  #[test]
  fn rules_table_unknown_string_is_ignored() {
    let _guard = GlobalConfigGuard::new();
    let dir = tempfile::tempdir().expect("create tempdir");
    write(
      dir.path(),
      ".xi.toml",
      "[doctor.rules]\na = \"not-a-real-setting\"\n",
    );

    let config = load_project_config(Some(dir.path()));
    assert!(config.doctor.rules.get("a").is_none());
  }

  #[test]
  fn repo_local_overrides_repo_shared_for_the_same_rule() {
    let _guard = GlobalConfigGuard::new();
    let dir = tempfile::tempdir().expect("create tempdir");
    write(
      dir.path(),
      ".xi.toml",
      "[doctor.rules]\ndirty-tree = \"warn\"\n",
    );
    write(
      dir.path(),
      ".xi.local.toml",
      "[doctor.rules]\ndirty-tree = \"off\"\n",
    );

    let config = load_project_config(Some(dir.path()));
    assert_eq!(
      config.doctor.rules.get("dirty-tree"),
      Some(&RuleSetting::Off),
      "the repo-local layer must win over the repo-shared layer for the same \
       key"
    );
  }
}
