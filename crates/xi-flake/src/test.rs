use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use color_eyre::Result;
use color_eyre::eyre::bail;
use nix_command::{CommandKind, NixCommand};
use tracing::{debug, info};
use yansi::{Color, Paint};

use crate::args::{TestArgs, TestBackend, TestFormat};
use crate::project_config::{self, CustomTestBackend, ProjectTestConfig};
use crate::{current_nix_system, ensure_flake_locked, resolve_local_flake_dir};

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

pub struct TestResult {
  pub name: String,
  pub status: TestStatus,
}

pub enum TestStatus {
  Pass,
  Fail {
    expected: Option<String>,
    got: Option<String>,
  },
  Error(String),
}

struct BackendResults {
  backend_name: String,
  results: Vec<TestResult>,
  duration: Duration,
}

// ---------------------------------------------------------------------------
// Nix expression for structured runTests results
// ---------------------------------------------------------------------------

/// Nix expression applied to a runTests attrset to get structured results
/// without throwing on failure.
///
/// For each test case `{ expected, expr }`, produces:
/// - `{ status = "pass"; }` if `expr == expected`
/// - `{ status = "fail"; expected = ...; got = ...; }` on mismatch
/// - `{ status = "error"; }` if evaluation throws
const RUN_TESTS_APPLY_EXPR: &str = r#"tests: builtins.mapAttrs (name: test: let er = builtins.tryEval (builtins.deepSeq test.expr test.expr); xr = builtins.tryEval (builtins.deepSeq test.expected test.expected); in if !er.success then { status = "error"; message = "expr evaluation failed"; } else if !xr.success then { status = "error"; message = "expected evaluation failed"; } else if er.value == xr.value then { status = "pass"; } else { status = "fail"; expected = builtins.toJSON xr.value; got = builtins.toJSON er.value; }) tests"#;

/// Nix expression to detect if an attribute is a runTests-shaped attrset
/// (all values have `expected` and `expr` keys).
const RUN_TESTS_DETECT_EXPR: &str = r"x: builtins.isAttrs x && builtins.all (v: builtins.isAttrs v && v ? expected && v ? expr) (builtins.attrValues x)";

// ---------------------------------------------------------------------------
// Backend detection
// ---------------------------------------------------------------------------

/// Resolved backend ready to execute.
enum ResolvedBackend {
  RunTests { attr: String },
  Checks { filter: String },
  NixUnit { test_dir: String },
  Nixt { test_dir: String },
  Namaka,
  Custom(CustomTestBackend),
}

impl ResolvedBackend {
  fn name(&self) -> &str {
    match self {
      Self::RunTests { .. } => "runTests",
      Self::Checks { .. } => "checks",
      Self::NixUnit { .. } => "nix-unit",
      Self::Nixt { .. } => "nixt",
      Self::Namaka => "namaka",
      Self::Custom(c) => &c.name,
    }
  }
}

/// Detect which test backends are available.
fn detect_backends(
  flake_ref: &str,
  flake_dir: Option<&Path>,
  config: &ProjectTestConfig,
  cli_backends: &[TestBackend],
) -> Vec<ResolvedBackend> {
  let mut backends = Vec::new();

  // If CLI specifies backends, only use those
  let explicit = !cli_backends.is_empty();
  let config_explicit =
    !config.backends.is_empty() && config.backends != vec!["auto"];

  let should_try = |name: &str, variant: Option<&TestBackend>| -> bool {
    if explicit {
      return variant.is_some();
    }
    if config_explicit {
      return config.backends.iter().any(|b| b == name);
    }
    true // auto-detect
  };

  let cli_has = |variant: &TestBackend| -> Option<&TestBackend> {
    cli_backends
      .iter()
      .find(|b| std::mem::discriminant(*b) == std::mem::discriminant(variant))
  };

  // runTests (eval-time)
  if should_try("runTests", cli_has(&TestBackend::RunTests)) {
    let system = current_nix_system();
    let attr = &config.run_tests_attr;
    if detect_run_tests(flake_ref, attr, &system) {
      backends.push(ResolvedBackend::RunTests { attr: attr.clone() });
    }
  }

  // checks (build-time)
  if should_try("checks", cli_has(&TestBackend::Checks)) {
    let system = current_nix_system();
    let checks = detect_checks(flake_ref, &system);
    if !checks.is_empty() {
      backends.push(ResolvedBackend::Checks {
        filter: config.checks_filter.clone(),
      });
    }
  }

  // nix-unit (command)
  if should_try("nix-unit", cli_has(&TestBackend::NixUnit)) {
    let dir = flake_dir.map_or_else(
      || Path::new(&config.nix_unit_test_dir).to_path_buf(),
      |d| d.join(&config.nix_unit_test_dir),
    );
    if which::which("nix-unit").is_ok() && dir.is_dir() {
      backends.push(ResolvedBackend::NixUnit {
        test_dir: config.nix_unit_test_dir.clone(),
      });
    }
  }

  // nixt (command)
  if should_try("nixt", cli_has(&TestBackend::Nixt)) {
    let dir = flake_dir.map_or_else(
      || Path::new(&config.nixt_test_dir).to_path_buf(),
      |d| d.join(&config.nixt_test_dir),
    );
    if which::which("nixt").is_ok() && dir.is_dir() {
      backends.push(ResolvedBackend::Nixt {
        test_dir: config.nixt_test_dir.clone(),
      });
    }
  }

  // namaka (command)
  if should_try("namaka", cli_has(&TestBackend::Namaka)) {
    let has_config = flake_dir.map_or_else(
      || Path::new("namaka.toml").exists(),
      |d| d.join("namaka.toml").exists(),
    );
    if which::which("namaka").is_ok() && has_config {
      backends.push(ResolvedBackend::Namaka);
    }
  }

  // Custom backends from config
  for custom in &config.custom {
    if should_try(&custom.name, None) {
      if which::which(&custom.command).is_ok() {
        backends.push(ResolvedBackend::Custom(custom.clone()));
      } else {
        debug!(
          command = custom.command,
          name = custom.name,
          "Custom test backend not found in PATH, skipping"
        );
      }
    }
  }

  backends
}

/// Check if the flake has a runTests-shaped attribute for the current system.
fn detect_run_tests(flake_ref: &str, attr: &str, system: &str) -> bool {
  // Try per-system first: .#tests.x86_64-linux
  let per_system_attr = format!("{flake_ref}#{attr}.{system}");
  if detect_run_tests_attr(&per_system_attr) {
    return true;
  }
  // Fall back to top-level: .#tests
  let top_level_attr = format!("{flake_ref}#{attr}");
  detect_run_tests_attr(&top_level_attr)
}

fn detect_run_tests_attr(attr: &str) -> bool {
  let output = NixCommand::new(CommandKind::Eval)
    .arg(attr)
    .arg("--apply")
    .arg(RUN_TESTS_DETECT_EXPR)
    .arg("--json")
    .output();

  match output {
    Ok(o) if o.status.success() => {
      let stdout = String::from_utf8_lossy(&o.stdout);
      stdout.trim() == "true"
    },
    _ => false,
  }
}

/// Discover check derivation names for the current system.
fn detect_checks(flake_ref: &str, system: &str) -> Vec<String> {
  let attr = format!("{flake_ref}#checks.{system}");
  let output = NixCommand::new(CommandKind::Eval)
    .arg(&attr)
    .arg("--apply")
    .arg("x: builtins.attrNames x")
    .arg("--json")
    .output();

  match output {
    Ok(o) if o.status.success() => {
      serde_json::from_slice::<Vec<String>>(&o.stdout).unwrap_or_default()
    },
    _ => Vec::new(),
  }
}

// ---------------------------------------------------------------------------
// Backend execution
// ---------------------------------------------------------------------------

/// Run the runTests eval backend.
fn run_eval_tests(
  flake_ref: &str,
  attr: &str,
  filter: Option<&str>,
) -> Result<BackendResults> {
  let start = Instant::now();
  let system = current_nix_system();

  // Try per-system first, fall back to top-level
  let eval_attr = {
    let per_system = format!("{flake_ref}#{attr}.{system}");
    if detect_run_tests_attr(&per_system) {
      per_system
    } else {
      format!("{flake_ref}#{attr}")
    }
  };

  debug!(eval_attr, "Running runTests eval");

  let output = NixCommand::new(CommandKind::Eval)
    .arg(&eval_attr)
    .arg("--apply")
    .arg(RUN_TESTS_APPLY_EXPR)
    .arg("--json")
    .output()
    .map_err(|e| color_eyre::eyre::eyre!("nix eval failed: {e}"))?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Ok(BackendResults {
      backend_name: "runTests".to_string(),
      results: vec![TestResult {
        name: attr.to_string(),
        status: TestStatus::Error(
          stderr.lines().last().unwrap_or("eval failed").to_string(),
        ),
      }],
      duration: start.elapsed(),
    });
  }

  let json: serde_json::Value = serde_json::from_slice(&output.stdout)
    .map_err(|e| {
      color_eyre::eyre::eyre!("Failed to parse runTests output: {e}")
    })?;

  let mut results = Vec::new();

  if let Some(obj) = json.as_object() {
    for (name, value) in obj {
      if let Some(f) = filter
        && !glob_match(f, name)
      {
        continue;
      }

      let status = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("error");

      let test_status = match status {
        "pass" => TestStatus::Pass,
        "fail" => TestStatus::Fail {
          expected: value
            .get("expected")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
          got: value
            .get("got")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
        },
        _ => TestStatus::Error(
          value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error")
            .to_string(),
        ),
      };

      results.push(TestResult {
        name: name.clone(),
        status: test_status,
      });
    }
  }

  Ok(BackendResults {
    backend_name: "runTests".to_string(),
    results,
    duration: start.elapsed(),
  })
}

/// Run the checks build backend.
fn run_check_builds(
  flake_ref: &str,
  filter: Option<&str>,
  checks_filter: &str,
  no_nom: bool,
  passthrough_args: &[String],
) -> BackendResults {
  let start = Instant::now();
  let system = current_nix_system();

  let check_names = detect_checks(flake_ref, &system);

  let mut results = Vec::new();

  for name in &check_names {
    // Apply config filter
    if !checks_filter.is_empty() && !glob_match(checks_filter, name) {
      continue;
    }
    // Apply CLI filter
    if let Some(f) = filter
      && !glob_match(f, name)
    {
      continue;
    }

    let installable = format!("{flake_ref}#checks.{system}.{name}");

    let check_start = Instant::now();

    let cmd = NixCommand::new(CommandKind::Build)
      .print_build_logs(false)
      .arg(&installable)
      .arg("--no-link")
      .args(passthrough_args);

    let build_result = crate::execute_build(&cmd, no_nom, false);
    let check_duration = check_start.elapsed();

    let status = match build_result {
      Ok(()) => TestStatus::Pass,
      Err(e) => TestStatus::Fail {
        expected: None,
        got: Some(format!(
          "build failed: {e} ({:.1}s)",
          check_duration.as_secs_f64()
        )),
      },
    };

    results.push(TestResult {
      name: name.clone(),
      status,
    });
  }

  BackendResults {
    backend_name: "checks".to_string(),
    results,
    duration: start.elapsed(),
  }
}

/// Run a command-based backend (nix-unit, nixt, namaka, custom).
fn run_command_backend(
  backend_name: &str,
  command: &str,
  args: &[&str],
) -> Result<BackendResults> {
  let start = Instant::now();

  debug!(backend_name, command, ?args, "Running command backend");

  let status = std::process::Command::new(command)
    .args(args)
    .stdin(std::process::Stdio::inherit())
    .stdout(std::process::Stdio::inherit())
    .stderr(std::process::Stdio::inherit())
    .status()
    .map_err(|e| color_eyre::eyre::eyre!("Failed to run {command}: {e}"))?;

  let results = vec![TestResult {
    name: format!("{backend_name} suite"),
    status: if status.success() {
      TestStatus::Pass
    } else {
      TestStatus::Fail {
        expected: None,
        got: Some(format!("exited with {status}")),
      }
    },
  }];

  Ok(BackendResults {
    backend_name: backend_name.to_string(),
    results,
    duration: start.elapsed(),
  })
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_backend_results(results: &BackendResults) {
  println!();
  println!(
    "  {} {}",
    Paint::new(&results.backend_name).bold(),
    Paint::new(format!("({:.1}s)", results.duration.as_secs_f64())).dim(),
  );

  for result in &results.results {
    match &result.status {
      TestStatus::Pass => {
        println!("    {} {}", Paint::new("✓").fg(Color::Green), result.name);
      },
      TestStatus::Fail { expected, got } => {
        println!(
          "    {} {}",
          Paint::new("✗").fg(Color::Red),
          Paint::new(&result.name).fg(Color::Red),
        );
        if let Some(exp) = expected {
          println!(
            "        {}: {}",
            Paint::new("expected").dim(),
            Paint::new(truncate_value(exp, 80)).fg(Color::Green),
          );
        }
        if let Some(g) = got {
          println!(
            "        {}:      {}",
            Paint::new("got").dim(),
            Paint::new(truncate_value(g, 80)).fg(Color::Red),
          );
        }
      },
      TestStatus::Error(msg) => {
        println!(
          "    {} {} — {}",
          Paint::new("!").fg(Color::Yellow),
          Paint::new(&result.name).fg(Color::Yellow),
          Paint::new(msg).dim(),
        );
      },
    }
  }
}

fn render_summary(all_results: &[BackendResults]) {
  let mut passed = 0usize;
  let mut failed = 0usize;
  let mut errors = 0usize;
  let mut total_duration = Duration::ZERO;

  for br in all_results {
    total_duration += br.duration;
    for r in &br.results {
      match &r.status {
        TestStatus::Pass => passed += 1,
        TestStatus::Fail { .. } => failed += 1,
        TestStatus::Error(_) => errors += 1,
      }
    }
  }

  println!();

  let total = passed + failed + errors;

  let summary = if failed > 0 || errors > 0 {
    format!(
      "{passed} passed, {failed} failed, {errors} errors ({total} total, {:.1}s)",
      total_duration.as_secs_f64()
    )
  } else {
    format!(
      "{passed} passed ({total} total, {:.1}s)",
      total_duration.as_secs_f64()
    )
  };

  if failed > 0 || errors > 0 {
    println!("  {}", Paint::new(summary).fg(Color::Red).bold());
  } else {
    println!("  {}", Paint::new(summary).fg(Color::Green).bold());
  }
}

fn render_json(all_results: &[BackendResults]) {
  let mut passed = 0usize;
  let mut failed = 0usize;
  let mut errors = 0usize;
  let mut total_secs = 0.0f64;

  let backends: Vec<serde_json::Value> = all_results
    .iter()
    .map(|br| {
      total_secs += br.duration.as_secs_f64();
      let tests: Vec<serde_json::Value> = br
        .results
        .iter()
        .map(|r| match &r.status {
          TestStatus::Pass => {
            passed += 1;
            serde_json::json!({
              "name": r.name,
              "status": "pass"
            })
          },
          TestStatus::Fail { expected, got } => {
            failed += 1;
            serde_json::json!({
              "name": r.name,
              "status": "fail",
              "expected": expected,
              "got": got
            })
          },
          TestStatus::Error(msg) => {
            errors += 1;
            serde_json::json!({
              "name": r.name,
              "status": "error",
              "message": msg
            })
          },
        })
        .collect();

      serde_json::json!({
        "backend": br.backend_name,
        "duration_secs": br.duration.as_secs_f64(),
        "tests": tests
      })
    })
    .collect();

  let output = serde_json::json!({
    "passed": passed,
    "failed": failed,
    "errors": errors,
    "total": passed + failed + errors,
    "duration_secs": total_secs,
    "backends": backends
  });

  println!(
    "{}",
    serde_json::to_string_pretty(&output).unwrap_or_default()
  );
}

fn truncate_value(s: &str, max_len: usize) -> &str {
  if s.len() <= max_len { s } else { &s[..max_len] }
}

/// Simple glob matching: supports `*` as wildcard.
fn glob_match(pattern: &str, value: &str) -> bool {
  if pattern.is_empty() || pattern == "*" {
    return true;
  }

  let parts: Vec<&str> = pattern.split('*').collect();

  if parts.len() == 1 {
    // No wildcard — exact match
    return value == pattern;
  }

  let mut pos = 0;
  for (i, part) in parts.iter().enumerate() {
    if part.is_empty() {
      continue;
    }
    if let Some(found) = value[pos..].find(part) {
      if i == 0 && found != 0 {
        // First part must match at start
        return false;
      }
      pos += found + part.len();
    } else {
      return false;
    }
  }

  // If pattern doesn't end with *, value must end exactly
  if !pattern.ends_with('*') && pos != value.len() {
    return false;
  }

  true
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

impl TestArgs {
  /// Run the test command.
  ///
  /// # Errors
  ///
  /// Returns an error if any backend fails critically or if tests fail.
  pub fn run(self) -> Result<()> {
    let flake_ref_owned =
      self.flake_ref.clone().unwrap_or_else(|| ".".to_string());
    let flake_ref = flake_ref_owned.as_str();
    let local_dir = resolve_local_flake_dir(Some(flake_ref));

    ensure_flake_locked(local_dir.clone())?;

    let config = project_config::load_project_config(local_dir.as_deref());
    let passthrough_args = self.passthrough.to_nix_args();

    info!("Running tests");

    let backends = detect_backends(
      flake_ref,
      local_dir.as_deref(),
      &config.test,
      &self.backend,
    );

    if backends.is_empty() {
      println!(
        "{}",
        Paint::new("No test backends detected. Configure via .xi.toml [test].")
          .fg(Color::Yellow)
      );
      return Ok(());
    }

    debug!(
      backends = ?backends.iter().map(ResolvedBackend::name).collect::<Vec<_>>(),
      "Detected test backends"
    );

    if self.list {
      list_tests(flake_ref, &backends);
      return Ok(());
    }

    let json_mode = matches!(self.format, TestFormat::Json);
    let review_mode = self.review;
    let watch_mode = self.watch;
    let filter = self.filter;
    let no_nom = self.no_nom;

    let run_once = |backends: &[ResolvedBackend]| -> Result<bool> {
      let mut all_results = Vec::new();
      let mut any_failed = false;

      for backend in backends {
        let results = match backend {
          ResolvedBackend::RunTests { attr } => {
            run_eval_tests(flake_ref, attr, filter.as_deref())?
          },
          ResolvedBackend::Checks {
            filter: checks_filter,
          } => run_check_builds(
            flake_ref,
            filter.as_deref(),
            checks_filter,
            no_nom,
            &passthrough_args,
          ),
          ResolvedBackend::NixUnit { test_dir } => {
            run_command_backend("nix-unit", "nix-unit", &[test_dir])?
          },
          ResolvedBackend::Nixt { test_dir } => {
            run_command_backend("nixt", "nixt", &["--path", test_dir])?
          },
          ResolvedBackend::Namaka => {
            let cmd = if review_mode { "review" } else { "check" };
            run_command_backend("namaka", "namaka", &[cmd])?
          },
          ResolvedBackend::Custom(c) => {
            let args: Vec<&str> = c.args.iter().map(String::as_str).collect();
            run_command_backend(&c.name, &c.command, &args)?
          },
        };

        if results
          .results
          .iter()
          .any(|r| matches!(r.status, TestStatus::Fail { .. }))
        {
          any_failed = true;
        }

        if !json_mode {
          render_backend_results(&results);
        }
        all_results.push(results);
      }

      if json_mode {
        render_json(&all_results);
      } else {
        render_summary(&all_results);
      }

      Ok(any_failed)
    };

    if watch_mode {
      run_watch_loop(&backends, &run_once, local_dir.as_deref())?;
    } else {
      let any_failed = run_once(&backends)?;
      if any_failed {
        bail!("Some tests failed");
      }
    }
    Ok(())
  }
}

// ---------------------------------------------------------------------------
// List mode
// ---------------------------------------------------------------------------

/// List detected tests without running them.
fn list_tests(flake_ref: &str, backends: &[ResolvedBackend]) {
  let system = current_nix_system();

  for backend in backends {
    match backend {
      ResolvedBackend::RunTests { attr } => {
        // Discover test names via nix eval
        let eval_attr = {
          let per_system = format!("{flake_ref}#{attr}.{system}");
          if detect_run_tests_attr(&per_system) {
            per_system
          } else {
            format!("{flake_ref}#{attr}")
          }
        };

        let output = NixCommand::new(CommandKind::Eval)
          .arg(&eval_attr)
          .arg("--apply")
          .arg("x: builtins.attrNames x")
          .arg("--json")
          .output();

        if let Ok(o) = output
          && o.status.success()
          && let Ok(names) = serde_json::from_slice::<Vec<String>>(&o.stdout)
        {
          println!(
            "{} {}",
            Paint::new("runTests").bold(),
            Paint::new(format!("({} tests)", names.len())).dim(),
          );
          for name in &names {
            println!("  {} {}", Paint::new("·").fg(Color::Green), name);
          }
        }
      },
      ResolvedBackend::Checks { filter } => {
        let mut check_names = detect_checks(flake_ref, &system);
        if !filter.is_empty() {
          check_names.retain(|n| glob_match(filter, n));
        }
        println!(
          "{} {}",
          Paint::new("checks").bold(),
          Paint::new(format!("({} checks)", check_names.len())).dim(),
        );
        for name in &check_names {
          println!("  {} {}", Paint::new("·").fg(Color::Green), name);
        }
      },
      _ => {
        println!(
          "{} {}",
          Paint::new(backend.name()).bold(),
          Paint::new("(external tool — run to discover tests)").dim(),
        );
      },
    }
  }
}

// ---------------------------------------------------------------------------
// Watch mode
// ---------------------------------------------------------------------------

const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Scan .nix files in the flake directory and return the latest mtime.
fn scan_nix_mtime(dir: &Path) -> Option<SystemTime> {
  let mut latest: Option<SystemTime> = None;

  for entry in walkdir::WalkDir::new(dir)
    .follow_links(false)
    .into_iter()
    .filter_entry(|e| {
      let name = e.file_name().to_string_lossy();
      !matches!(
        name.as_ref(),
        ".git" | ".direnv" | "node_modules" | "result" | ".devenv"
      )
    })
    .flatten()
  {
    if !entry.file_type().is_file() {
      continue;
    }
    let path = entry.path();
    let dominated = path
      .extension()
      .is_some_and(|ext| ext == "nix" || ext == "lock" || ext == "toml");
    if !dominated {
      continue;
    }
    if let Ok(meta) = path.metadata()
      && let Ok(mtime) = meta.modified()
    {
      latest = Some(latest.map_or(mtime, |l| l.max(mtime)));
    }
  }

  latest
}

/// Run tests in a loop, re-running when .nix files change.
fn run_watch_loop(
  backends: &[ResolvedBackend],
  run_once: &dyn Fn(&[ResolvedBackend]) -> Result<bool>,
  flake_dir: Option<&Path>,
) -> Result<()> {
  let dir = flake_dir.unwrap_or_else(|| Path::new("."));

  println!(
    "{}",
    Paint::new("Watching for .nix file changes... (Ctrl+C to stop)")
      .fg(Color::Cyan)
      .bold()
  );

  let mut last_mtime = scan_nix_mtime(dir);

  // Run once immediately
  let _ = run_once(backends);

  loop {
    std::thread::sleep(WATCH_POLL_INTERVAL);

    let current_mtime = scan_nix_mtime(dir);

    if current_mtime != last_mtime {
      last_mtime = current_mtime;

      // Clear screen
      print!("\x1b[2J\x1b[H");

      println!(
        "{}",
        Paint::new("File change detected, re-running tests...")
          .fg(Color::Cyan)
          .bold()
      );
      println!();

      let _ = run_once(backends);
    }
  }
}

// ---------------------------------------------------------------------------
// CI integration
// ---------------------------------------------------------------------------

/// Result of running eval-time tests for CI integration.
pub struct CiTestResult {
  pub passed: usize,
  pub failed: usize,
  pub errors: usize,
}

/// Run eval-time tests (runTests backend) for CI integration.
///
/// Returns `None` if no runTests attribute is detected.
/// Returns `Some(CiTestResult)` with counts otherwise.
pub fn run_ci_eval_tests(
  flake_ref: &str,
  test_attr: &str,
) -> Option<CiTestResult> {
  let system = current_nix_system();

  if !detect_run_tests(flake_ref, test_attr, &system) {
    return None;
  }

  let results = run_eval_tests(flake_ref, test_attr, None).ok()?;

  let mut passed = 0;
  let mut failed = 0;
  let mut errors = 0;

  for r in &results.results {
    match &r.status {
      TestStatus::Pass => passed += 1,
      TestStatus::Fail { .. } => failed += 1,
      TestStatus::Error(_) => errors += 1,
    }
  }

  Some(CiTestResult {
    passed,
    failed,
    errors,
  })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn glob_match_exact() {
    assert!(glob_match("foo", "foo"));
    assert!(!glob_match("foo", "bar"));
  }

  #[test]
  fn glob_match_wildcard() {
    assert!(glob_match("test_*", "test_foo"));
    assert!(glob_match("*_oci", "test_oci"));
    assert!(glob_match("*oci*", "test_oci_foo"));
    assert!(!glob_match("test_*", "other_foo"));
  }

  #[test]
  fn glob_match_star_only() {
    assert!(glob_match("*", "anything"));
    assert!(glob_match("", "anything"));
  }

  #[test]
  fn glob_match_prefix_suffix() {
    assert!(glob_match("test_*_pass", "test_foo_pass"));
    assert!(!glob_match("test_*_pass", "test_foo_fail"));
  }

  #[test]
  fn run_tests_detect_expr_valid() {
    // Just verify the const is a non-empty string
    assert!(!RUN_TESTS_DETECT_EXPR.is_empty());
    assert!(RUN_TESTS_DETECT_EXPR.contains("builtins.isAttrs"));
  }

  #[test]
  fn run_tests_apply_expr_valid() {
    assert!(!RUN_TESTS_APPLY_EXPR.is_empty());
    assert!(RUN_TESTS_APPLY_EXPR.contains("builtins.tryEval"));
    assert!(RUN_TESTS_APPLY_EXPR.contains("builtins.deepSeq"));
  }

  #[test]
  fn truncate_short_value() {
    assert_eq!(truncate_value("hello", 10), "hello");
  }

  #[test]
  fn truncate_long_value() {
    let long = "a".repeat(100);
    assert_eq!(truncate_value(&long, 10).len(), 10);
  }
}
