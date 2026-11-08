use std::path::Path;

use tracing::{debug, warn};
use xi_core::sources::config::{ConfigError, SourcesConfig};

/// Project-level configuration from `.xi.toml`.
#[derive(Debug, Default)]
pub struct ProjectConfig {
  pub ci:       ProjectCiConfig,
  pub consumer: ProjectConsumerConfig,
  pub doctor:   ProjectDoctorConfig,
  pub test:     ProjectTestConfig,
  pub fmt:      ProjectFmtConfig,
  pub sources:  SourcesConfig,
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

/// Load project config from `.xi.toml` in the given directory.
pub fn load_project_config(flake_dir: Option<&Path>) -> ProjectConfig {
  let dir = flake_dir.unwrap_or_else(|| Path::new("."));
  let config_path = dir.join(".xi.toml");

  let Ok(raw) = std::fs::read_to_string(&config_path) else {
    return default_config();
  };

  let doc = match raw.parse::<toml_edit::DocumentMut>() {
    Ok(d) => d,
    Err(e) => {
      warn!("Failed to parse {}: {e}", config_path.display());
      return default_config();
    },
  };

  let mut config = default_config();

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
  match SourcesConfig::from_toml_str(&config_path, &raw) {
    Ok(sources) => config.sources = sources,
    Err(e) => warn!("{e}"),
  }

  debug!(?config, "Loaded project config from .xi.toml");
  config
}

/// Load and validate the `.xi.toml [sources]` section, strictly.
///
/// Unlike `load_project_config`, which is crate-private and warns, a
/// malformed `[sources]` block is an
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
  SourcesConfig::from_toml_file(&dir.join(".xi.toml"))
}

fn default_config() -> ProjectConfig {
  ProjectConfig {
    ci:       ProjectCiConfig::default(),
    consumer: ProjectConsumerConfig::default(),
    doctor:   ProjectDoctorConfig {
      max_input_age_days:       None,
      require_official_nixpkgs: true,
      supported_branches:       vec![],
      disable_structure_checks: false,
      disable_metadata_checks:  false,
      disable_output_checks:    false,
    },
    test:     ProjectTestConfig::default(),
    fmt:      ProjectFmtConfig::default(),
    sources:  SourcesConfig::default(),
  }
}
