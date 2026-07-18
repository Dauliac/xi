use std::path::Path;

use tracing::{debug, warn};

/// Project-level configuration from `.xi.toml`.
#[derive(Debug, Default)]
pub struct ProjectConfig {
  pub ci: ProjectCiConfig,
  pub consumer: ProjectConsumerConfig,
  pub doctor: ProjectDoctorConfig,
  pub test: ProjectTestConfig,
  pub fmt: ProjectFmtConfig,
  pub materialize: ProjectMaterializeConfig,
}

/// Formatter configuration.
#[derive(Debug, Default)]
pub struct ProjectFmtConfig {
  /// Formatter backend (auto, flake, nixfmt, alejandra, treefmt).
  pub backend: crate::args::FmtBackend,
}

/// CI-specific project configuration.
#[derive(Debug, Default)]
pub struct ProjectCiConfig {
  /// Build backend preference (auto, devour-flake, nix-fast-build).
  pub backend: crate::args::CiBackend,
  /// Extra flake output paths to discover and build/eval.
  pub extra_outputs: Vec<String>,
}

/// Test configuration.
#[derive(Debug)]
pub struct ProjectTestConfig {
  /// Which backends to run. Empty = auto-detect all.
  pub backends: Vec<String>,
  /// Flake attribute for `runTests` backend (per-system).
  pub run_tests_attr: String,
  /// Glob filter on check derivation names.
  pub checks_filter: String,
  /// Test directory for nix-unit standalone CLI.
  pub nix_unit_test_dir: String,
  /// Test directory for nixt.
  pub nixt_test_dir: String,
  /// Custom command-based backends.
  pub custom: Vec<CustomTestBackend>,
}

/// A user-defined test backend that shells out to a command.
#[derive(Debug, Clone)]
pub struct CustomTestBackend {
  pub name: String,
  pub command: String,
  pub args: Vec<String>,
}

impl Default for ProjectTestConfig {
  fn default() -> Self {
    Self {
      backends: Vec::new(),
      run_tests_attr: "tests".to_string(),
      checks_filter: String::new(),
      nix_unit_test_dir: "tests/".to_string(),
      nixt_test_dir: "tests/".to_string(),
      custom: Vec::new(),
    }
  }
}

/// Doctor/health-check configuration.
#[derive(Debug, Default)]
pub struct ProjectDoctorConfig {
  /// Maximum input age in days before warning. Default: 30.
  pub max_input_age_days: Option<u64>,
  /// Whether to warn on nixpkgs forks. Default: true.
  pub require_official_nixpkgs: bool,
  /// Override the list of supported nixpkgs branches.
  pub supported_branches: Vec<String>,
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

/// Materialization configuration.
///
/// Controls caching of expensive eval-time computations.
/// Each target is independently generated and cached.
#[derive(Debug)]
pub struct ProjectMaterializeConfig {
  /// Base path for committed materialized files (AOT mode).
  /// Default: `"nix/materialized"`.
  pub commit_path: String,
  /// Verify freshness in CI (`xi ci`).
  pub check_in_ci: bool,
  /// Apply skip-worktree to committed materialized files so they
  /// don't appear in `git status`. Default: true.
  pub git_hide: bool,
  /// Automatically run `xi materialize` before build/ci commands.
  /// Only re-runs stale targets. Default: false.
  pub pre_build: bool,
  /// After `--commit`, automatically `git add` the materialized files.
  /// Default: false.
  pub auto_stage: bool,
  /// Restrict auto-stage to specific branches. Empty = all branches.
  pub auto_stage_branches: Vec<String>,
  /// Individual materialization targets.
  pub targets: Vec<MaterializeTarget>,
}

impl Default for ProjectMaterializeConfig {
  fn default() -> Self {
    Self {
      commit_path: "nix/materialized".to_string(),
      check_in_ci: false,
      git_hide: true,
      pre_build: false,
      auto_stage: false,
      auto_stage_branches: Vec::new(),
      targets: Vec::new(),
    }
  }
}

/// A single materialization target.
#[derive(Debug, Clone)]
pub struct MaterializeTarget {
  /// Human-readable name (e.g. "cargo-hash").
  pub name: String,
  /// Shell command to produce the output. Runs in project root.
  /// Stdout is captured and written to the output file.
  pub command: String,
  /// Output path relative to the materialization base directory.
  /// Trailing `/` means directory output (command writes to
  /// `$XI_MATERIALIZE_OUT`).
  pub output: String,
  /// Glob patterns of files whose content determines cache validity.
  /// Hash of these files is compared to decide if re-materialization
  /// is needed.
  pub sources: Vec<String>,
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
  }

  // [fmt] section
  if let Some(fmt) = doc.get("fmt").and_then(|v| v.as_table())
    && let Some(backend) = fmt.get("backend").and_then(|v| v.as_str())
  {
    config.fmt.backend = match backend {
      "flake" => crate::args::FmtBackend::Flake,
      "nixfmt" => crate::args::FmtBackend::Nixfmt,
      "alejandra" => crate::args::FmtBackend::Alejandra,
      "treefmt" => crate::args::FmtBackend::Treefmt,
      _ => crate::args::FmtBackend::Auto,
    };
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

  // [materialize] section
  if let Some(mat) = doc.get("materialize").and_then(|v| v.as_table()) {
    if let Some(path) = mat.get("commit-path").and_then(|v| v.as_str()) {
      config.materialize.commit_path = path.to_string();
    }

    if let Some(v) = mat.get("check-in-ci").and_then(toml_edit::Item::as_bool) {
      config.materialize.check_in_ci = v;
    }

    if let Some(v) = mat.get("git-hide").and_then(toml_edit::Item::as_bool) {
      config.materialize.git_hide = v;
    }

    if let Some(v) = mat.get("pre-build").and_then(toml_edit::Item::as_bool) {
      config.materialize.pre_build = v;
    }

    if let Some(v) = mat.get("auto-stage").and_then(toml_edit::Item::as_bool) {
      config.materialize.auto_stage = v;
    }

    if let Some(branches) =
      mat.get("auto-stage-branches").and_then(|v| v.as_array())
    {
      config.materialize.auto_stage_branches = branches
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    }

    // [[materialize.target]] array of tables
    if let Some(targets) =
      mat.get("target").and_then(|v| v.as_array_of_tables())
    {
      for entry in targets {
        let name = entry.get("name").and_then(|v| v.as_str());
        let command = entry.get("command").and_then(|v| v.as_str());
        let output = entry.get("output").and_then(|v| v.as_str());

        if let (Some(name), Some(command), Some(output)) =
          (name, command, output)
        {
          let sources = entry
            .get("sources")
            .and_then(|v| v.as_array())
            .map(|arr| {
              arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
            })
            .unwrap_or_default();

          config.materialize.targets.push(MaterializeTarget {
            name: name.to_string(),
            command: command.to_string(),
            output: output.to_string(),
            sources,
          });
        }
      }
    }
  }

  debug!(?config, "Loaded project config from .xi.toml");
  config
}

fn default_config() -> ProjectConfig {
  ProjectConfig {
    ci: ProjectCiConfig::default(),
    consumer: ProjectConsumerConfig::default(),
    doctor: ProjectDoctorConfig {
      max_input_age_days: None,
      require_official_nixpkgs: true,
      supported_branches: vec![],
    },
    test: ProjectTestConfig::default(),
    fmt: ProjectFmtConfig::default(),
    materialize: ProjectMaterializeConfig::default(),
  }
}
