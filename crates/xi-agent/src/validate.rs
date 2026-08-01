use std::{
  path::Path,
  process::{Command, Stdio},
  time::Instant,
};

use chrono::Utc;
use color_eyre::eyre::Result;

use crate::{
  args::ValidateArgs,
  schema::{
    Envelope, ValidationEvent, ValidationPlan, ValidationStatus,
    ValidationStep, elapsed_ms, emit,
  },
};

pub const CMD: &str = "validate";

pub fn run(args: ValidateArgs) -> Result<()> {
  let start = Instant::now();
  let plan = derive_plan();
  if !args.run {
    emit(&Envelope::ok(CMD, plan, elapsed_ms(start)))?;
    return Ok(());
  }
  execute(plan, args.fail_fast, start)
}

/// Derive a plan from what the project surface actually supports.
///
/// Detection is intentionally cheap: presence of `flake.nix`, `.xi.toml`
/// sections, and known xi subcommands. No Nix evaluation happens here.
#[must_use]
pub fn derive_plan() -> ValidationPlan {
  let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
  let mut steps: Vec<ValidationStep> = Vec::new();

  if cwd.join("flake.nix").exists() {
    steps.push(ValidationStep {
      id:         "fmt-check".to_owned(),
      command:    vec!["xi".into(), "fmt".into(), "--check".into()],
      purpose:    "verify formatting is stable".to_owned(),
      blocking:   true,
      depends_on: Vec::new(),
    });
    steps.push(ValidationStep {
      id:         "doctor".to_owned(),
      command:    vec!["xi".into(), "doctor".into()],
      purpose:    "run xi's flake sanity checks".to_owned(),
      blocking:   true,
      depends_on: Vec::new(),
    });
    steps.push(ValidationStep {
      id:         "flake-check".to_owned(),
      command:    vec!["xi".into(), "check".into()],
      purpose:    "evaluate `nix flake check` outputs".to_owned(),
      blocking:   true,
      depends_on: vec!["doctor".into()],
    });
  }

  if cwd.join("Cargo.toml").exists() {
    steps.push(ValidationStep {
      id:         "cargo-test".to_owned(),
      command:    vec!["cargo".into(), "test".into(), "--workspace".into()],
      purpose:    "run the Rust test suite".to_owned(),
      blocking:   true,
      depends_on: Vec::new(),
    });
  }

  if xi_toml_has_section(&cwd, "ci") {
    steps.push(ValidationStep {
      id:         "ci".to_owned(),
      command:    vec!["xi".into(), "ci".into()],
      purpose:    "run the same pipeline as CI".to_owned(),
      blocking:   true,
      depends_on: vec!["flake-check".into()],
    });
  }

  ValidationPlan { steps }
}

fn xi_toml_has_section(cwd: &Path, section: &str) -> bool {
  let path = cwd.join(".xi.toml");
  let Ok(contents) = std::fs::read_to_string(&path) else {
    return false;
  };
  contents.contains(&format!("[{section}]"))
}

fn execute(
  plan: ValidationPlan,
  fail_fast: bool,
  start: Instant,
) -> Result<()> {
  use std::collections::{HashMap, HashSet};
  let mut passed: HashSet<String> = HashSet::new();
  let mut failed: HashSet<String> = HashSet::new();
  let by_id: HashMap<String, ValidationStep> =
    plan.steps.iter().cloned().map(|s| (s.id.clone(), s)).collect();
  let mut all_blocking_passed = true;

  for step in &plan.steps {
    // Deps
    let blocked = step
      .depends_on
      .iter()
      .any(|d| failed.contains(d) || !by_id.contains_key(d));
    if blocked {
      let ev = ValidationEvent::Finished {
        id:          step.id.clone(),
        status:      ValidationStatus::Blocked,
        duration_ms: 0,
        error:       Some("dependency failed".to_owned()),
      };
      emit(&Envelope::ok(CMD, ev, 0))?;
      if step.blocking {
        all_blocking_passed = false;
      }
      failed.insert(step.id.clone());
      if fail_fast && step.blocking {
        break;
      }
      continue;
    }

    let started = ValidationEvent::Started {
      id: step.id.clone(),
      at: Utc::now(),
    };
    emit(&Envelope::ok(CMD, started, 0))?;

    let step_start = Instant::now();
    let (status, err) = run_step(&step.command);
    let dur = elapsed_ms(step_start);
    let finished = ValidationEvent::Finished {
      id:          step.id.clone(),
      status,
      duration_ms: dur,
      error:       err,
    };
    emit(&Envelope::ok(CMD, finished, dur))?;

    match status {
      ValidationStatus::Passed => {
        passed.insert(step.id.clone());
      },
      ValidationStatus::Failed | ValidationStatus::Blocked => {
        failed.insert(step.id.clone());
        if step.blocking {
          all_blocking_passed = false;
          if fail_fast {
            break;
          }
        }
      },
      ValidationStatus::Skipped => {},
    }
  }

  let complete = ValidationEvent::Complete {
    total_ms:            elapsed_ms(start),
    all_blocking_passed,
  };
  emit(&Envelope::ok(CMD, complete, elapsed_ms(start)))?;
  if all_blocking_passed {
    Ok(())
  } else {
    Err(color_eyre::eyre::eyre!(
      "one or more blocking validation steps failed"
    ))
  }
}

fn run_step(argv: &[String]) -> (ValidationStatus, Option<String>) {
  let Some((prog, rest)) = argv.split_first() else {
    return (
      ValidationStatus::Failed,
      Some("empty command".to_owned()),
    );
  };
  let output = Command::new(prog)
    .args(rest)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output();
  match output {
    Ok(o) if o.status.success() => (ValidationStatus::Passed, None),
    Ok(o) => (
      ValidationStatus::Failed,
      Some(
        String::from_utf8_lossy(&o.stderr)
          .lines()
          .rev()
          .take(3)
          .collect::<Vec<_>>()
          .join(" | "),
      ),
    ),
    Err(e) => (ValidationStatus::Failed, Some(e.to_string())),
  }
}

/// Helper for the context subcommand: the plan is often needed alongside
/// other data without any diagnostics.
#[must_use]
pub fn plan_only() -> ValidationPlan {
  derive_plan()
}
