use std::fmt;

/// Shell types supported by xi develop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellType {
  Bash,
  Zsh,
  Fish,
}

impl ShellType {
  /// Parse shell type from string.
  ///
  /// # Errors
  ///
  /// Returns an error if the shell type is not recognized.
  pub fn parse(s: &str) -> color_eyre::Result<Self> {
    match s.to_lowercase().as_str() {
      "bash" => Ok(Self::Bash),
      "zsh" => Ok(Self::Zsh),
      "fish" => Ok(Self::Fish),
      _ => Err(color_eyre::eyre::eyre!(
        "Unsupported shell: '{s}'. Supported: bash, zsh, fish"
      )),
    }
  }

  /// Env file name for this shell type.
  #[must_use]
  pub fn env_file_name(self, target: &str) -> String {
    match self {
      Self::Fish => format!("env-{target}.fish"),
      Self::Bash | Self::Zsh => format!("env-{target}.sh"),
    }
  }

  /// Generate an export statement.
  #[must_use]
  pub fn export(self, key: &str, value: &str) -> String {
    match self {
      Self::Fish => {
        let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
        format!("set -gx {key} '{escaped}'")
      },
      Self::Bash | Self::Zsh => {
        let escaped = value.replace('\'', "'\\''");
        format!("export {key}='{escaped}'")
      },
    }
  }

  /// Generate an unset statement.
  #[must_use]
  pub fn unset(self, key: &str) -> String {
    match self {
      Self::Fish => format!("set -e {key}"),
      Self::Bash | Self::Zsh => format!("unset {key}"),
    }
  }

  /// Name as used in CLI arguments.
  #[must_use]
  pub const fn name(self) -> &'static str {
    match self {
      Self::Bash => "bash",
      Self::Zsh => "zsh",
      Self::Fish => "fish",
    }
  }
}

impl fmt::Display for ShellType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.name())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_shells() {
    assert_eq!(ShellType::parse("bash").ok(), Some(ShellType::Bash));
    assert_eq!(ShellType::parse("ZSH").ok(), Some(ShellType::Zsh));
    assert_eq!(ShellType::parse("fish").ok(), Some(ShellType::Fish));
    assert!(ShellType::parse("nushell").is_err());
  }

  #[test]
  fn export_bash() {
    assert_eq!(ShellType::Bash.export("FOO", "bar"), "export FOO='bar'");
  }

  #[test]
  fn export_bash_with_quotes() {
    assert_eq!(
      ShellType::Bash.export("FOO", "it's"),
      "export FOO='it'\\''s'"
    );
  }

  #[test]
  fn export_fish() {
    assert_eq!(ShellType::Fish.export("FOO", "bar"), "set -gx FOO 'bar'");
  }

  #[test]
  fn env_file_name_per_target() {
    assert_eq!(ShellType::Bash.env_file_name("default"), "env-default.sh");
    assert_eq!(ShellType::Fish.env_file_name("python"), "env-python.fish");
  }
}
