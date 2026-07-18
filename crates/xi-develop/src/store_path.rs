use serde::{Deserialize, Serialize};

/// Parsed package info from a Nix store path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInfo {
  pub name: String,
  pub version: Option<String>,
  pub store_path: String,
}

/// Parse a Nix store path into package name and version.
///
/// Store paths follow the format:
/// `/nix/store/{32-char-hash}-{name}-{version}/...`
///
/// The version is the rightmost dash-separated segment that starts with a digit.
pub fn parse(store_path: &str) -> Option<PackageInfo> {
  // Extract the store entry name (after hash, before any subpath)
  let after_store = store_path.strip_prefix("/nix/store/")?;

  // Skip the 32-char hash + dash (33 chars total)
  if after_store.len() < 33 {
    return None;
  }
  let name_ver = &after_store[33..];

  // Remove trailing subpath (/bin, /lib, etc.)
  let name_ver = name_ver.split('/').next().unwrap_or(name_ver);

  if name_ver.is_empty() {
    return None;
  }

  // Split name and version at the rightmost dash followed by a digit
  let (name, version) = split_name_version(name_ver);

  Some(PackageInfo {
    name: name.to_string(),
    version: version.map(ToString::to_string),
    store_path: store_path.to_string(),
  })
}

/// Known nix output suffixes to strip before version parsing.
const OUTPUT_SUFFIXES: &[&str] =
  &["-bin", "-dev", "-lib", "-doc", "-man", "-info", "-out"];

/// Split "cargo-1.95.0" into ("cargo", Some("1.95.0")).
/// Split "bash-interactive-5.2" into ("bash-interactive", Some("5.2")).
/// Split "zstd-1.5.7-bin" into ("zstd", Some("1.5.7")).
/// Split "some-package" into ("some-package", None).
fn split_name_version(s: &str) -> (&str, Option<&str>) {
  // Strip known output suffixes first
  let mut s = s;
  for suffix in OUTPUT_SUFFIXES {
    if let Some(stripped) = s.strip_suffix(suffix) {
      s = stripped;
      break;
    }
  }

  // Scan from right, find rightmost '-' where the next char is a digit
  for (i, _) in s.rmatch_indices('-') {
    let after = &s[i + 1..];
    if after.starts_with(|c: char| c.is_ascii_digit()) {
      return (&s[..i], Some(after));
    }
  }
  (s, None)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_cargo() {
    let p =
      parse("/nix/store/abc123def456ghi789jkl012mno345pq-cargo-1.95.0/bin")
        .expect("should parse");
    assert_eq!(p.name, "cargo");
    assert_eq!(p.version.as_deref(), Some("1.95.0"));
  }

  #[test]
  fn parse_bash_interactive() {
    let p = parse(
      "/nix/store/abc123def456ghi789jkl012mno345pq-bash-interactive-5.2/bin",
    )
    .expect("should parse");
    assert_eq!(p.name, "bash-interactive");
    assert_eq!(p.version.as_deref(), Some("5.2"));
  }

  #[test]
  fn parse_no_version() {
    let p =
      parse("/nix/store/abc123def456ghi789jkl012mno345pq-some-package/bin")
        .expect("should parse");
    assert_eq!(p.name, "some-package");
    assert_eq!(p.version, None);
  }

  #[test]
  fn parse_rustc_wrapper() {
    let p = parse(
      "/nix/store/abc123def456ghi789jkl012mno345pq-rustc-wrapper-1.95.0/bin",
    )
    .expect("should parse");
    assert_eq!(p.name, "rustc-wrapper");
    assert_eq!(p.version.as_deref(), Some("1.95.0"));
  }

  #[test]
  fn parse_not_store_path() {
    assert!(parse("/usr/bin/cargo").is_none());
  }

  #[test]
  fn parse_too_short() {
    assert!(parse("/nix/store/short").is_none());
  }

  #[test]
  fn split_name_version_basic() {
    assert_eq!(
      split_name_version("cargo-1.95.0"),
      ("cargo", Some("1.95.0"))
    );
  }

  #[test]
  fn split_name_version_multi_dash() {
    assert_eq!(
      split_name_version("bash-interactive-5.2"),
      ("bash-interactive", Some("5.2"))
    );
  }

  #[test]
  fn split_name_version_no_version() {
    assert_eq!(split_name_version("some-package"), ("some-package", None));
  }
}
