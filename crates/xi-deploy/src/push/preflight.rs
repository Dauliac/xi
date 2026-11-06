//! Per-target preflight, run before anything mutates a target.
//!
//! All four checks always run, even after one fails: the operator wants
//! "this host has no sudo and only 300 MiB free", not "this host has no
//! sudo" followed by a second push to learn about the disk.

use color_eyre::Result;

use super::{
  discover::PushTarget,
  engine::{RemoteCommand, Transport},
};

/// Free space preflight insists on under `/nix`.
pub const MIN_FREE_NIX_KIB: u64 = 2 * 1024 * 1024;

/// Which preflight check an outcome belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
  /// `ssh -o BatchMode=yes -o ConnectTimeout=10 <host> true`.
  Ssh,
  /// At least [`MIN_FREE_NIX_KIB`] free under `/nix`.
  Disk,
  /// `sudo -n true`, skipped under `--interactive-sudo`.
  Sudo,
  /// The target's rebuild tool is on `PATH`.
  RebuildTool,
}

impl Check {
  /// Short name used in the per-target report.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Ssh => "ssh",
      Self::Disk => "disk",
      Self::Sudo => "sudo",
      Self::RebuildTool => "rebuild-tool",
    }
  }
}

/// One check's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
  /// Which check this is.
  pub check:  Check,
  /// `None` when the check passed, otherwise why it failed.
  pub reason: Option<String>,
}

impl CheckResult {
  /// A passing result.
  #[must_use]
  pub const fn pass(check: Check) -> Self {
    Self {
      check,
      reason: None,
    }
  }

  /// A failing result.
  #[must_use]
  pub fn fail(check: Check, reason: impl Into<String>) -> Self {
    Self {
      check,
      reason: Some(reason.into()),
    }
  }

  /// Whether this check passed.
  #[must_use]
  pub const fn passed(&self) -> bool {
    self.reason.is_none()
  }
}

/// Every check's verdict for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
  /// Target name, so a report survives being moved out of its target.
  pub target:  String,
  /// One entry per check attempted.
  pub results: Vec<CheckResult>,
}

impl Report {
  /// Whether every attempted check passed.
  #[must_use]
  pub fn ok(&self) -> bool {
    self.results.iter().all(CheckResult::passed)
  }

  /// The failures, in check order.
  #[must_use]
  pub fn failures(&self) -> Vec<&CheckResult> {
    self
      .results
      .iter()
      .filter(|result| !result.passed())
      .collect()
  }

  /// A one-line summary of the failures, for the end-of-run report.
  #[must_use]
  pub fn summary(&self) -> String {
    self
      .failures()
      .iter()
      .map(|result| {
        format!(
          "{}: {}",
          result.check.as_str(),
          result.reason.as_deref().unwrap_or("failed")
        )
      })
      .collect::<Vec<_>>()
      .join("; ")
  }
}

/// Run every preflight check against one target.
///
/// A broken SSH connection short-circuits the remaining three, because
/// every one of them is itself an SSH call and would report the same
/// connection error three more times.
///
/// # Errors
///
/// Returns an error only when the local `ssh` cannot be spawned at all;
/// a target that refuses the connection is a failed check, not an error.
pub fn run(
  transport: &dyn Transport,
  target: &PushTarget,
  interactive_sudo: bool,
) -> Result<Report> {
  let mut results = Vec::new();

  let ssh = transport.run(&target.addr, &RemoteCommand::probe("true"))?;
  if !ssh.success {
    results.push(CheckResult::fail(
      Check::Ssh,
      format!("cannot connect: {}", ssh.message()),
    ));
    return Ok(Report {
      target: target.addr.name.clone(),
      results,
    });
  }
  results.push(CheckResult::pass(Check::Ssh));

  let disk = transport.run(
    &target.addr,
    // `-k` for KiB and `-P` for the POSIX single-line format, because
    // plain `df` wraps long device names onto a second line and the
    // field offsets then shift.
    &RemoteCommand::probe("df -kP /nix | tail -n 1"),
  )?;
  results.push(if disk.success {
    check_disk(&disk.stdout)
  } else {
    CheckResult::fail(
      Check::Disk,
      format!("could not read free space: {}", disk.message()),
    )
  });

  if interactive_sudo {
    // Skipped rather than failed: `--interactive-sudo` exists precisely
    // because `sudo -n` cannot succeed on this host, so running it would
    // report a failure the operator has already acknowledged.
    results.push(CheckResult::pass(Check::Sudo));
  } else {
    let sudo =
      transport.run(&target.addr, &RemoteCommand::probe("sudo -n true"))?;
    results.push(if sudo.success {
      CheckResult::pass(Check::Sudo)
    } else {
      CheckResult::fail(
        Check::Sudo,
        "passwordless sudo unavailable; pass --interactive-sudo or configure \
         NOPASSWD",
      )
    });
  }

  let tool = transport.run(
    &target.addr,
    &RemoteCommand::probe(format!("command -v {}", target.rebuild_tool)),
  )?;
  results.push(if tool.success {
    CheckResult::pass(Check::RebuildTool)
  } else {
    CheckResult::fail(
      Check::RebuildTool,
      format!("{} is not on PATH", target.rebuild_tool),
    )
  });

  Ok(Report {
    target: target.addr.name.clone(),
    results,
  })
}

/// Read the available-KiB column out of one `df -kP` line.
///
/// Exposed for tests: the parsing, not the SSH call, is where this check
/// can realistically be wrong.
#[must_use]
pub fn check_disk(df_line: &str) -> CheckResult {
  let Some(available) = df_line
    .split_whitespace()
    .nth(3)
    .and_then(|field| field.parse::<u64>().ok())
  else {
    return CheckResult::fail(
      Check::Disk,
      format!("could not parse df output: {:?}", df_line.trim()),
    );
  };

  if available < MIN_FREE_NIX_KIB {
    return CheckResult::fail(
      Check::Disk,
      format!(
        "only {:.1} GiB free on /nix, need {:.1} GiB",
        gib(available),
        gib(MIN_FREE_NIX_KIB)
      ),
    );
  }
  CheckResult::pass(Check::Disk)
}

/// KiB as GiB, for a message a human can act on.
fn gib(kib: u64) -> f64 {
  #[expect(
    clippy::cast_precision_loss,
    reason = "A display rounding of a disk size; precision is irrelevant"
  )]
  let as_f64 = kib as f64;
  as_f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_roomy_filesystem_passes() {
    let line = "/dev/sda1 100000000 10000000 90000000 11% /nix";
    assert!(check_disk(line).passed());
  }

  #[test]
  fn a_full_filesystem_names_both_numbers() {
    let line = "/dev/sda1 100000000 99000000 1000000 99% /nix";
    let result = check_disk(line);
    assert!(!result.passed());
    let reason = result.reason.unwrap_or_default();
    assert!(reason.contains("free on /nix"), "got: {reason}");
    assert!(reason.contains("need"), "got: {reason}");
  }

  #[test]
  fn unparsable_df_output_is_a_failure_not_a_pass() {
    let result = check_disk("df: /nix: No such file or directory");
    assert!(!result.passed());
  }

  #[test]
  fn a_report_summarises_every_failure() {
    let report = Report {
      target:  "a".to_owned(),
      results: vec![
        CheckResult::pass(Check::Ssh),
        CheckResult::fail(Check::Sudo, "no sudo"),
        CheckResult::fail(Check::RebuildTool, "no nixos-rebuild"),
      ],
    };
    assert!(!report.ok());
    assert_eq!(report.failures().len(), 2);
    let summary = report.summary();
    assert!(summary.contains("sudo: no sudo"), "got: {summary}");
    assert!(
      summary.contains("rebuild-tool: no nixos-rebuild"),
      "got: {summary}"
    );
  }
}
