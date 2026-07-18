use std::time::Instant;

use color_eyre::Result;
use color_eyre::eyre::bail;
use nix_command::{CommandKind, NixCommand};
use tracing::{debug, info};
use yansi::{Color, Paint};

use crate::args::LibArgs;
use crate::show::DISCOVER_ATTRS_NIX;
use crate::{ensure_flake_locked, resolve_local_flake_dir};

// ---------------------------------------------------------------------------
// Counting
// ---------------------------------------------------------------------------

/// Count all leaf attrs in a discovered JSON tree recursively.
pub fn count_lib_attrs(value: &serde_json::Value) -> usize {
  if let serde_json::Value::Object(obj) = value {
    obj.values().map(count_lib_attrs).sum()
  } else {
    1
  }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Discover `lib` attrs using `nix eval <flake>#lib --apply <expr> --json`.
///
/// Returns `None` if the flake has no `lib` output.
pub fn discover_lib(flake_ref: &str) -> Option<serde_json::Value> {
  let attr = format!("{flake_ref}#lib");

  debug!(attr, "Discovering lib attributes");

  let cmd = NixCommand::new(CommandKind::Eval)
    .arg(&attr)
    .arg("--apply")
    .arg(DISCOVER_ATTRS_NIX)
    .arg("--json");

  let output = cmd.output().ok()?;
  if !output.status.success() {
    debug!(
      attr,
      stderr = %String::from_utf8_lossy(&output.stderr),
      "nix eval failed for lib, likely no lib output"
    );
    return None;
  }

  let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

  if value.is_null() {
    return None;
  }

  Some(value)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render a discovered lib tree recursively.
fn render_lib_tree(
  obj: &serde_json::Map<String, serde_json::Value>,
  indent: usize,
) {
  for (name, value) in obj {
    let prefix = "  ".repeat(indent);
    if value.is_null() {
      println!("{prefix}{}", Paint::new(name).fg(Color::Blue));
    } else if let Some(child_obj) = value.as_object() {
      println!("{prefix}{}", Paint::new(name).fg(Color::Cyan));
      render_lib_tree(child_obj, indent + 1);
    }
  }
}

// ---------------------------------------------------------------------------
// Eval (deepSeq)
// ---------------------------------------------------------------------------

/// Deeply evaluate `lib` using `builtins.deepSeq`.
///
/// # Errors
///
/// Returns an error if nix evaluation fails (type errors, missing attrs, etc.).
pub fn eval_lib(
  flake_ref: &str,
  show_trace: bool,
) -> Result<std::time::Duration> {
  let attr = format!("{flake_ref}#lib");

  debug!(attr, "Deep-evaluating lib with deepSeq");

  let mut cmd = NixCommand::new(CommandKind::Eval)
    .arg(&attr)
    .arg("--apply")
    .arg("x: builtins.deepSeq x true")
    .arg("--json");

  if show_trace {
    cmd = cmd.arg("--show-trace");
  }

  let start = Instant::now();
  let output = cmd
    .output()
    .map_err(|e| color_eyre::eyre::eyre!("Failed to run nix eval: {e}"))?;

  let duration = start.elapsed();

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let error_msg = stderr
      .lines()
      .rfind(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with("fetching ")
      })
      .unwrap_or("lib evaluation failed")
      .to_string();
    bail!("{error_msg}");
  }

  Ok(duration)
}

/// Check whether the flake has a `lib` output (cheap: uses `nix eval --apply`
/// on the flake outputs).
#[must_use]
pub fn has_lib_output(flake_ref: &str) -> bool {
  let attr = format!("{flake_ref}#lib");
  let cmd = NixCommand::new(CommandKind::Eval)
    .arg(&attr)
    .arg("--apply")
    .arg("_: true")
    .arg("--json");

  cmd.output().ok().is_some_and(|o| o.status.success())
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

impl LibArgs {
  /// Run the lib command.
  ///
  /// # Errors
  ///
  /// Returns an error if nix evaluation fails.
  pub fn run(self) -> Result<()> {
    let flake_ref = self.flake_ref.as_deref().unwrap_or(".");
    ensure_flake_locked(resolve_local_flake_dir(Some(flake_ref)))?;

    if self.eval {
      info!("Evaluating lib outputs");
      print!("  {} ...", Paint::new("Evaluating lib (deepSeq)").bold());

      match eval_lib(flake_ref, self.show_trace) {
        Ok(duration) => {
          println!(
            "\r  {} {} {}",
            Paint::new("Evaluating lib (deepSeq)").bold(),
            Paint::new("ok").fg(Color::Green).bold(),
            Paint::new(format!("({:.1}s)", duration.as_secs_f64())).dim(),
          );
        },
        Err(e) => {
          println!(
            "\r  {} {}",
            Paint::new("Evaluating lib (deepSeq)").bold(),
            Paint::new("FAIL").fg(Color::Red).bold(),
          );
          bail!("lib evaluation failed: {e}");
        },
      }

      return Ok(());
    }

    // Default: list lib attrs
    info!("Listing lib outputs");

    let tree = discover_lib(flake_ref);

    match tree {
      None => {
        println!(
          "{}",
          Paint::new("No lib output found in this flake").fg(Color::Yellow)
        );
      },
      Some(ref value) => {
        let count = count_lib_attrs(value);
        println!(
          "{} {}",
          Paint::new("lib").bold(),
          Paint::new(format!("({count} attrs)")).dim(),
        );
        if let Some(obj) = value.as_object() {
          render_lib_tree(obj, 1);
        }
      },
    }

    Ok(())
  }
}
