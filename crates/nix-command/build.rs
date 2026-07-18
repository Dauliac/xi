// Build script: writeln! to String is infallible, and env/fs operations
// should panic with clear messages in build scripts.
#![allow(
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::cast_possible_truncation
)]

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
  println!("cargo::rerun-if-env-changed=NIX_CLI_SCHEMA_PATH");
  println!("cargo::rerun-if-env-changed=XI_NIX_BIN");

  let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
  let output_path = out_dir.join("generated_schema.rs");

  let json = load_schema_json();

  if let Some(json) = json {
    let code = generate_schema_code(&json);
    fs::write(&output_path, code).expect("failed to write generated_schema.rs");
  } else {
    let code = generate_empty_schema();
    fs::write(&output_path, code).expect("failed to write generated_schema.rs");
  }
}

/// Try to load the nix CLI schema JSON:
/// 1. From `$NIX_CLI_SCHEMA_PATH` (file path, set by nix build)
/// 2. From running `nix __dump-cli` (plain cargo build fallback)
fn load_schema_json() -> Option<serde_json::Value> {
  // Try env var first (nix build sets this to a store path)
  if let Ok(path) = env::var("NIX_CLI_SCHEMA_PATH")
    && let Ok(contents) = fs::read_to_string(&path)
    && let Ok(json) = serde_json::from_str(&contents)
  {
    eprintln!("build.rs: loaded schema from NIX_CLI_SCHEMA_PATH={path}");
    return Some(json);
  }

  // Fallback: run nix __dump-cli
  let nix_bin = env::var("XI_NIX_BIN").unwrap_or_else(|_| "nix".to_string());
  if let Ok(output) = Command::new(&nix_bin).arg("__dump-cli").output()
    && output.status.success()
    && let Ok(json) = serde_json::from_slice(&output.stdout)
  {
    eprintln!("build.rs: loaded schema from `{nix_bin} __dump-cli`");
    return Some(json);
  }

  eprintln!("build.rs: nix not available, generating empty schema");
  None
}

/// Represents a flag extracted from the schema.
struct Flag {
  name: String,
  arity: u8,
}

/// Extract flags from a JSON object where keys are flag names and values
/// have an "arity" field.
fn extract_flags(flags_obj: &serde_json::Value) -> Vec<Flag> {
  let Some(map) = flags_obj.as_object() else {
    return Vec::new();
  };

  let mut flags: Vec<Flag> = map
    .iter()
    .map(|(name, info)| Flag {
      name: name.clone(),
      arity: info
        .get("arity")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u8,
    })
    .collect();

  flags.sort_by(|a, b| a.name.cmp(&b.name));
  flags
}

/// Extract command names and their flags from the schema.
fn extract_commands(args: &serde_json::Value) -> BTreeMap<String, Vec<Flag>> {
  let mut commands = BTreeMap::new();

  let Some(cmds) = args.get("commands").and_then(|c| c.as_object()) else {
    return commands;
  };

  for (name, cmd_info) in cmds {
    // Extract direct flags
    if let Some(flags_obj) = cmd_info.get("flags") {
      let flags = extract_flags(flags_obj);
      if !flags.is_empty() {
        commands.insert(name.clone(), flags);
      }
    }

    // Extract subcommand flags (e.g., flake.check, flake.show)
    if let Some(sub_cmds) = cmd_info
      .get("args")
      .and_then(|a| a.get("commands"))
      .and_then(|c| c.as_object())
    {
      for (sub_name, sub_info) in sub_cmds {
        if let Some(flags_obj) = sub_info.get("flags") {
          let flags = extract_flags(flags_obj);
          if !flags.is_empty() {
            commands.insert(format!("{name}_{sub_name}"), flags);
          }
        }
      }
    }
  }

  commands
}

/// Generate Rust source code for the schema module.
fn generate_schema_code(json: &serde_json::Value) -> String {
  let mut code = String::with_capacity(64 * 1024);

  writeln!(
    code,
    "// Auto-generated from `nix __dump-cli` — do not edit manually."
  )
  .unwrap();
  writeln!(
    code,
    "// Re-generate by rebuilding the nix-command crate with nix available."
  )
  .unwrap();
  writeln!(code).unwrap();

  // FlagDef struct
  writeln!(code, "/// A nix CLI flag definition.").unwrap();
  writeln!(code, "#[derive(Debug, Clone, Copy)]").unwrap();
  writeln!(code, "pub struct FlagDef {{").unwrap();
  writeln!(
    code,
    "    /// Flag name without leading dashes (e.g., \"keep-going\")."
  )
  .unwrap();
  writeln!(code, "    pub name: &'static str,").unwrap();
  writeln!(
    code,
    "    /// Number of arguments: 0 = boolean, 1 = single value, 2 = pair."
  )
  .unwrap();
  writeln!(code, "    pub arity: u8,").unwrap();
  writeln!(code, "}}").unwrap();
  writeln!(code).unwrap();

  // Schema available flag
  writeln!(
    code,
    "/// Whether the schema was generated from a real nix binary."
  )
  .unwrap();
  writeln!(code, "pub const SCHEMA_AVAILABLE: bool = true;").unwrap();
  writeln!(code).unwrap();

  let args = json.get("args").unwrap_or(json);

  // Global flags
  let global_flags = args.get("flags").map(extract_flags).unwrap_or_default();

  write_flag_array(&mut code, "GLOBAL_FLAGS", &global_flags, "nix global");

  // Per-command flags
  let commands = extract_commands(args);

  // Command list
  let all_cmd_names: Vec<&str> = {
    let mut names: Vec<&str> = args
      .get("commands")
      .and_then(|c| c.as_object())
      .map(|m| m.keys().map(String::as_str).collect())
      .unwrap_or_default();
    names.sort_unstable();
    names
  };

  writeln!(code, "/// All known nix subcommand names.").unwrap();
  writeln!(
    code,
    "pub const COMMANDS: &[&str] = &[{}];",
    all_cmd_names
      .iter()
      .map(|n| format!("\"{n}\""))
      .collect::<Vec<_>>()
      .join(", ")
  )
  .unwrap();
  writeln!(code).unwrap();

  // Per-command flag arrays
  for (cmd_name, flags) in &commands {
    let const_name = cmd_name.to_uppercase().replace('-', "_");
    write_flag_array(
      &mut code,
      &format!("{const_name}_FLAGS"),
      flags,
      &format!("`nix {}`", cmd_name.replace('_', " ")),
    );
  }

  // Lookup functions
  writeln!(
    code,
    "/// Look up the arity of a global flag by name (without leading dashes)."
  )
  .unwrap();
  writeln!(code, "///").unwrap();
  writeln!(
    code,
    "/// Returns `None` if the flag is not a known global flag."
  )
  .unwrap();
  writeln!(
    code,
    "/// Uses binary search (O(log n)) since the array is sorted."
  )
  .unwrap();
  writeln!(code, "#[must_use]").unwrap();
  writeln!(
    code,
    "pub fn global_flag_arity(name: &str) -> Option<u8> {{"
  )
  .unwrap();
  writeln!(code, "    GLOBAL_FLAGS").unwrap();
  writeln!(code, "        .binary_search_by_key(&name, |f| f.name)").unwrap();
  writeln!(code, "        .ok()").unwrap();
  writeln!(code, "        .map(|i| GLOBAL_FLAGS[i].arity)").unwrap();
  writeln!(code, "}}").unwrap();
  writeln!(code).unwrap();

  // Command flag lookup
  writeln!(
    code,
    "/// Look up the arity of a command-specific flag by command and flag name."
  )
  .unwrap();
  writeln!(code, "#[must_use]").unwrap();
  writeln!(
    code,
    "pub fn command_flag_arity(command: &str, name: &str) -> Option<u8> {{"
  )
  .unwrap();
  writeln!(code, "    let flags: &[FlagDef] = match command {{").unwrap();
  for cmd_name in commands.keys() {
    let const_name = cmd_name.to_uppercase().replace('-', "_");
    let match_str = cmd_name.replace('_', " ");
    writeln!(code, "        \"{match_str}\" => {const_name}_FLAGS,").unwrap();
  }
  writeln!(code, "        _ => return None,").unwrap();
  writeln!(code, "    }};").unwrap();
  writeln!(code, "    flags").unwrap();
  writeln!(code, "        .binary_search_by_key(&name, |f| f.name)").unwrap();
  writeln!(code, "        .ok()").unwrap();
  writeln!(code, "        .map(|i| flags[i].arity)").unwrap();
  writeln!(code, "}}").unwrap();

  code
}

/// Write a sorted `FlagDef` const array.
fn write_flag_array(
  code: &mut String,
  name: &str,
  flags: &[Flag],
  doc_label: &str,
) {
  writeln!(
    code,
    "/// Flags for {doc_label} ({} entries, sorted).",
    flags.len()
  )
  .unwrap();
  writeln!(code, "pub const {name}: &[FlagDef] = &[").unwrap();
  for flag in flags {
    writeln!(
      code,
      "    FlagDef {{ name: \"{}\", arity: {} }},",
      flag.name, flag.arity
    )
    .unwrap();
  }
  writeln!(code, "];").unwrap();
  writeln!(code).unwrap();
}

/// Generate an empty schema when nix is not available.
fn generate_empty_schema() -> String {
  let mut code = String::new();

  writeln!(
    code,
    "// Empty schema — nix was not available at build time."
  )
  .unwrap();
  writeln!(
    code,
    "// The proxy will fall back to a hardcoded flag list."
  )
  .unwrap();
  writeln!(code).unwrap();
  writeln!(code, "#[derive(Debug, Clone, Copy)]").unwrap();
  writeln!(code, "pub struct FlagDef {{").unwrap();
  writeln!(code, "    pub name: &'static str,").unwrap();
  writeln!(code, "    pub arity: u8,").unwrap();
  writeln!(code, "}}").unwrap();
  writeln!(code).unwrap();
  writeln!(code, "pub const SCHEMA_AVAILABLE: bool = false;").unwrap();
  writeln!(code).unwrap();
  writeln!(code, "pub const GLOBAL_FLAGS: &[FlagDef] = &[];").unwrap();
  writeln!(code, "pub const COMMANDS: &[&str] = &[];").unwrap();
  writeln!(code).unwrap();
  writeln!(
    code,
    "#[must_use]\npub fn global_flag_arity(_name: &str) -> Option<u8> {{ None }}"
  )
  .unwrap();
  writeln!(code).unwrap();
  writeln!(
    code,
    "#[must_use]\npub fn command_flag_arity(_command: &str, _name: &str) -> Option<u8> {{ None }}"
  )
  .unwrap();

  code
}
