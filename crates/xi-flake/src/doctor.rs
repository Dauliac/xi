use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::Result;
use color_eyre::eyre::bail;
use tracing::debug;
use yansi::{Color, Paint};

use crate::args::DoctorArgs;
use crate::project_config::ProjectDoctorConfig;
use crate::resolve_local_flake_dir;

// ---------------------------------------------------------------------------
// Supported nixpkgs branches (built-in defaults)
// ---------------------------------------------------------------------------

const DEFAULT_SUPPORTED_BRANCHES: &[&str] = &[
  "nixos-25.05",
  "nixos-25.05-small",
  "nixos-24.11",
  "nixos-24.11-small",
  "nixos-unstable",
  "nixos-unstable-small",
  "nixpkgs-unstable",
  "nixpkgs-25.05-darwin",
  "nixpkgs-24.11-darwin",
];

const DEFAULT_MAX_INPUT_AGE_DAYS: u64 = 30;
const SECONDS_PER_DAY: u64 = 86400;

// ---------------------------------------------------------------------------
// Check result types
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[allow(dead_code)]
pub enum CheckStatus {
  Ok(String),
  Warn(String),
  Fail(String),
}

#[derive(Debug)]
pub struct CheckResult {
  pub name: &'static str,
  pub status: CheckStatus,
}

impl CheckResult {
  fn print(&self) {
    match &self.status {
      CheckStatus::Ok(msg) => {
        println!(
          "    {} {}",
          Paint::new(self.name).bold(),
          Paint::new(format!("ok — {msg}")).fg(Color::Green),
        );
      },
      CheckStatus::Warn(msg) => {
        println!(
          "    {} {}",
          Paint::new(self.name).bold(),
          Paint::new(format!("warn — {msg}")).fg(Color::Yellow),
        );
      },
      CheckStatus::Fail(msg) => {
        println!(
          "    {} {}",
          Paint::new(self.name).bold(),
          Paint::new(format!("FAIL — {msg}")).fg(Color::Red),
        );
      },
    }
  }

  pub const fn is_warn(&self) -> bool {
    matches!(self.status, CheckStatus::Warn(_))
  }

  pub const fn is_fail(&self) -> bool {
    matches!(self.status, CheckStatus::Fail(_))
  }
}

// ---------------------------------------------------------------------------
// Flake lock parsing (minimal, no external dep)
// ---------------------------------------------------------------------------

/// A direct flake input with its locked metadata.
#[derive(Debug)]
struct FlakeInput {
  name: String,
  owner: Option<String>,
  repo: Option<String>,
  git_ref: Option<String>,
  last_modified: Option<u64>,
  #[allow(dead_code)]
  source_type: Option<String>,
}

/// Parse flake.lock and extract direct (root-level) inputs.
fn parse_flake_lock(lock_path: &Path) -> Result<Vec<FlakeInput>> {
  let content = std::fs::read_to_string(lock_path)?;
  let lock: serde_json::Value = serde_json::from_str(&content)?;

  let nodes = lock
    .get("nodes")
    .and_then(serde_json::Value::as_object)
    .ok_or_else(|| {
      color_eyre::eyre::eyre!("Invalid flake.lock: missing nodes")
    })?;

  let root_name = lock
    .get("root")
    .and_then(serde_json::Value::as_str)
    .unwrap_or("root");

  let root_node = nodes
    .get(root_name)
    .and_then(serde_json::Value::as_object)
    .ok_or_else(|| {
      color_eyre::eyre::eyre!("Invalid flake.lock: missing root node")
    })?;

  let root_inputs = root_node
    .get("inputs")
    .and_then(serde_json::Value::as_object);

  let Some(root_inputs) = root_inputs else {
    return Ok(vec![]);
  };

  let mut inputs = Vec::new();

  for (input_name, target) in root_inputs {
    // Target can be a string (node name) or array (follows)
    let node_name = match target {
      serde_json::Value::String(s) => s.as_str(),
      serde_json::Value::Array(arr) => {
        // "follows" reference — resolve the chain
        arr.last().and_then(serde_json::Value::as_str).unwrap_or("")
      },
      _ => continue,
    };

    let Some(node) =
      nodes.get(node_name).and_then(serde_json::Value::as_object)
    else {
      continue;
    };

    let locked = node.get("locked").and_then(serde_json::Value::as_object);
    let original = node.get("original").and_then(serde_json::Value::as_object);

    inputs.push(FlakeInput {
      name: input_name.clone(),
      owner: locked
        .and_then(|l| l.get("owner"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      repo: locked
        .and_then(|l| l.get("repo"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      git_ref: original
        .and_then(|o| o.get("ref"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      last_modified: locked
        .and_then(|l| l.get("lastModified"))
        .and_then(serde_json::Value::as_u64),
      source_type: locked
        .and_then(|l| l.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
    });
  }

  Ok(inputs)
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

/// Check if a nixpkgs input is on a supported branch.
fn check_nixpkgs_branch(
  input: &FlakeInput,
  supported: &[String],
) -> Option<CheckResult> {
  if !is_nixpkgs_input(input) {
    return None;
  }

  let Some(ref git_ref) = input.git_ref else {
    return Some(CheckResult {
      name: "Nixpkgs branch",
      status: CheckStatus::Warn(format!(
        "input '{}' has no branch ref",
        input.name
      )),
    });
  };

  if supported.iter().any(|b| b == git_ref) {
    Some(CheckResult {
      name: "Nixpkgs branch",
      status: CheckStatus::Ok(format!("{} ({})", input.name, git_ref)),
    })
  } else {
    Some(CheckResult {
      name: "Nixpkgs branch",
      status: CheckStatus::Warn(format!(
        "input '{}' uses branch '{}' which is not in the supported list",
        input.name, git_ref
      )),
    })
  }
}

/// Check if an input is stale (older than threshold).
fn check_input_freshness(
  input: &FlakeInput,
  max_age_days: u64,
) -> Option<CheckResult> {
  let last_modified = input.last_modified?;

  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs();

  let age_days = now.saturating_sub(last_modified) / SECONDS_PER_DAY;

  if age_days <= max_age_days {
    Some(CheckResult {
      name: "Input freshness",
      status: CheckStatus::Ok(format!(
        "{} — {} days old (threshold: {})",
        input.name, age_days, max_age_days
      )),
    })
  } else {
    Some(CheckResult {
      name: "Input freshness",
      status: CheckStatus::Warn(format!(
        "input '{}' is {} days old (threshold: {}). Run `xi update` to refresh.",
        input.name, age_days, max_age_days
      )),
    })
  }
}

/// Check if a nixpkgs input comes from the official NixOS org.
fn check_nixpkgs_owner(input: &FlakeInput) -> Option<CheckResult> {
  if !is_nixpkgs_input(input) {
    return None;
  }

  let owner = input.owner.as_ref()?;

  if owner == "NixOS" {
    Some(CheckResult {
      name: "Nixpkgs source",
      status: CheckStatus::Ok(format!(
        "{} (github:{}/{})",
        input.name,
        owner,
        input.repo.as_deref().unwrap_or("nixpkgs")
      )),
    })
  } else {
    Some(CheckResult {
      name: "Nixpkgs source",
      status: CheckStatus::Warn(format!(
        "input '{}' uses fork from '{}' instead of NixOS",
        input.name, owner
      )),
    })
  }
}

/// Check if a flake formatter is declared.
fn check_formatter(flake_ref: &str) -> CheckResult {
  let system = crate::current_nix_system();
  let output = nix_command::NixCommand::new(nix_command::CommandKind::Eval)
    .arg(format!("{flake_ref}#formatter.{system}"))
    .arg("--apply")
    .arg("_: true")
    .arg("--json")
    .output();

  match output {
    Ok(o) if o.status.success() => CheckResult {
      name: "Formatter",
      status: CheckStatus::Ok("declared".to_string()),
    },
    _ => CheckResult {
      name: "Formatter",
      status: CheckStatus::Warn(
        "no formatter declared. Consider adding one or using `xi.formatter.enable` in the xi flake-parts module."
          .to_string(),
      ),
    },
  }
}

/// Heuristic: is this input a nixpkgs input?
fn is_nixpkgs_input(input: &FlakeInput) -> bool {
  let name_match = input.name.contains("nixpkgs");
  let repo_match = input.repo.as_deref().is_some_and(|r| r == "nixpkgs");
  name_match || repo_match
}

// ---------------------------------------------------------------------------
// Run all health checks
// ---------------------------------------------------------------------------

/// Run all flake health checks and return results.
pub fn run_health_checks(
  flake_dir: &Path,
  flake_ref: &str,
  config: &ProjectDoctorConfig,
) -> Result<Vec<CheckResult>> {
  let lock_path = flake_dir.join("flake.lock");
  if !lock_path.exists() {
    bail!(
      "No flake.lock found in {}. Run `nix flake lock` first.",
      flake_dir.display()
    );
  }

  let inputs = parse_flake_lock(&lock_path)?;
  debug!("Parsed {} direct inputs from flake.lock", inputs.len());

  let max_age = config
    .max_input_age_days
    .unwrap_or(DEFAULT_MAX_INPUT_AGE_DAYS);

  let supported_branches: Vec<String> = if config.supported_branches.is_empty()
  {
    DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect()
  } else {
    config.supported_branches.clone()
  };

  let mut results = Vec::new();

  // Per-input checks
  for input in &inputs {
    if let Some(r) = check_nixpkgs_branch(input, &supported_branches) {
      results.push(r);
    }
    if let Some(r) = check_input_freshness(input, max_age) {
      results.push(r);
    }
    if config.require_official_nixpkgs
      && let Some(r) = check_nixpkgs_owner(input)
    {
      results.push(r);
    }
  }

  // Formatter check (requires nix eval, may be slow on first run)
  results.push(check_formatter(flake_ref));

  Ok(results)
}

// ---------------------------------------------------------------------------
// Doctor command
// ---------------------------------------------------------------------------

impl DoctorArgs {
  /// Run the doctor command.
  ///
  /// # Errors
  ///
  /// Returns an error if no flake.lock is found.
  pub fn run(self) -> Result<()> {
    let flake_ref = self.flake_ref.as_deref().unwrap_or(".");
    let local_dir = resolve_local_flake_dir(Some(flake_ref));

    let Some(ref flake_dir) = local_dir else {
      bail!("doctor requires a local flake reference, got: {flake_ref}");
    };

    let config = crate::project_config::load_project_config(Some(flake_dir));

    println!();
    println!(
      "  {}",
      Paint::new(format!("Flake health ({flake_ref})")).bold()
    );
    println!();

    let results = run_health_checks(flake_dir, flake_ref, &config.doctor)?;

    for result in &results {
      result.print();
    }

    let warnings = results.iter().filter(|r| r.is_warn()).count();
    let failures = results.iter().filter(|r| r.is_fail()).count();

    println!();

    if failures > 0 {
      println!(
        "  {}",
        Paint::new(format!("{failures} issue(s), {warnings} warning(s)"))
          .fg(Color::Red)
          .bold()
      );
      bail!("{failures} health check(s) failed");
    } else if warnings > 0 {
      println!(
        "  {}",
        Paint::new(format!("{warnings} warning(s)"))
          .fg(Color::Yellow)
          .bold()
      );
    } else {
      println!(
        "  {}",
        Paint::new("All checks passed").fg(Color::Green).bold()
      );
    }

    println!();
    Ok(())
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  fn make_nixpkgs_input(
    branch: &str,
    owner: &str,
    age_days: u64,
  ) -> FlakeInput {
    let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_secs();

    FlakeInput {
      name: "nixpkgs".to_string(),
      owner: Some(owner.to_string()),
      repo: Some("nixpkgs".to_string()),
      git_ref: Some(branch.to_string()),
      last_modified: Some(now - (age_days * SECONDS_PER_DAY)),
      source_type: Some("github".to_string()),
    }
  }

  #[test]
  fn supported_branch_passes() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 5);
    let supported: Vec<String> = DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect();
    let result = check_nixpkgs_branch(&input, &supported);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Ok(_)));
  }

  #[test]
  fn unsupported_branch_warns() {
    let input = make_nixpkgs_input("nixos-23.05", "NixOS", 5);
    let supported: Vec<String> = DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect();
    let result = check_nixpkgs_branch(&input, &supported);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn fresh_input_passes() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 10);
    let result = check_input_freshness(&input, 30);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Ok(_)));
  }

  #[test]
  fn stale_input_warns() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 60);
    let result = check_input_freshness(&input, 30);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn official_owner_passes() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 5);
    let result = check_nixpkgs_owner(&input);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Ok(_)));
  }

  #[test]
  fn fork_owner_warns() {
    let input = make_nixpkgs_input("nixos-unstable", "someuser", 5);
    let result = check_nixpkgs_owner(&input);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn non_nixpkgs_skips_branch_check() {
    let input = FlakeInput {
      name: "crane".to_string(),
      owner: Some("ipetkov".to_string()),
      repo: Some("crane".to_string()),
      git_ref: Some("master".to_string()),
      last_modified: Some(0),
      source_type: Some("github".to_string()),
    };
    let supported: Vec<String> = DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect();
    assert!(check_nixpkgs_branch(&input, &supported).is_none());
    assert!(check_nixpkgs_owner(&input).is_none());
  }

  #[test]
  fn parse_lock_file_realistic() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let lock_content = r#"{
      "nodes": {
        "nixpkgs": {
          "locked": {
            "lastModified": 1700000000,
            "owner": "NixOS",
            "repo": "nixpkgs",
            "rev": "abc123",
            "type": "github"
          },
          "original": {
            "owner": "NixOS",
            "repo": "nixpkgs",
            "ref": "nixos-unstable",
            "type": "github"
          }
        },
        "crane": {
          "locked": {
            "lastModified": 1700000000,
            "owner": "ipetkov",
            "repo": "crane",
            "rev": "def456",
            "type": "github"
          },
          "original": {
            "owner": "ipetkov",
            "repo": "crane",
            "ref": "master",
            "type": "github"
          }
        },
        "root": {
          "inputs": {
            "nixpkgs": "nixpkgs",
            "crane": "crane"
          }
        }
      },
      "root": "root",
      "version": 7
    }"#;

    std::fs::write(dir.path().join("flake.lock"), lock_content)
      .expect("write lock");

    let inputs =
      parse_flake_lock(&dir.path().join("flake.lock")).expect("parse");
    assert_eq!(inputs.len(), 2);

    let nixpkgs = inputs
      .iter()
      .find(|i| i.name == "nixpkgs")
      .expect("nixpkgs");
    assert_eq!(nixpkgs.owner.as_deref(), Some("NixOS"));
    assert_eq!(nixpkgs.git_ref.as_deref(), Some("nixos-unstable"));

    let crane = inputs.iter().find(|i| i.name == "crane").expect("crane");
    assert_eq!(crane.owner.as_deref(), Some("ipetkov"));
  }
}
