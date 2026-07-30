use nix_command::{CommandKind, NixCommand};
use strsim::jaro_winkler;
use tracing::debug;
use yansi::{Color, Paint};

use crate::flake_output::FlakeOutput;

/// Minimum Jaro-Winkler similarity to consider a match "close".
/// 0.0 = completely different, 1.0 = identical.
const SIMILARITY_THRESHOLD: f64 = 0.7;

/// Return the current nix system string (e.g. `x86_64-linux`).
fn current_system() -> String {
  crate::flake_output::current_nix_system()
}

/// Fetch and parse `nix flake show --json` for a flake reference.
fn fetch_flake_show(flake_ref: &str) -> color_eyre::Result<serde_json::Value> {
  let cmd = NixCommand::new(CommandKind::Flake)
    .arg("show")
    .arg("--json")
    .arg(flake_ref);

  let output = cmd.output().map_err(|e| {
    color_eyre::eyre::eyre!("failed to run nix flake show: {e}")
  })?;

  if !output.status.success() {
    color_eyre::eyre::bail!("nix flake show failed");
  }

  serde_json::from_slice(&output.stdout).map_err(|e| {
    color_eyre::eyre::eyre!("failed to parse flake show output: {e}")
  })
}

/// A suggestion entry: the full attribute path and the display-friendly name.
struct Suggestion {
  /// Full attribute path (e.g. `packages.x86_64-linux.hello`)
  full_path: String,
  /// The leaf attribute name used for distance matching
  leaf: String,
}

/// Collect available attributes from parsed flake show JSON.
///
/// If `category` is specified, only look in that category. Otherwise, collect
/// from all categories.
fn collect_available_attrs(
  json: &serde_json::Value,
  category: Option<&str>,
  system: &str,
) -> Vec<Suggestion> {
  let Some(root) = json.as_object() else {
    return Vec::new();
  };

  let mut suggestions = Vec::new();

  let categories: Vec<&str> = category.map_or_else(
    || root.keys().map(String::as_str).collect(),
    |cat| vec![cat],
  );

  for cat_name in categories {
    let Some(cat_value) = root.get(cat_name) else {
      continue;
    };
    let Some(cat_obj) = cat_value.as_object() else {
      continue;
    };

    if FlakeOutput::is_name_per_system(cat_name) {
      // Look inside the current system
      if let Some(system_value) = cat_obj.get(system) {
        if FlakeOutput::from_nix_name(cat_name)
          == Some(FlakeOutput::Formatter)
        {
          suggestions.push(Suggestion {
            full_path: format!("{cat_name}.{system}"),
            leaf: "formatter".to_string(),
          });
        } else if let Some(attrs_obj) = system_value.as_object() {
          for attr_name in attrs_obj.keys() {
            suggestions.push(Suggestion {
              full_path: format!("{cat_name}.{system}.{attr_name}"),
              leaf: attr_name.clone(),
            });
          }
        }
      }
    } else {
      // Flat category (nixosConfigurations, homeConfigurations, etc.)
      for attr_name in cat_obj.keys() {
        suggestions.push(Suggestion {
          full_path: format!("{cat_name}.{attr_name}"),
          leaf: attr_name.clone(),
        });
      }
    }
  }

  suggestions
}

/// Format a suggestion for display, showing the flake ref and attribute path.
fn format_suggestion(flake_ref: &str, suggestion: &Suggestion) -> String {
  format!("{flake_ref}#{}", suggestion.full_path)
}

/// Best-effort: print similar attribute suggestions after a build failure.
///
/// This runs `nix flake show --json` and finds attributes similar to the
/// one that was attempted. Errors are silently ignored since we're already
/// in an error path.
///
/// # Arguments
///
/// * `flake_ref` - The flake reference (e.g. `.`, `github:user/repo`)
/// * `attempted_attr` - The attribute name the user tried
/// * `category` - Optional category filter (e.g. `"nixosConfigurations"`,
///   `"devShells"`, `"checks"`)
pub fn print_suggestions_on_failure(
  flake_ref: &str,
  attempted_attr: &str,
  category: Option<&str>,
) {
  if let Err(e) = try_print_suggestions(flake_ref, attempted_attr, category) {
    debug!("Failed to generate attribute suggestions: {e}");
  }
}

fn try_print_suggestions(
  flake_ref: &str,
  attempted_attr: &str,
  category: Option<&str>,
) -> color_eyre::Result<()> {
  if attempted_attr.is_empty() {
    return Ok(());
  }

  let json = fetch_flake_show(flake_ref)?;
  let system = current_system();

  let available = collect_available_attrs(&json, category, &system);

  if available.is_empty() {
    return Ok(());
  }

  // Score by Jaro-Winkler similarity on the leaf name (higher = more similar)
  let mut scored: Vec<(&Suggestion, f64)> = available
    .iter()
    .map(|s| (s, jaro_winkler(attempted_attr, &s.leaf)))
    .collect();
  scored.sort_by(|(_, a), (_, b)| {
    b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)
  });

  let close_matches: Vec<_> = scored
    .iter()
    .filter(|(_, sim)| *sim >= SIMILARITY_THRESHOLD)
    .take(5)
    .collect();

  eprintln!();

  if close_matches.is_empty() {
    // No close matches, show all available
    let label = category.map_or_else(
      || "Available attributes".to_string(),
      |c| format!("Available {c}"),
    );
    eprintln!("{}", Paint::new(format!("{label}:")).bold());
    for suggestion in available.iter().take(15) {
      eprintln!(
        "  {}",
        Paint::new(format_suggestion(flake_ref, suggestion)).fg(Color::Cyan)
      );
    }
    if available.len() > 15 {
      eprintln!(
        "  {}",
        Paint::new(format!("... and {} more", available.len() - 15)).dim()
      );
    }
  } else {
    eprintln!("{}", Paint::new("Did you mean one of these?").bold());
    for (suggestion, _) in &close_matches {
      eprintln!(
        "  {}",
        Paint::new(format_suggestion(flake_ref, suggestion)).fg(Color::Cyan)
      );
    }

    // Also show remaining available if there are few
    let remaining: Vec<_> = available
      .iter()
      .filter(|s| !close_matches.iter().any(|(m, _)| m.leaf == s.leaf))
      .collect();

    if !remaining.is_empty() && remaining.len() <= 10 {
      eprintln!();
      eprintln!("{}", Paint::new("All available:").dim());
      for suggestion in &remaining {
        eprintln!(
          "  {}",
          Paint::new(format_suggestion(flake_ref, suggestion)).dim()
        );
      }
    }
  }

  Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn jaro_winkler_similar_strings_above_threshold() {
    // "hello" vs "hello" should be very similar
    assert!(jaro_winkler("hello", "hello") >= SIMILARITY_THRESHOLD);
  }

  #[test]
  fn jaro_winkler_dissimilar_strings_below_threshold() {
    // "hello" vs "xyz" should be dissimilar
    assert!(jaro_winkler("hello", "xyz") < SIMILARITY_THRESHOLD);
  }

  #[test]
  fn jaro_winkler_identical_is_one() {
    assert!((jaro_winkler("hello", "hello") - 1.0).abs() < f64::EPSILON);
  }

  #[test]
  fn collect_attrs_per_system() {
    let json = serde_json::json!({
      "packages": {
        "x86_64-linux": {
          "default": {"type": "derivation", "name": "xi-4.4.0"},
          "xi": {"type": "derivation", "name": "xi-4.4.0"},
          "hello": {"type": "derivation", "name": "hello-2.10"}
        }
      }
    });

    let attrs =
      collect_available_attrs(&json, Some("packages"), "x86_64-linux");
    assert_eq!(attrs.len(), 3);

    let leaves: Vec<&str> = attrs.iter().map(|s| s.leaf.as_str()).collect();
    assert!(leaves.contains(&"default"));
    assert!(leaves.contains(&"xi"));
    assert!(leaves.contains(&"hello"));
  }

  #[test]
  fn collect_attrs_flat_category() {
    let json = serde_json::json!({
      "nixosConfigurations": {
        "myhost": {"type": "nixos-configuration"},
        "otherhost": {"type": "nixos-configuration"}
      }
    });

    let attrs = collect_available_attrs(
      &json,
      Some("nixosConfigurations"),
      "x86_64-linux",
    );
    assert_eq!(attrs.len(), 2);

    let leaves: Vec<&str> = attrs.iter().map(|s| s.leaf.as_str()).collect();
    assert!(leaves.contains(&"myhost"));
    assert!(leaves.contains(&"otherhost"));
  }

  #[test]
  fn collect_attrs_no_category_filter() {
    let json = serde_json::json!({
      "packages": {
        "x86_64-linux": {
          "hello": {"type": "derivation"}
        }
      },
      "nixosConfigurations": {
        "myhost": {"type": "nixos-configuration"}
      }
    });

    let attrs = collect_available_attrs(&json, None, "x86_64-linux");
    assert_eq!(attrs.len(), 2);
  }

  #[test]
  fn collect_attrs_missing_system() {
    let json = serde_json::json!({
      "packages": {
        "aarch64-darwin": {
          "hello": {"type": "derivation"}
        }
      }
    });

    let attrs =
      collect_available_attrs(&json, Some("packages"), "x86_64-linux");
    assert!(attrs.is_empty());
  }
}
