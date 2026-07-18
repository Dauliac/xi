//! BDD: tests/features/18_file_watcher.feature
//!
//! Tests for file watcher pattern matching.
//! Pure unit tests — no inotify, no git, no daemon.

/// Test the pattern matching logic used by the watcher.
/// We re-implement the matching here since it's a private function.
/// These tests verify the behavior described in the BDD feature.
fn matches_patterns(path: &str, extra_patterns: &[String]) -> bool {
  // Default: only flake.nix and flake.lock (exact filename match)
  const DEFAULT_PATTERNS: &[&str] = &["flake.nix", "flake.lock"];

  let filename = path.rsplit('/').next().unwrap_or(path);

  for pat in DEFAULT_PATTERNS {
    if filename == *pat {
      return true;
    }
  }

  for pat in extra_patterns {
    if pat.starts_with("*.") {
      let ext = &pat[1..];
      if path.ends_with(ext) {
        return true;
      }
    } else if filename == pat.as_str() || path.ends_with(pat.as_str()) {
      return true;
    }
  }

  false
}

/// BDD: 18_file_watcher.feature#Default watch patterns
#[test]
fn default_patterns_match_flake_files() {
  assert!(matches_patterns("flake.nix", &[]));
  assert!(matches_patterns("flake.lock", &[]));
  assert!(matches_patterns("subdir/flake.nix", &[]));
}

/// BDD: 18_file_watcher.feature#Default watch patterns (negative)
#[test]
fn default_patterns_ignore_non_flake_files() {
  // Other .nix files do NOT match by default
  assert!(!matches_patterns("shell.nix", &[]));
  assert!(!matches_patterns("modules/foo.nix", &[]));
  assert!(!matches_patterns("src/main.rs", &[]));
  assert!(!matches_patterns("Cargo.toml", &[]));
  assert!(!matches_patterns("README.md", &[]));
  assert!(!matches_patterns("package.json", &[]));
}

/// BDD: 18_file_watcher.feature#Extra watch patterns from config
#[test]
fn extra_extension_patterns() {
  let extras = vec!["*.yaml".to_string()];
  assert!(matches_patterns("config.yaml", &extras));
  assert!(matches_patterns("deep/dir/foo.yaml", &extras));
  assert!(!matches_patterns("config.json", &extras));
}

/// BDD: 18_file_watcher.feature#Extra watch patterns from config (exact match)
#[test]
fn extra_exact_filename_patterns() {
  let extras = vec!["version.txt".to_string(), "Cargo.lock".to_string()];
  assert!(matches_patterns("version.txt", &extras));
  assert!(matches_patterns("some/dir/version.txt", &extras));
  assert!(matches_patterns("Cargo.lock", &extras));
  assert!(!matches_patterns("README.md", &extras));
}

/// BDD: 18_file_watcher.feature#Default patterns still work with extras
#[test]
fn flake_files_always_match_with_extras() {
  let extras = vec!["*.yaml".to_string()];
  assert!(matches_patterns("flake.nix", &extras));
  assert!(matches_patterns("flake.lock", &extras));
}

/// Users can opt-in to watching all .nix files via extras
#[test]
fn extra_nix_pattern_watches_all_nix_files() {
  let extras = vec!["*.nix".to_string()];
  assert!(matches_patterns("shell.nix", &extras));
  assert!(matches_patterns("modules/foo.nix", &extras));
  assert!(matches_patterns("flake.nix", &extras)); // still matches via default
}

/// BDD: 05_live_update_noop.feature#README edit does not trigger re-evaluation
#[test]
fn readme_does_not_match_any_pattern() {
  let extras = vec!["*.yaml".to_string(), "version.txt".to_string()];
  assert!(!matches_patterns("README.md", &extras));
  assert!(!matches_patterns("src/lib.rs", &extras));
  assert!(!matches_patterns("docs/guide.html", &extras));
}
