use std::collections::BTreeMap;

use tracing::debug;
use xi_core::flake_output::{self, FlakeOutput, FlakeOutputKind};
use yansi::{Color, Paint};

/// A single flake output entry parsed from JSON.
struct OutputEntry {
  name: Option<String>,
  description: Option<String>,
  output_type: Option<String>,
}

/// Parsed version info from a nix derivation name.
struct VersionInfo<'a> {
  version: &'a str,
  dirty: bool,
}

impl OutputEntry {
  fn split_name_version(&self) -> (Option<&str>, Option<VersionInfo<'_>>) {
    let Some(ref full_name) = self.name else {
      return (None, None);
    };

    let mut split_pos = None;
    for (i, ch) in full_name.char_indices() {
      if ch == '-'
        && let Some(next) = full_name[i + 1..].chars().next()
        && next.is_ascii_digit()
      {
        split_pos = Some(i);
      }
    }

    let Some(pos) = split_pos else {
      return (Some(full_name.as_str()), None);
    };

    let pkg = &full_name[..pos];
    let raw_version = &full_name[pos + 1..];

    #[allow(clippy::option_if_let_else)]
    let (version, dirty) =
      if let Some(stripped) = raw_version.strip_suffix("-dirty") {
        let clean = stripped
          .rsplit_once('-')
          .filter(|(_, hash)| {
            hash.len() >= 6 && hash.chars().all(|c| c.is_ascii_hexdigit())
          })
          .map_or(stripped, |(before, _)| before);
        (clean, true)
      } else {
        (raw_version, false)
      };

    (Some(pkg), Some(VersionInfo { version, dirty }))
  }
}

// Per-system classification, display order, and hidden categories are
// all derived from the centralised `FlakeOutput` enum and
// `flake_output::HIDDEN_CATEGORIES`.  No local constants needed.

/// Detect the kind of a top-level flake output from its JSON value.
///
/// Returns a human-readable label:
/// - `"derivation"`, `"app"`, `"nixos-configuration"`, `"nixpkgs-overlay"` → known types
/// - `"module"` → detected from naming convention (ends with Module/Modules)
/// - `"function"` → opaque nix function (type=unknown, no name/description)
/// - `None` → per-system category, handled differently
fn detect_output_kind(
  cat_name: &str,
  value: &serde_json::Value,
) -> Option<&'static str> {
  // Per-system categories are handled by render_per_system_category
  if FlakeOutput::is_name_per_system(cat_name) {
    return None;
  }

  let obj = value.as_object();
  let type_field = obj
    .and_then(|o| o.get("type"))
    .and_then(serde_json::Value::as_str);

  match type_field {
    Some("nixpkgs-overlay") => Some("overlay"),
    Some("nixos-configuration") => Some("nixos-configuration"),
    Some("unknown") | None => {
      // Infer from category name via centralized logic
      FlakeOutputKind::infer_from_category(cat_name)
        .map(FlakeOutputKind::as_str)
        .or(Some("function"))
    },
    Some(_) => None,
  }
}

/// Returns true if a JSON object is a leaf nix flake show entry
/// (has only metadata keys like type/name/description, no real children).
fn is_leaf_output(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
  obj
    .keys()
    .all(|k| matches!(k.as_str(), "type" | "name" | "description"))
}

/// Returns true if a category value has been enriched with discovered children
/// (contains null leaf values from `nix eval` discovery).
fn is_discovered_tree(
  obj: &serde_json::Map<String, serde_json::Value>,
) -> bool {
  obj
    .values()
    .any(|v| v.is_null() || (v.is_object() && is_discovered_subtree(v)))
}

/// Check if a value looks like a discovered subtree (object without "type" key).
fn is_discovered_subtree(value: &serde_json::Value) -> bool {
  value
    .as_object()
    .is_some_and(|o| !o.contains_key("type") && !o.is_empty())
}

/// Returns true if this category should be hidden by default.
fn is_hidden_category(
  cat_name: &str,
  value: &serde_json::Value,
  show_all: bool,
) -> bool {
  if show_all {
    return false;
  }

  // Explicit hide list
  if flake_output::is_hidden_by_default(cat_name) {
    return true;
  }

  // Never hide categories in the known display order
  if FlakeOutput::is_in_display_order(cat_name) {
    return false;
  }

  // Never hide categories matching known naming patterns
  if FlakeOutputKind::is_known_pattern(cat_name) {
    return false;
  }

  // Hide categories where the top-level value is a leaf type=unknown
  // (opaque functions, typically flake-parts internal attrs)
  if let Some(obj) = value.as_object()
    && is_leaf_output(obj)
  {
    return true;
  }

  false
}

/// Render the `nix flake show --json` output in a compact, colored format.
pub fn render_flake_outputs(json: &serde_json::Value, show_all: bool) {
  let Some(root) = json.as_object() else {
    println!(
      "{}",
      Paint::new("Invalid flake output format").fg(Color::Red)
    );
    return;
  };

  if root.is_empty() {
    println!("{}", Paint::new("No outputs found").fg(Color::Yellow));
    return;
  }

  let mut printed_any = false;
  let mut hidden_count = 0usize;

  // Print known categories in order
  for &output in FlakeOutput::DISPLAY_ORDER {
    let cat_name = output.as_str();
    let Some(cat_value) = root.get(cat_name) else {
      continue;
    };
    render_category(
      cat_name,
      cat_value,
      show_all,
      &mut printed_any,
      &mut hidden_count,
    );
  }

  // Print remaining categories not in the known order
  for (cat_name, cat_value) in root {
    if FlakeOutput::is_in_display_order(cat_name.as_str()) {
      continue;
    }

    render_category(
      cat_name,
      cat_value,
      show_all,
      &mut printed_any,
      &mut hidden_count,
    );
  }

  if hidden_count > 0 {
    println!();
    println!(
      "{}",
      Paint::new(format!(
        "{hidden_count} internal output(s) hidden (use --all to show)"
      ))
      .dim()
    );
  }

  if !printed_any && hidden_count == 0 {
    println!("{}", Paint::new("No outputs found").fg(Color::Yellow));
  }
}

/// Render a single category.
fn render_category(
  cat_name: &str,
  cat_value: &serde_json::Value,
  show_all: bool,
  printed_any: &mut bool,
  hidden_count: &mut usize,
) {
  if is_hidden_category(cat_name, cat_value, show_all) {
    *hidden_count += 1;
    return;
  }

  let Some(cat_obj) = cat_value.as_object() else {
    if *printed_any {
      println!();
    }
    *printed_any = true;
    print_opaque_category(cat_name, cat_value);
    return;
  };

  if cat_obj.is_empty() {
    return;
  }

  // Leaf output (e.g. {"type": "unknown"}) — show as opaque label
  if is_leaf_output(cat_obj) {
    if *printed_any {
      println!();
    }
    *printed_any = true;
    print_opaque_category(cat_name, cat_value);
    return;
  }

  if *printed_any {
    println!();
  }
  *printed_any = true;

  if FlakeOutput::from_nix_name(cat_name) == Some(FlakeOutput::Formatter) {
    render_formatter_inline(cat_obj);
    return;
  }

  // lib-like outputs: show type + count, point to `xi lib` for details
  if FlakeOutputKind::infer_from_category(cat_name)
    == Some(FlakeOutputKind::Lib)
  {
    let count = crate::flake_lib::count_lib_attrs(&serde_json::Value::Object(
      cat_obj.clone(),
    ));
    println!(
      "{} {} {}",
      Paint::new(cat_name).bold(),
      Paint::new(":: lib").fg(Color::Green).dim(),
      Paint::new(format!("({count} attrs)")).dim(),
    );
    return;
  }

  if FlakeOutput::is_name_per_system(cat_name) {
    // Skip per-system categories where no system has any attributes
    let has_any_attr = cat_obj.values().any(|system_value| {
      system_value
        .as_object()
        .is_some_and(|attrs| !attrs.is_empty())
    });
    if !has_any_attr {
      return;
    }
    println!("{}", Paint::new(cat_name).bold());
    render_per_system_category(cat_obj, cat_name);
  } else if is_discovered_tree(cat_obj) {
    // If the entire tree is test results, show a compact summary
    if let Some(count) = count_tests_only(cat_obj) {
      println!(
        "{} {}",
        Paint::new(cat_name).bold(),
        Paint::new(format!("({count} tests)")).dim(),
      );
    } else {
      println!("{}", Paint::new(cat_name).bold());
      render_discovered_tree(cat_obj, cat_name, 1);
    }
  } else {
    println!("{}", Paint::new(cat_name).bold());
    render_flat_category(cat_obj, cat_name);
  }
}

/// Returns true if a discovered tree node looks like a test result
/// (e.g. from `lib.runTests` or `nix-unit`: `{ expected = ...; expr = ...; }`).
fn is_test_result_node(
  obj: &serde_json::Map<String, serde_json::Value>,
) -> bool {
  obj.contains_key("expected") && obj.contains_key("expr")
}

/// Count the total number of test leaves in a discovered tree.
/// Returns `Some(count)` if ALL leaves are test results, `None` otherwise.
fn count_tests_only(
  obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<usize> {
  let mut count = 0;
  for value in obj.values() {
    if value.is_null() {
      // Non-test leaf found → not a pure test tree
      return None;
    } else if let Some(child_obj) = value.as_object() {
      if is_test_result_node(child_obj) {
        count += 1;
      } else {
        // Nested group — recurse
        count += count_tests_only(child_obj)?;
      }
    }
  }
  Some(count)
}

/// Render a discovered attribute tree recursively.
///
/// Discovered trees come from `nix eval` enrichment where leaf values
/// are `null` and nested containers are objects without `type` keys.
/// Test result nodes (`{expected, expr}`) are collapsed to a single line.
fn render_discovered_tree(
  obj: &serde_json::Map<String, serde_json::Value>,
  cat_name: &str,
  indent: usize,
) {
  for (name, value) in obj {
    let prefix = "  ".repeat(indent);
    if value.is_null() {
      // Leaf node — show with kind label inferred from category
      let kind = FlakeOutputKind::infer_from_category(cat_name);
      print!("{prefix}{}", Paint::new(name).fg(Color::Blue));
      if let Some(k) = kind {
        print!(" :: {}", Paint::new(k.as_str()).fg(Color::Green).dim());
      }
      if name == "default" {
        print!(" {}", Paint::new("[default]").fg(Color::Yellow));
      }
      println!();
    } else if let Some(child_obj) = value.as_object() {
      if is_test_result_node(child_obj) {
        // Test case — show name only, don't recurse into expected/expr
        print!("{prefix}{}", Paint::new(name).fg(Color::Blue));
        print!(" :: {}", Paint::new("test").fg(Color::Green).dim());
        if name == "default" {
          print!(" {}", Paint::new("[default]").fg(Color::Yellow));
        }
        println!();
      } else {
        // Nested group — show as sub-header and recurse
        println!("{prefix}{}", Paint::new(name).fg(Color::Cyan));
        render_discovered_tree(child_obj, cat_name, indent + 1);
      }
    }
  }
}

/// Print a top-level output that is an opaque value (type=unknown).
fn print_opaque_category(cat_name: &str, value: &serde_json::Value) {
  let kind = value
    .as_object()
    .and_then(|o| o.get("type"))
    .and_then(serde_json::Value::as_str)
    .unwrap_or("unknown");

  let label = match kind {
    "unknown" => FlakeOutputKind::infer_from_category(cat_name)
      .map_or("opaque", |k| k.as_str()),
    other => other,
  };

  println!(
    "{} {}",
    Paint::new(cat_name).bold(),
    Paint::new(format!(":: {label}")).fg(Color::Green).dim(),
  );
}

/// Render the formatter category inline as `formatter :: <name>`.
///
/// The formatter has no child attrs — each system maps directly to a derivation.
fn render_formatter_inline(
  cat_obj: &serde_json::Map<String, serde_json::Value>,
) {
  let formatter_name = cat_obj.values().find_map(|system_value| {
    let entry = parse_entry(system_value);
    let (pkg_name, _version_info) = entry.split_name_version();
    pkg_name.map(std::string::ToString::to_string)
  });

  if let Some(name) = formatter_name {
    println!(
      "{} {} {}",
      Paint::new("formatter").bold(),
      Paint::new("::").dim(),
      Paint::new(name).fg(Color::Green),
    );
  } else {
    println!("{}", Paint::new("formatter").bold());
  }
}

/// Render a per-system category, deduplicating `default` aliases.
fn render_per_system_category(
  cat_obj: &serde_json::Map<String, serde_json::Value>,
  cat_name: &str,
) {
  let mut attrs: BTreeMap<String, OutputEntry> = BTreeMap::new();

  for (_system, system_value) in cat_obj {
    if FlakeOutput::from_nix_name(cat_name) == Some(FlakeOutput::Formatter) {
      let entry = parse_entry(system_value);
      if entry.name.is_some()
        && attrs.get("default").is_none_or(|e| e.name.is_none())
      {
        attrs.insert("default".to_string(), entry);
      } else {
        attrs
          .entry("default".to_string())
          .or_insert_with(empty_entry);
      }
      continue;
    }

    let Some(attrs_obj) = system_value.as_object() else {
      continue;
    };

    for (attr_name, attr_value) in attrs_obj {
      let entry = parse_entry(attr_value);
      if entry.name.is_some()
        && attrs.get(attr_name).is_none_or(|e| e.name.is_none())
      {
        attrs.insert(attr_name.clone(), entry);
      } else {
        attrs.entry(attr_name.clone()).or_insert_with(empty_entry);
      }
    }
  }

  let default_alias_target = attrs.get("default").and_then(|default_entry| {
    let default_name = default_entry.name.as_deref()?;
    attrs
      .iter()
      .find(|(k, e)| {
        k.as_str() != "default" && e.name.as_deref() == Some(default_name)
      })
      .map(|(k, _)| k.clone())
  });

  for (attr_name, entry) in &attrs {
    if attr_name == "default" && default_alias_target.is_some() {
      continue;
    }

    let is_default = attr_name == "default"
      || default_alias_target
        .as_deref()
        .is_some_and(|target| target == attr_name);

    print_entry(attr_name, entry, is_default);
  }
}

/// Render a flat category, detecting entry types and deduplicating `default`.
fn render_flat_category(
  cat_obj: &serde_json::Map<String, serde_json::Value>,
  cat_name: &str,
) {
  // Find if "default" is an alias for another entry (by structural equality).
  let default_alias_target = cat_obj.get("default").and_then(|default_value| {
    cat_obj
      .iter()
      .find(|(k, v)| k.as_str() != "default" && *v == default_value)
      .map(|(k, _)| k.clone())
  });

  for (name, value) in cat_obj {
    // Skip "default" if it aliases another entry.
    if name == "default" && default_alias_target.is_some() {
      continue;
    }

    let is_default = name == "default"
      || default_alias_target
        .as_deref()
        .is_some_and(|target| target == name);

    let entry = parse_entry(value);
    let kind = detect_output_kind(cat_name, value);

    print!("  {}", Paint::new(name).fg(Color::Blue));

    // Show type for known structured entries
    if let Some(ref t) = entry.output_type {
      if t != "unknown" {
        print!(" :: {}", Paint::new(t).fg(Color::Green));
      } else if let Some(k) = kind {
        print!(" :: {}", Paint::new(k).fg(Color::Green).dim());
      }
    } else if let Some(k) = kind {
      print!(" :: {}", Paint::new(k).fg(Color::Green).dim());
    }

    if is_default {
      print!(" {}", Paint::new("[default]").fg(Color::Yellow));
    }

    if let Some(ref d) = entry.description {
      print!(" - {}", Paint::new(d).dim());
    }

    println!();
  }
}

/// Print a single derivation entry on one line.
fn print_entry(attr_name: &str, entry: &OutputEntry, is_default: bool) {
  let (pkg_name, version_info) = entry.split_name_version();

  print!("  {}", Paint::new(attr_name).fg(Color::Blue));

  if let Some(vi) = &version_info {
    print!(" ({})", Paint::new(vi.version).fg(Color::Green));
  } else if let Some(n) = pkg_name
    && n != attr_name
  {
    print!(" :: {}", Paint::new(n).fg(Color::Green));
  }

  if is_default {
    print!(" {}", Paint::new("[default]").fg(Color::Yellow));
  }

  if version_info.as_ref().is_some_and(|vi| vi.dirty) {
    print!(" {}", Paint::new("[dirty]").fg(Color::Red));
  }

  if let Some(ref d) = entry.description {
    print!(" - {}", Paint::new(d).dim());
  }

  println!();
}

fn parse_entry(value: &serde_json::Value) -> OutputEntry {
  let obj = value.as_object();
  OutputEntry {
    name: obj
      .and_then(|o| o.get("name"))
      .and_then(serde_json::Value::as_str)
      .map(String::from),
    description: obj
      .and_then(|o| o.get("description"))
      .and_then(serde_json::Value::as_str)
      .map(String::from),
    output_type: obj
      .and_then(|o| o.get("type"))
      .and_then(serde_json::Value::as_str)
      .map(String::from),
  }
}

const fn empty_entry() -> OutputEntry {
  OutputEntry {
    name: None,
    description: None,
    output_type: None,
  }
}

/// Nix expression to recursively discover attribute names up to depth 4.
///
/// Returns `null` for leaf values (functions, derivations, evaluated module
/// sets, and module definitions with `imports`). Returns nested objects for
/// namespace containers.
pub const DISCOVER_ATTRS_NIX: &str = "x: let go = d: v: if d == 0 then null else if !(builtins.isAttrs v) then null else let names = builtins.attrNames v; in if builtins.elem \"config\" names && builtins.elem \"options\" names then null else if builtins.elem \"imports\" names then null else if builtins.elem \"outPath\" names || builtins.elem \"drvPath\" names then null else if builtins.elem \"expected\" names && builtins.elem \"expr\" names then null else builtins.mapAttrs (_: go (d - 1)) v; in go 4 x";

/// Enrich a flake JSON by discovering children of leaf categories.
///
/// For each top-level category that is a leaf (e.g. `{"type": "unknown"}`),
/// this replaces it with the discovered tree from `nix eval`.
pub fn enrich_flake_json(
  json: &mut serde_json::Value,
  discovered: &[(String, serde_json::Value)],
) {
  let Some(root) = json.as_object_mut() else {
    return;
  };

  for (cat_name, tree) in discovered {
    // Only enrich if current value is a leaf
    if let Some(current) = root.get(cat_name)
      && let Some(obj) = current.as_object()
      && is_leaf_output(obj)
      && !tree.is_null()
    {
      debug!(cat_name, "Enriching leaf category with discovered tree");
      root.insert(cat_name.clone(), tree.clone());
    }
  }
}

/// Return the list of leaf category names that should be discovered.
pub fn leaf_categories_to_discover(json: &serde_json::Value) -> Vec<String> {
  let Some(root) = json.as_object() else {
    return Vec::new();
  };

  root
    .iter()
    .filter_map(|(name, value)| {
      // Skip hidden-by-default categories
      if flake_output::is_hidden_by_default(name.as_str()) {
        return None;
      }

      // Only discover leaf outputs
      let obj = value.as_object()?;
      if is_leaf_output(obj) {
        Some(name.clone())
      } else {
        None
      }
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn entry(name: &str) -> OutputEntry {
    OutputEntry {
      name: Some(name.to_string()),
      description: None,
      output_type: None,
    }
  }

  #[test]
  fn clean_version() {
    let e = entry("xi-4.4.0");
    let (pkg, vi) = e.split_name_version();
    assert_eq!(pkg, Some("xi"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "4.4.0");
    assert!(!vi.dirty);
  }

  #[test]
  fn dirty_version_with_hash() {
    let e = entry("xi-4.4.0-ab81553-dirty");
    let (pkg, vi) = e.split_name_version();
    assert_eq!(pkg, Some("xi"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "4.4.0");
    assert!(vi.dirty);
  }

  #[test]
  fn dirty_version_without_hash() {
    let e = entry("foo-1.2.3-dirty");
    let (pkg, vi) = e.split_name_version();
    assert_eq!(pkg, Some("foo"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "1.2.3");
    assert!(vi.dirty);
  }

  #[test]
  fn no_version() {
    let e = entry("nix-shell");
    let (pkg, vi) = e.split_name_version();
    assert_eq!(pkg, Some("nix-shell"));
    assert!(vi.is_none());
  }

  #[test]
  fn complex_name_with_version() {
    let e = entry("nix3-fmt-wrapper-2.25.0");
    let (pkg, vi) = e.split_name_version();
    assert_eq!(pkg, Some("nix3-fmt-wrapper"));
    let vi = vi.expect("should have version");
    assert_eq!(vi.version, "2.25.0");
    assert!(!vi.dirty);
  }

  #[test]
  fn no_name() {
    let e = empty_entry();
    let (pkg, vi) = e.split_name_version();
    assert!(pkg.is_none());
    assert!(vi.is_none());
  }

  #[test]
  fn hidden_debug_output() {
    let val = serde_json::json!({"type": "unknown"});
    assert!(is_hidden_category("debug", &val, false));
    assert!(!is_hidden_category("debug", &val, true));
  }

  #[test]
  fn hidden_all_systems() {
    let val = serde_json::json!({"type": "unknown"});
    assert!(is_hidden_category("allSystems", &val, false));
  }

  #[test]
  fn visible_known_category_in_order() {
    // Categories in CATEGORY_ORDER should never be hidden
    let val = serde_json::json!({"type": "unknown"});
    assert!(!is_hidden_category("homeConfigurations", &val, false));
    assert!(!is_hidden_category("homeModules", &val, false));
    assert!(!is_hidden_category("darwinConfigurations", &val, false));
    assert!(!is_hidden_category("overlays", &val, false));
  }

  #[test]
  fn visible_known_pattern() {
    // Categories matching naming patterns should not be hidden
    let val = serde_json::json!({"type": "unknown"});
    assert!(!is_hidden_category("systemConfigs", &val, false));
    assert!(!is_hidden_category("customModules", &val, false));
    assert!(!is_hidden_category("myConfigurations", &val, false));
  }

  #[test]
  fn hidden_unknown_opaque() {
    // Unknown categories with type=unknown (no known pattern) should be hidden
    let val = serde_json::json!({"type": "unknown"});
    assert!(is_hidden_category("someInternalThing", &val, false));
  }

  #[test]
  fn visible_known_category() {
    let val = serde_json::json!({"default": {"type": "nixpkgs-overlay"}});
    assert!(!is_hidden_category("overlays", &val, false));
  }

  #[test]
  fn detect_module_from_name() {
    let val = serde_json::json!({"type": "unknown"});
    assert_eq!(detect_output_kind("homeModules", &val), Some("module"));
    assert_eq!(detect_output_kind("nixosModules", &val), Some("module"));
  }

  #[test]
  fn detect_config_from_name() {
    let val = serde_json::json!({"type": "unknown"});
    assert_eq!(
      detect_output_kind("homeConfigurations", &val),
      Some("configuration")
    );
  }

  #[test]
  fn leaf_output_detection() {
    let leaf: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({"type": "unknown"}))
        .expect("valid json");
    assert!(is_leaf_output(&leaf));

    let container: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "gary": {"type": "nixos-configuration"},
        "spongebob": {"type": "nixos-configuration"}
      }))
      .expect("valid json");
    assert!(!is_leaf_output(&container));
  }

  #[test]
  fn discovered_tree_detection() {
    let discovered: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "dauliac": null,
        "juliendauliac": null
      }))
      .expect("valid json");
    assert!(is_discovered_tree(&discovered));

    let nested: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "generic": {"mod1": null, "mod2": null},
        "nixos": {"mod3": null}
      }))
      .expect("valid json");
    assert!(is_discovered_tree(&nested));

    let regular: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "gary": {"type": "nixos-configuration"}
      }))
      .expect("valid json");
    assert!(!is_discovered_tree(&regular));
  }

  #[test]
  fn enrich_replaces_leaf() {
    let mut json = serde_json::json!({
      "homeConfigurations": {"type": "unknown"},
      "nixosConfigurations": {
        "gary": {"type": "nixos-configuration"}
      }
    });

    let discovered = vec![(
      "homeConfigurations".to_string(),
      serde_json::json!({"dauliac": null, "juliendauliac": null}),
    )];

    enrich_flake_json(&mut json, &discovered);

    assert_eq!(
      json["homeConfigurations"],
      serde_json::json!({"dauliac": null, "juliendauliac": null})
    );
    // nixosConfigurations should be unchanged
    assert_eq!(
      json["nixosConfigurations"]["gary"]["type"],
      "nixos-configuration"
    );
  }

  #[test]
  fn enrich_skips_non_leaf() {
    let mut json = serde_json::json!({
      "nixosConfigurations": {
        "gary": {"type": "nixos-configuration"}
      }
    });

    let discovered = vec![(
      "nixosConfigurations".to_string(),
      serde_json::json!({"gary": null}),
    )];

    enrich_flake_json(&mut json, &discovered);

    // Should NOT be replaced since it's not a leaf
    assert_eq!(
      json["nixosConfigurations"]["gary"]["type"],
      "nixos-configuration"
    );
  }

  #[test]
  fn leaf_categories_found() {
    let json = serde_json::json!({
      "homeConfigurations": {"type": "unknown"},
      "nixosConfigurations": {
        "gary": {"type": "nixos-configuration"}
      },
      "modules": {"type": "unknown"},
      "debug": {"type": "unknown"}
    });

    let leaves = leaf_categories_to_discover(&json);
    assert!(leaves.contains(&"homeConfigurations".to_string()));
    assert!(leaves.contains(&"modules".to_string()));
    assert!(!leaves.contains(&"nixosConfigurations".to_string()));
    // debug is in HIDDEN_BY_DEFAULT, so skip it
    assert!(!leaves.contains(&"debug".to_string()));
  }

  #[test]
  fn flat_category_default_alias_detected() {
    let cat_obj: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "default": {"type": "nixpkgs-overlay"},
        "myOverlay": {"type": "nixpkgs-overlay"}
      }))
      .expect("valid json map");

    let default_alias_target =
      cat_obj.get("default").and_then(|default_value| {
        cat_obj
          .iter()
          .find(|(k, v)| k.as_str() != "default" && *v == default_value)
          .map(|(k, _)| k.clone())
      });

    assert_eq!(default_alias_target.as_deref(), Some("myOverlay"));
  }

  #[test]
  fn flat_category_default_no_alias_when_unique() {
    let cat_obj: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "default": {"type": "nixpkgs-overlay"},
        "other": {"type": "unknown"}
      }))
      .expect("valid json map");

    let default_alias_target =
      cat_obj.get("default").and_then(|default_value| {
        cat_obj
          .iter()
          .find(|(k, v)| k.as_str() != "default" && *v == default_value)
          .map(|(k, _)| k.clone())
      });

    assert!(default_alias_target.is_none());
  }

  #[test]
  fn flat_category_default_no_alias_when_only_default() {
    let cat_obj: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "default": {"type": "unknown"}
      }))
      .expect("valid json map");

    let default_alias_target =
      cat_obj.get("default").and_then(|default_value| {
        cat_obj
          .iter()
          .find(|(k, v)| k.as_str() != "default" && *v == default_value)
          .map(|(k, _)| k.clone())
      });

    assert!(default_alias_target.is_none());
  }

  #[test]
  fn infer_module_kind() {
    assert_eq!(
      FlakeOutputKind::infer_from_category("modules"),
      Some(FlakeOutputKind::Module)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("nixosModules"),
      Some(FlakeOutputKind::Module)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("homeModules"),
      Some(FlakeOutputKind::Module)
    );
  }

  #[test]
  fn infer_config_kind() {
    assert_eq!(
      FlakeOutputKind::infer_from_category("homeConfigurations"),
      Some(FlakeOutputKind::Configuration)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("systemConfigs"),
      Some(FlakeOutputKind::Configuration)
    );
  }

  #[test]
  fn infer_lib_kind() {
    assert_eq!(
      FlakeOutputKind::infer_from_category("lib"),
      Some(FlakeOutputKind::Lib)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("evalLib"),
      Some(FlakeOutputKind::Lib)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("myLibs"),
      Some(FlakeOutputKind::Lib)
    );
  }

  #[test]
  fn infer_no_kind_for_unknown() {
    // "templates" and "packages" are known FlakeOutput variants,
    // so they DO have inferred kinds now. Test truly unknown names instead.
    assert_eq!(FlakeOutputKind::infer_from_category("randomThing"), None);
    assert_eq!(FlakeOutputKind::infer_from_category("myStuff"), None);
  }

  #[test]
  fn test_result_node_detected() {
    let node: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "expected": {"container1": {"cve": null}},
        "expr": {"container1": {"cve": null}}
      }))
      .expect("valid json");
    assert!(is_test_result_node(&node));
  }

  #[test]
  fn test_result_node_scalar() {
    let node: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "expected": null,
        "expr": null
      }))
      .expect("valid json");
    assert!(is_test_result_node(&node));
  }

  #[test]
  fn test_result_node_not_detected_without_both_keys() {
    let node: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "expected": null,
        "something_else": null
      }))
      .expect("valid json");
    assert!(!is_test_result_node(&node));
  }

  #[test]
  fn test_result_node_not_detected_for_regular() {
    let node: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "mod1": null,
        "mod2": null
      }))
      .expect("valid json");
    assert!(!is_test_result_node(&node));
  }

  #[test]
  fn count_tests_all_tests() {
    let tree: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "test1": {"expected": null, "expr": null},
        "test2": {"expected": "foo", "expr": "foo"}
      }))
      .expect("valid json");
    assert_eq!(count_tests_only(&tree), Some(2));
  }

  #[test]
  fn count_tests_mixed_tree() {
    let tree: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "test1": {"expected": null, "expr": null},
        "lib_func": null
      }))
      .expect("valid json");
    assert_eq!(count_tests_only(&tree), None);
  }

  #[test]
  fn count_tests_nested() {
    let tree: serde_json::Map<String, serde_json::Value> =
      serde_json::from_value(serde_json::json!({
        "group1": {
          "test1": {"expected": null, "expr": null},
          "test2": {"expected": null, "expr": null}
        },
        "test3": {"expected": null, "expr": null}
      }))
      .expect("valid json");
    assert_eq!(count_tests_only(&tree), Some(3));
  }

  #[test]
  fn discover_nix_expr_detects_tests() {
    // The DISCOVER_ATTRS_NIX expression should contain the
    // expected+expr leaf detection
    assert!(
      DISCOVER_ATTRS_NIX.contains("\"expected\""),
      "DISCOVER_ATTRS_NIX should detect expected+expr test nodes"
    );
    assert!(
      DISCOVER_ATTRS_NIX.contains("\"expr\""),
      "DISCOVER_ATTRS_NIX should detect expected+expr test nodes"
    );
  }
}
