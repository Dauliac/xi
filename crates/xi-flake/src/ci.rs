use std::path::Path;
use std::time::Instant;

use color_eyre::Result;
use color_eyre::eyre::bail;
use nix_command::{CommandKind, NixCommand};
use tracing::{debug, info};
use xi_core::progress;
use yansi::{Color, Paint};

use crate::args::{CiArgs, CiBackend, NixPassthroughArgs};
use crate::project_config;
use crate::test;
use crate::{
  build_all_for_flake_ref, build_all_nix_fast_build, discover_subflakes,
  ensure_flake_locked, execute_build, resolve_backend, resolve_local_flake_dir,
};

// ---------------------------------------------------------------------------
// Step result tracking
// ---------------------------------------------------------------------------

struct StepResult {
  name: &'static str,
  status: StepStatus,
  duration: std::time::Duration,
  detail: Option<String>,
}

#[allow(dead_code)]
enum StepStatus {
  Ok,
  Warn(String),
  Fail(String),
  Skipped,
}

impl StepResult {
  const fn is_failure(&self) -> bool {
    matches!(self.status, StepStatus::Fail(_))
  }
}

// ---------------------------------------------------------------------------
// Phase 1 steps (quiet — capture output, show spinner)
// ---------------------------------------------------------------------------

/// Step: verify flake.lock is in sync (quiet — captures output).
fn step_lock_check(flake_ref: &str) -> StepResult {
  let start = Instant::now();

  let cmd = NixCommand::new(CommandKind::Flake)
    .arg("lock")
    .arg("--no-update-lock-file")
    .arg(flake_ref);

  match cmd.output() {
    Ok(output) => {
      if output.status.success() {
        StepResult {
          name: "Lock check",
          status: StepStatus::Ok,
          duration: start.elapsed(),
          detail: None,
        }
      } else {
        StepResult {
          name: "Lock check",
          status: StepStatus::Fail(
            "flake.lock is out of sync. Run `nix flake lock` to update."
              .to_string(),
          ),
          duration: start.elapsed(),
          detail: None,
        }
      }
    },
    Err(e) => StepResult {
      name: "Lock check",
      status: StepStatus::Fail(format!(
        "nix flake lock --no-update-lock-file failed: {e}"
      )),
      duration: start.elapsed(),
      detail: None,
    },
  }
}

/// Step: run flake health checks (input freshness, branch, source).
///
/// Health issues are reported as warnings, not failures, so they don't
/// block the CI pipeline by default.
fn step_health_check(
  flake_ref: &str,
  flake_dir: Option<&Path>,
  doctor_config: &project_config::ProjectDoctorConfig,
) -> StepResult {
  let start = Instant::now();

  let Some(dir) = flake_dir else {
    return StepResult {
      name: "Health check",
      status: StepStatus::Skipped,
      duration: start.elapsed(),
      detail: Some("remote flake, skipping health check".to_string()),
    };
  };

  match crate::doctor::run_health_checks(dir, flake_ref, doctor_config) {
    Ok(checks) => {
      let warnings = checks.iter().filter(|c| c.is_warn()).count();
      let failures = checks.iter().filter(|c| c.is_fail()).count();

      if failures > 0 {
        StepResult {
          name: "Health check",
          status: StepStatus::Warn(format!(
            "{failures} issue(s), {warnings} warning(s). Run `xi doctor` for details."
          )),
          duration: start.elapsed(),
          detail: None,
        }
      } else if warnings > 0 {
        StepResult {
          name: "Health check",
          status: StepStatus::Warn(format!(
            "{warnings} warning(s). Run `xi doctor` for details."
          )),
          duration: start.elapsed(),
          detail: None,
        }
      } else {
        StepResult {
          name: "Health check",
          status: StepStatus::Ok,
          duration: start.elapsed(),
          detail: None,
        }
      }
    },
    Err(e) => StepResult {
      name: "Health check",
      status: StepStatus::Warn(format!("Could not run health checks: {e}")),
      duration: start.elapsed(),
      detail: None,
    },
  }
}

/// Step: run eval-time tests (runTests backend) if detected.
#[allow(clippy::option_if_let_else)]
fn step_eval_tests(flake_ref: &str, test_attr: &str) -> StepResult {
  let start = Instant::now();

  match test::run_ci_eval_tests(flake_ref, test_attr) {
    None => StepResult {
      name: "Eval tests",
      status: StepStatus::Skipped,
      duration: start.elapsed(),
      detail: Some("no runTests attribute detected".to_string()),
    },
    Some(ci_result) => {
      let total = ci_result.passed + ci_result.failed + ci_result.errors;

      if ci_result.failed > 0 || ci_result.errors > 0 {
        StepResult {
          name: "Eval tests",
          status: StepStatus::Fail(format!(
            "{} passed, {} failed, {} errors ({total} total)",
            ci_result.passed, ci_result.failed, ci_result.errors
          )),
          duration: start.elapsed(),
          detail: Some(
            "Run `xi test --backend run-tests` for details".to_string(),
          ),
        }
      } else {
        StepResult {
          name: "Eval tests",
          status: StepStatus::Ok,
          duration: start.elapsed(),
          detail: Some(format!("{total} test(s) passed")),
        }
      }
    },
  }
}

/// Step: eval all systems via `nix flake show --all-systems --json`.
///
/// Returns the parsed JSON and a list of discovered extra derivation paths.
fn step_eval_all_systems(
  flake_ref: &str,
  all_systems: bool,
  no_ifd: bool,
  extra_output_names: &[String],
) -> (StepResult, Option<serde_json::Value>, Vec<String>) {
  let label = if all_systems {
    "Eval all systems"
  } else {
    "Eval current system"
  };

  let start = Instant::now();

  let mut cmd = NixCommand::new(CommandKind::Flake)
    .arg("show")
    .arg("--json");

  if all_systems {
    cmd = cmd.arg("--all-systems");
  }

  if no_ifd {
    cmd = cmd
      .arg("--option")
      .arg("allow-import-from-derivation")
      .arg("false");
  }

  cmd = cmd.arg(flake_ref);

  debug!(argv = ?cmd.argv());

  let output = match cmd.output() {
    Ok(o) => o,
    Err(e) => {
      return (
        StepResult {
          name: label,
          status: StepStatus::Fail(format!(
            "Failed to run nix flake show: {e}"
          )),
          duration: start.elapsed(),
          detail: None,
        },
        None,
        vec![],
      );
    },
  };

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let error_msg = stderr
      .lines()
      .rfind(|l| {
        let t = l.trim();
        !t.is_empty()
          && !t.starts_with("fetching ")
          && !t.starts_with("error (ignored)")
      })
      .unwrap_or("evaluation failed")
      .to_string();

    return (
      StepResult {
        name: label,
        status: StepStatus::Fail(error_msg),
        duration: start.elapsed(),
        detail: None,
      },
      None,
      vec![],
    );
  }

  let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
    Ok(v) => v,
    Err(e) => {
      return (
        StepResult {
          name: label,
          status: StepStatus::Fail(format!(
            "Failed to parse flake show output: {e}"
          )),
          duration: start.elapsed(),
          detail: None,
        },
        None,
        vec![],
      );
    },
  };

  let extra_paths = discover_extra_derivation_paths(&json, extra_output_names);

  let detail = if extra_paths.is_empty() {
    None
  } else {
    Some(format!(
      "Discovered {} extra output(s): {}",
      extra_paths.len(),
      extra_paths.join(", ")
    ))
  };

  (
    StepResult {
      name: label,
      status: StepStatus::Ok,
      duration: start.elapsed(),
      detail,
    },
    Some(json),
    extra_paths,
  )
}

/// Step: check materialization freshness (if check-in-ci is enabled).
fn step_materialize_check(flake_dir: Option<&Path>) -> StepResult {
  let start = Instant::now();

  let Some(dir) = flake_dir else {
    return StepResult {
      name: "Materialize",
      status: StepStatus::Skipped,
      duration: start.elapsed(),
      detail: None,
    };
  };

  match crate::materialize::check_materialize_freshness(dir) {
    Ok(None) => StepResult {
      name: "Materialize",
      status: StepStatus::Skipped,
      duration: start.elapsed(),
      detail: None,
    },
    Ok(Some((0, total))) => StepResult {
      name: "Materialize",
      status: StepStatus::Ok,
      duration: start.elapsed(),
      detail: Some(format!("{total} target(s) fresh")),
    },
    Ok(Some((stale, total))) => StepResult {
      name: "Materialize",
      status: StepStatus::Fail(format!(
        "{stale} of {total} target(s) are stale. Run `xi materialize`."
      )),
      duration: start.elapsed(),
      detail: None,
    },
    Err(e) => StepResult {
      name: "Materialize",
      status: StepStatus::Warn(format!("Could not check materialization: {e}")),
      duration: start.elapsed(),
      detail: None,
    },
  }
}

// ---------------------------------------------------------------------------
// Extra output discovery
// ---------------------------------------------------------------------------

/// Known standard outputs that devour-flake already handles.
const DEVOUR_HANDLED: &[&str] = &[
  "packages",
  "checks",
  "devShells",
  "apps",
  "nixosConfigurations",
  "darwinConfigurations",
  "legacyPackages",
];

/// Walk the flake show JSON and find derivation outputs matching the
/// requested extra output names that aren't already handled by
/// devour-flake.
fn discover_extra_derivation_paths(
  json: &serde_json::Value,
  extra_output_names: &[String],
) -> Vec<String> {
  let Some(root) = json.as_object() else {
    return vec![];
  };

  let mut paths = Vec::new();

  for name in extra_output_names {
    if DEVOUR_HANDLED.contains(&name.as_str()) {
      continue;
    }

    let Some(value) = root.get(name.as_str()) else {
      continue;
    };

    collect_derivation_paths(value, &format!(".#{name}"), &mut paths);
  }

  paths
}

/// Recursively collect attribute paths that are derivations.
fn collect_derivation_paths(
  value: &serde_json::Value,
  prefix: &str,
  paths: &mut Vec<String>,
) {
  let Some(obj) = value.as_object() else {
    return;
  };

  // Check if this node itself is a derivation
  if obj
    .get("type")
    .and_then(serde_json::Value::as_str)
    .is_some_and(|t| t == "derivation")
  {
    paths.push(prefix.to_string());
    return;
  }

  // Recurse into children
  for (key, child) in obj {
    collect_derivation_paths(child, &format!("{prefix}.{key}"), paths);
  }
}

/// Step: deeply evaluate `lib` outputs with `builtins.deepSeq`.
fn step_eval_lib(flake_ref: &str) -> StepResult {
  let start = Instant::now();

  // Quick check: does this flake even have a lib output?
  if !crate::flake_lib::has_lib_output(flake_ref) {
    return StepResult {
      name: "Eval lib",
      status: StepStatus::Skipped,
      duration: start.elapsed(),
      detail: None,
    };
  }

  match crate::flake_lib::eval_lib(flake_ref, false) {
    Ok(duration) => StepResult {
      name: "Eval lib",
      status: StepStatus::Ok,
      duration,
      detail: None,
    },
    Err(e) => StepResult {
      name: "Eval lib",
      status: StepStatus::Fail(format!("{e}")),
      duration: start.elapsed(),
      detail: None,
    },
  }
}

// ---------------------------------------------------------------------------
// Phase 2: build
// ---------------------------------------------------------------------------

/// Build extra outputs discovered from `nix flake show`.
fn build_extra_outputs(
  extra_paths: &[String],
  passthrough_args: &[String],
  no_nom: bool,
  dry: bool,
) -> StepResult {
  if extra_paths.is_empty() {
    return StepResult {
      name: "Extra outputs",
      status: StepStatus::Skipped,
      duration: std::time::Duration::ZERO,
      detail: None,
    };
  }

  let start = Instant::now();

  let mut cmd = NixCommand::new(CommandKind::Build).print_build_logs(false);

  for path in extra_paths {
    cmd = cmd.arg(path.as_str());
  }

  cmd = cmd.arg("--no-link").args(passthrough_args);

  if dry {
    cmd = cmd.arg("--dry-run");
  }

  match execute_build(&cmd, no_nom, dry) {
    Ok(()) => StepResult {
      name: "Extra outputs",
      status: StepStatus::Ok,
      duration: start.elapsed(),
      detail: Some(format!("Built {} output(s)", extra_paths.len())),
    },
    Err(e) => StepResult {
      name: "Extra outputs",
      status: StepStatus::Fail(format!("{e}")),
      duration: start.elapsed(),
      detail: None,
    },
  }
}

// ---------------------------------------------------------------------------
// Pipeline orchestration
// ---------------------------------------------------------------------------

impl CiArgs {
  /// Run the CI pipeline.
  ///
  /// # Errors
  ///
  /// Returns an error if any step fails (unless `--continue-on-error`).
  pub fn run(self) -> Result<()> {
    let flake_ref_owned =
      self.flake_ref.clone().unwrap_or_else(|| ".".to_string());
    let flake_ref = flake_ref_owned.as_str();
    let local_dir = resolve_local_flake_dir(Some(flake_ref));
    let all_systems = !self.current_system_only;
    let passthrough_args = self.passthrough.to_nix_args();

    // Pre-build materialization (if configured in .xi.toml)
    if let Some(ref dir) = local_dir {
      crate::materialize::run_pre_build_materialize(dir)?;
    }

    // Auto-create flake.lock if missing
    ensure_flake_locked(local_dir.clone())?;

    // Load .xi.toml project config
    let project_config =
      project_config::load_project_config(local_dir.as_deref());

    if self.recursive {
      return self.run_recursive(flake_ref);
    }

    println!();
    println!("  {}", Paint::new("Validate").bold());

    let validate_spinner = progress::spinner(format!(
      "    {} ...",
      Paint::new("Running checks").bold()
    ));

    // Phase 1: validation steps in parallel
    let phase1_results = {
      let flake_ref_owned = flake_ref.to_string();
      let no_lock = self.no_lock_check;
      let no_eval = self.no_eval;
      let no_health = self.no_health_check;
      let no_test = self.no_test;
      let no_lib = self.no_lib_eval;
      let no_ifd = self.no_ifd;
      let extra_names = project_config.ci.extra_outputs.clone();
      let doctor_config = project_config.doctor;
      let test_attr = project_config.test.run_tests_attr.clone();

      std::thread::scope(|s| {
        // Lock check
        let lock_handle = if no_lock {
          None
        } else {
          let ref_clone = flake_ref_owned.clone();
          Some(s.spawn(move || step_lock_check(&ref_clone)))
        };

        // Eval all systems
        let eval_handle = if no_eval {
          None
        } else {
          let ref_clone = flake_ref_owned.clone();
          Some(s.spawn(move || {
            step_eval_all_systems(&ref_clone, all_systems, no_ifd, &extra_names)
          }))
        };

        // Health check
        let health_handle = if no_health {
          None
        } else {
          let ref_clone = flake_ref_owned.clone();
          let local_clone = local_dir.clone();
          Some(s.spawn(move || {
            step_health_check(
              &ref_clone,
              local_clone.as_deref(),
              &doctor_config,
            )
          }))
        };

        // Eval-time tests (runTests)
        let test_handle = if no_test {
          None
        } else {
          let ref_clone = flake_ref_owned.clone();
          let attr_clone = test_attr;
          Some(s.spawn(move || step_eval_tests(&ref_clone, &attr_clone)))
        };

        // Eval lib (deepSeq)
        let lib_handle = if no_lib {
          None
        } else {
          let ref_clone = flake_ref_owned.clone();
          Some(s.spawn(move || step_eval_lib(&ref_clone)))
        };

        // Materialize freshness check (if check-in-ci = true)
        let mat_handle = {
          let local_clone = local_dir.clone();
          Some(s.spawn(move || step_materialize_check(local_clone.as_deref())))
        };

        // Collect results
        let lock_result = lock_handle.map(|h| {
          h.join().unwrap_or_else(|_| StepResult {
            name: "Lock check",
            status: StepStatus::Fail("Thread panicked".to_string()),
            duration: std::time::Duration::ZERO,
            detail: None,
          })
        });

        #[allow(clippy::option_if_let_else)]
        let (eval_result, _eval_json, extra_paths) =
          if let Some(h) = eval_handle {
            match h.join() {
              Ok((result, json, paths)) => (Some(result), json, paths),
              Err(_) => (
                Some(StepResult {
                  name: "Eval all systems",
                  status: StepStatus::Fail("Thread panicked".to_string()),
                  duration: std::time::Duration::ZERO,
                  detail: None,
                }),
                None,
                vec![],
              ),
            }
          } else {
            (None, None, vec![])
          };

        let health_result = health_handle.map(|h| {
          h.join().unwrap_or_else(|_| StepResult {
            name: "Health check",
            status: StepStatus::Fail("Thread panicked".to_string()),
            duration: std::time::Duration::ZERO,
            detail: None,
          })
        });

        let test_result = test_handle.map(|h| {
          h.join().unwrap_or_else(|_| StepResult {
            name: "Eval tests",
            status: StepStatus::Fail("Thread panicked".to_string()),
            duration: std::time::Duration::ZERO,
            detail: None,
          })
        });

        let lib_result = lib_handle.map(|h| {
          h.join().unwrap_or_else(|_| StepResult {
            name: "Eval lib",
            status: StepStatus::Fail("Thread panicked".to_string()),
            duration: std::time::Duration::ZERO,
            detail: None,
          })
        });

        let mat_result = mat_handle.map(|h| {
          h.join().unwrap_or_else(|_| StepResult {
            name: "Materialize",
            status: StepStatus::Fail("Thread panicked".to_string()),
            duration: std::time::Duration::ZERO,
            detail: None,
          })
        });

        (
          lock_result,
          eval_result,
          extra_paths,
          health_result,
          test_result,
          lib_result,
          mat_result,
        )
      })
    };

    let (
      lock_result,
      eval_result,
      extra_paths,
      health_result,
      test_result,
      lib_result,
      mat_result,
    ) = phase1_results;

    // Clear the spinner and print results sequentially
    validate_spinner.finish_and_clear();

    let mut all_results: Vec<StepResult> = Vec::new();

    if let Some(r) = lock_result {
      print_step_result(&r);
      all_results.push(r);
    }
    if let Some(r) = eval_result {
      print_step_result(&r);
      all_results.push(r);
    }
    if let Some(r) = health_result {
      print_step_result(&r);
      all_results.push(r);
    }
    if let Some(r) = test_result {
      print_step_result(&r);
      all_results.push(r);
    }
    if let Some(r) = lib_result {
      print_step_result(&r);
      all_results.push(r);
    }
    if let Some(r) = mat_result {
      print_step_result(&r);
      all_results.push(r);
    }

    // Check for phase 1 failures
    let phase1_failures = all_results.iter().filter(|r| r.is_failure()).count();
    if phase1_failures > 0 && !self.continue_on_error {
      println!();
      println!(
        "  {}",
        Paint::new(format!(
          "Validation failed ({phase1_failures} step(s)). \
           Skipping build."
        ))
        .fg(Color::Red)
        .bold()
      );
      bail!("{phase1_failures} validation step(s) failed");
    }

    // Phase 2: build
    println!();
    if self.no_build {
      println!("  {}", Paint::new("Build skipped (--no-build)").dim());
    } else {
      println!("  {}", Paint::new("Build").bold());

      let backend = resolve_backend(&self.backend, &project_config.ci.backend);

      let build_start = Instant::now();
      let using_nfb = matches!(backend, CiBackend::NixFastBuild);

      let (step_name, build_result) = match backend {
        CiBackend::NixFastBuild => {
          info!("Using nix-fast-build backend");
          (
            "Build all (nix-fast-build)",
            build_all_nix_fast_build(
              flake_ref,
              &passthrough_args,
              &self.extra_args,
              self.no_nom,
              self.dry,
              self.no_ifd,
            ),
          )
        },
        _ => (
          "Build all (devour-flake)",
          build_all_for_flake_ref(
            flake_ref,
            &passthrough_args,
            &self.extra_args,
            self.no_nom,
            self.dry,
          ),
        ),
      };

      let build_step = match build_result {
        Ok(()) => StepResult {
          name: step_name,
          status: StepStatus::Ok,
          duration: build_start.elapsed(),
          detail: None,
        },
        Err(e) => StepResult {
          name: step_name,
          status: StepStatus::Fail(format!("{e}")),
          duration: build_start.elapsed(),
          detail: None,
        },
      };

      print_step_result(&build_step);
      let build_failed = build_step.is_failure();
      all_results.push(build_step);

      if build_failed && !self.continue_on_error {
        bail!("Build failed");
      }

      // Extra outputs — only needed for devour-flake (nix-fast-build
      // already discovers and builds all outputs)
      if !using_nfb && !extra_paths.is_empty() {
        let extra_step = build_extra_outputs(
          &extra_paths,
          &passthrough_args,
          self.no_nom,
          self.dry,
        );
        print_step_result(&extra_step);
        let extra_failed = extra_step.is_failure();
        all_results.push(extra_step);

        if extra_failed && !self.continue_on_error {
          bail!("Extra outputs build failed");
        }
      }
    }

    // Summary
    let total_failures = all_results.iter().filter(|r| r.is_failure()).count();
    let total_passed = all_results
      .iter()
      .filter(|r| matches!(r.status, StepStatus::Ok))
      .count();
    let total_duration: std::time::Duration =
      all_results.iter().map(|r| r.duration).sum();

    println!();
    if total_failures == 0 {
      println!(
        "  {} {}",
        Paint::new(format!("All {total_passed} step(s) passed"))
          .fg(Color::Green)
          .bold(),
        Paint::new(format!("({:.1}s)", total_duration.as_secs_f64())).dim(),
      );
    } else {
      println!(
        "  {} {}",
        Paint::new(format!(
          "{total_failures} of {} step(s) failed",
          all_results.len()
        ))
        .fg(Color::Red)
        .bold(),
        Paint::new(format!("({:.1}s)", total_duration.as_secs_f64())).dim(),
      );
      bail!("{total_failures} CI step(s) failed");
    }

    println!();
    Ok(())
  }

  /// Run CI pipeline recursively on all discovered subflakes.
  fn run_recursive(self, root_ref: &str) -> Result<()> {
    let local_dir = resolve_local_flake_dir(Some(root_ref));
    let Some(ref base_dir) = local_dir else {
      bail!(
        "--recursive requires a local flake reference, \
         got remote ref: {root_ref}"
      );
    };

    let subflake_dirs = discover_subflakes(base_dir)?;
    if subflake_dirs.is_empty() {
      bail!("No flake.nix files found under {}", base_dir.display());
    }

    info!(
      "Found {} flake(s) under {}",
      subflake_dirs.len(),
      base_dir.display()
    );

    let mut errors = Vec::new();

    for dir in &subflake_dirs {
      let flake_ref = if dir == Path::new(".") || dir == base_dir {
        root_ref.to_string()
      } else {
        let relative =
          dir.strip_prefix(base_dir).unwrap_or(dir).to_string_lossy();
        format!("path:{root_ref}?dir={relative}")
      };

      println!(
        "\n  {}",
        Paint::new(format!("Subflake: {flake_ref}")).bold()
      );

      let sub_args = Self {
        flake_ref: Some(flake_ref.clone()),
        no_lock_check: self.no_lock_check,
        no_eval: self.no_eval,
        no_build: self.no_build,
        no_ifd: self.no_ifd,
        no_health_check: self.no_health_check,
        no_test: self.no_test,
        no_lib_eval: self.no_lib_eval,
        current_system_only: self.current_system_only,
        recursive: false,
        no_nom: self.no_nom,
        dry: self.dry,
        continue_on_error: self.continue_on_error,
        backend: self.backend.clone(),
        passthrough: NixPassthroughArgs::default(),
        extra_args: self.extra_args.clone(),
      };

      if let Err(e) = sub_args.run() {
        if !self.continue_on_error {
          return Err(e);
        }
        errors.push((flake_ref, e));
      }
    }

    if errors.is_empty() {
      info!("All {} subflake(s) passed CI", subflake_dirs.len());
      Ok(())
    } else {
      for (ref_name, err) in &errors {
        tracing::error!("CI failed for {ref_name}: {err}");
      }
      bail!(
        "{} of {} subflake(s) failed CI",
        errors.len(),
        subflake_dirs.len()
      );
    }
  }
}

/// Print a step result directly (for phase 2, no spinner).
fn print_step_result(result: &StepResult) {
  let duration = format!("({:.1}s)", result.duration.as_secs_f64());

  match &result.status {
    StepStatus::Ok => {
      println!(
        "    {} {} {}",
        Paint::new(result.name).bold(),
        Paint::new("ok").fg(Color::Green).bold(),
        Paint::new(&duration).dim(),
      );
    },
    StepStatus::Warn(msg) => {
      println!(
        "    {} {} {}",
        Paint::new(result.name).bold(),
        Paint::new("warn").fg(Color::Yellow).bold(),
        Paint::new(&duration).dim(),
      );
      println!("      {}", Paint::new(msg).fg(Color::Yellow));
    },
    StepStatus::Fail(msg) => {
      println!(
        "    {} {} {}",
        Paint::new(result.name).bold(),
        Paint::new("FAIL").fg(Color::Red).bold(),
        Paint::new(&duration).dim(),
      );
      println!("      {}", Paint::new(msg).fg(Color::Red));
    },
    StepStatus::Skipped => {
      println!(
        "    {} {}",
        Paint::new(result.name).bold(),
        Paint::new("skipped").dim(),
      );
    },
  }

  if let Some(ref detail) = result.detail {
    println!("      {}", Paint::new(detail).dim());
  }
}
