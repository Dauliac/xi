//! Nested-flake devshell policy.
//!
//! When a trusted devshell's `$PWD` walks into a directory owned by a
//! *different* flake, `prompt.rs::handle_subshell`'s "Case 3" decides what
//! happens next. This module controls that decision:
//!
//! - [`NestingMode::Stack`] (default): if the nested flake is also trusted,
//!   spawn a nested subshell for it (today's long-standing behaviour).
//! - [`NestingMode::Exclusive`]: never stack, regardless of the nested flake's
//!   trust state  -  stay in the parent devshell as if the nested flake were
//!   untrusted.
//!
//! Layering, lowest to highest precedence (same rule as [`crate::timeouts`]):
//!
//! 1. **Default**  -  [`NestingMode::default`] (`Stack`).
//! 2. **Project config**  -  `[develop.nesting]` table in `.xi.toml` at the
//!    flake root.
//! 3. **Environment variable**  -  `XI_DEVELOP_NESTING_MODE` overrides the
//!    project config.

use std::path::Path;

use tracing::warn;

/// Policy for a nested flake encountered while already inside a devshell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NestingMode {
  /// Spawn a nested subshell for a trusted nested flake (default).
  #[default]
  Stack,
  /// Never stack; stay in the parent devshell regardless of the nested
  /// flake's trust state.
  Exclusive,
}

impl NestingMode {
  fn parse(raw: &str) -> Option<Self> {
    match raw.trim().to_ascii_lowercase().as_str() {
      "stack" => Some(Self::Stack),
      "exclusive" => Some(Self::Exclusive),
      _ => None,
    }
  }
}

/// Nested-flake devshell configuration.
///
/// See the module docs for the layering rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NestingConfig {
  pub mode: NestingMode,
}

impl NestingConfig {
  /// Load by reading `.xi.toml` (`[develop.nesting]`) at `flake_dir` (or `.`
  /// if `None`), then overlaying the `XI_DEVELOP_NESTING_MODE` env var.
  ///
  /// Never fails: bad values are logged and dropped, leaving the previous
  /// layer intact.
  #[must_use]
  pub fn load(flake_dir: Option<&Path>) -> Self {
    let dir = flake_dir.unwrap_or_else(|| Path::new("."));
    let config_path = dir.join(".xi.toml");
    let project_doc =
      std::fs::read_to_string(&config_path).ok().and_then(|raw| {
        match raw.parse::<toml_edit::DocumentMut>() {
          Ok(d) => Some(d),
          Err(e) => {
            warn!("Failed to parse {}: {e}", config_path.display());
            None
          },
        }
      });

    Self::resolve(project_doc.as_ref(), &|k| std::env::var(k).ok())
  }

  /// Layered loader. `env` is a lookup closure so tests can inject fixtures
  /// without touching the real process env.
  #[must_use]
  pub fn resolve(
    project_doc: Option<&toml_edit::DocumentMut>,
    env: &dyn Fn(&str) -> Option<String>,
  ) -> Self {
    let mut out = Self::default();

    if let Some(doc) = project_doc
      && let Some(table) = doc
        .get("develop")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("nesting"))
        .and_then(|v| v.as_table())
      && let Some(raw) = table.get("mode").and_then(|v| v.as_str())
    {
      match NestingMode::parse(raw) {
        Some(mode) => out.mode = mode,
        None => {
          warn!(
            "Ignoring invalid [develop.nesting].mode = {raw:?}; expected \
             \"stack\" or \"exclusive\""
          );
        },
      }
    }

    if let Some(raw) = env("XI_DEVELOP_NESTING_MODE") {
      match NestingMode::parse(&raw) {
        Some(mode) => out.mode = mode,
        None => {
          warn!(
            "Ignoring invalid XI_DEVELOP_NESTING_MODE={raw:?}; expected \
             \"stack\" or \"exclusive\""
          );
        },
      }
    }

    out
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse_doc(src: &str) -> toml_edit::DocumentMut {
    src.parse().expect("valid TOML")
  }

  fn no_env(_: &str) -> Option<String> {
    None
  }

  #[test]
  fn defaults_to_stack() {
    assert_eq!(NestingConfig::default().mode, NestingMode::Stack);
  }

  #[test]
  fn empty_env_and_no_project_yields_default() {
    let c = NestingConfig::resolve(None, &no_env);
    assert_eq!(c.mode, NestingMode::Stack);
  }

  #[test]
  fn project_config_sets_exclusive() {
    let doc = parse_doc(
      r#"
[develop.nesting]
mode = "exclusive"
"#,
    );
    let c = NestingConfig::resolve(Some(&doc), &no_env);
    assert_eq!(c.mode, NestingMode::Exclusive);
  }

  #[test]
  fn env_overrides_project_config() {
    let doc = parse_doc(
      r#"
[develop.nesting]
mode = "stack"
"#,
    );
    let c = NestingConfig::resolve(Some(&doc), &|k| {
      (k == "XI_DEVELOP_NESTING_MODE").then(|| "exclusive".to_string())
    });
    assert_eq!(c.mode, NestingMode::Exclusive);
  }

  #[test]
  fn invalid_project_value_falls_back_to_default() {
    let doc = parse_doc(
      r#"
[develop.nesting]
mode = "yolo"
"#,
    );
    let c = NestingConfig::resolve(Some(&doc), &no_env);
    assert_eq!(c.mode, NestingMode::Stack);
  }

  #[test]
  fn invalid_env_value_falls_back_to_project_config() {
    let doc = parse_doc(
      r#"
[develop.nesting]
mode = "exclusive"
"#,
    );
    let c = NestingConfig::resolve(Some(&doc), &|k| {
      (k == "XI_DEVELOP_NESTING_MODE").then(|| "not-a-mode".to_string())
    });
    assert_eq!(c.mode, NestingMode::Exclusive);
  }

  #[test]
  fn missing_develop_table_yields_default() {
    let doc = parse_doc(
      r#"
[ci]
backend = "auto"
"#,
    );
    let c = NestingConfig::resolve(Some(&doc), &no_env);
    assert_eq!(c.mode, NestingMode::Stack);
  }
}
