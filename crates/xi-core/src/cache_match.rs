//! The `xi-binary-cache-match` capability: resolves the current git remote
//! / flake-ref to a matchable candidate string and selects which
//! configured `[cache.*]` target(s) apply.
//!
//! Resolution order: an explicit flake-ref argument on the invoking xi
//! subcommand outranks the git remote of the resolved flake root for the
//! current working directory. The most specific matching glob pattern
//! wins; a target with no `match` patterns at all matches unconditionally
//! (the documented backward-compat default for configs written before
//! this capability existed). Absent any match and absent a `default =
//! true` target, nothing is selected and the caller is expected to warn.

use std::{path::Path, process::Command};

use crate::args::CacheTarget;

/// Resolve the candidate string xi matches `[cache.<name>].match` glob
/// patterns against.
///
/// `explicit_flake_ref` is whatever flake-ref the invoking command was
/// given on its command line, if any  -  used verbatim (minus any
/// `#attribute` suffix), since a flake registry shorthand like
/// `github:dauliac/widget` is not a git URL and must not be run through
/// git-URL canonicalisation (`github:` would be mis-parsed as an
/// scp-style host). `git_remote_url` is the `origin` remote of the git
/// checkout at the flake root for the current working directory, if
/// any, and *is* normalised via [`crate::sources::remote_host_path`].
/// Returns `None` when neither resolves to a matchable candidate.
#[must_use]
pub fn resolve_candidate(
  explicit_flake_ref: Option<&str>,
  git_remote_url: Option<&str>,
) -> Option<String> {
  if let Some(flake_ref) = explicit_flake_ref {
    let candidate = strip_flake_attr(flake_ref.trim());
    if !candidate.is_empty() {
      return Some(candidate.to_owned());
    }
  }
  git_remote_url
    .and_then(crate::sources::remote_host_path)
    .map(|(host, path)| {
      if path.is_empty() {
        host
      } else {
        format!("{host}/{path}")
      }
    })
}

/// Strip a trailing `#attribute` from a flake-ref, e.g.
/// `github:dauliac/xi#homeConfigurations.foo` -> `github:dauliac/xi`.
fn strip_flake_attr(flake_ref: &str) -> &str {
  flake_ref.split('#').next().unwrap_or(flake_ref)
}

/// The `origin` remote URL of the git checkout at `dir`, if any.
///
/// Best-effort: a non-repository, a detached checkout, or a missing
/// `git` binary all resolve to `None` rather than an error  -  cache
/// matching degrades to "no remote resolved", not a hard failure.
#[must_use]
pub fn git_remote_origin_url(dir: &Path) -> Option<String> {
  let out = Command::new("git")
    .arg("-C")
    .arg(dir)
    .args(["remote", "get-url", "origin"])
    .output()
    .ok()?;
  if !out.status.success() {
    return None;
  }
  let url = String::from_utf8_lossy(&out.stdout).trim().to_owned();
  if url.is_empty() { None } else { Some(url) }
}

/// Select which configured targets apply for a resolved match candidate.
///
/// - A target with an empty `match` list always matches (back-compat default).
/// - Among targets with a non-empty `match` list, the most specific matching
///   pattern wins; ties all apply.
/// - If none of those match (or no candidate resolved at all), targets with
///   `default = true` apply instead.
#[must_use]
pub fn select_targets<'a>(
  targets: &'a [CacheTarget],
  candidate: Option<&str>,
) -> Vec<&'a CacheTarget> {
  let mut result: Vec<&CacheTarget> = targets
    .iter()
    .filter(|t| t.match_patterns.is_empty())
    .collect();

  let scoped: Vec<&CacheTarget> = targets
    .iter()
    .filter(|t| !t.match_patterns.is_empty())
    .collect();
  if scoped.is_empty() {
    return result;
  }

  let mut best_specificity: Option<usize> = None;
  let mut best: Vec<&CacheTarget> = Vec::new();
  if let Some(candidate) = candidate {
    for target in &scoped {
      for pattern in &target.match_patterns {
        let Some(specificity) = match_specificity(pattern, candidate) else {
          continue;
        };
        match best_specificity {
          Some(current) if specificity < current => {},
          Some(current) if specificity == current => {
            if !best.iter().any(|t| std::ptr::eq(*t, *target)) {
              best.push(target);
            }
          },
          _ => {
            best_specificity = Some(specificity);
            best = vec![*target];
          },
        }
      }
    }
  }

  if best.is_empty() {
    result.extend(scoped.iter().filter(|t| t.default));
  } else {
    result.extend(best);
  }

  result
}

/// Specificity of `pattern` against `candidate`, or `None` if it does not
/// match. Specificity is the length of the literal prefix before the
/// first `*`  -  the longer that prefix, the more specific the pattern.
fn match_specificity(pattern: &str, candidate: &str) -> Option<usize> {
  if !glob_match(pattern, candidate) {
    return None;
  }
  Some(pattern.find('*').unwrap_or(pattern.len()))
}

/// Minimal glob matcher over `*` (matches any run of characters,
/// including none); everything else must match literally. Sufficient
/// for `host/path`-shaped match patterns  -  no need for a full glob
/// crate dependency.
fn glob_match(pattern: &str, text: &str) -> bool {
  let parts: Vec<&str> = pattern.split('*').collect();
  if parts.len() == 1 {
    return pattern == text;
  }

  let mut rest = text;
  let last = parts.len() - 1;
  for (i, part) in parts.iter().enumerate() {
    if i == 0 {
      if !rest.starts_with(part) {
        return false;
      }
      rest = &rest[part.len()..];
    } else if i == last {
      return rest.ends_with(part);
    } else if !part.is_empty() {
      let Some(pos) = rest.find(part) else {
        return false;
      };
      rest = &rest[pos + part.len()..];
    }
  }
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  fn target(name: &str, patterns: &[&str], default: bool) -> CacheTarget {
    CacheTarget {
      name: name.to_owned(),
      match_patterns: patterns
        .iter()
        .map(std::string::ToString::to_string)
        .collect(),
      default,
      ..Default::default()
    }
  }

  #[test]
  fn most_specific_match_wins() {
    let a = target("a", &["gitlab.corp.example.com/*"], false);
    let b = target("b", &["gitlab.corp.example.com/team-x/*"], false);
    let targets = vec![a, b];

    let selected =
      select_targets(&targets, Some("gitlab.corp.example.com/team-x/widget"));
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "b");
  }

  #[test]
  fn no_match_no_default_selects_nothing() {
    let a = target("a", &["gitlab.corp.example.com/*"], false);
    let targets = vec![a];

    assert!(
      select_targets(&targets, Some("github.com/dauliac/widget")).is_empty()
    );
  }

  #[test]
  fn default_target_catches_unmatched_remote() {
    let a = target("a", &["gitlab.corp.example.com/*"], false);
    let fallback = target("fallback", &["gitlab.corp.example.com/*"], true);
    let targets = vec![a, fallback];

    let selected = select_targets(&targets, Some("github.com/dauliac/widget"));
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "fallback");
  }

  #[test]
  fn target_with_no_match_patterns_always_applies() {
    let legacy = target("legacy", &[], false);
    let targets = vec![legacy];

    let selected = select_targets(&targets, Some("github.com/dauliac/widget"));
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "legacy");

    // Still applies with no candidate resolved at all.
    assert_eq!(select_targets(&targets, None).len(), 1);
  }

  #[test]
  fn explicit_flake_ref_outranks_git_remote() {
    let candidate = resolve_candidate(
      Some("github:dauliac/widget"),
      Some("git@gitlab.corp.example.com:team-x/other.git"),
    );
    assert_eq!(candidate, Some("github:dauliac/widget".to_string()));
  }

  #[test]
  fn explicit_flake_ref_attribute_suffix_is_stripped() {
    let candidate = resolve_candidate(
      Some("github:dauliac/widget#homeConfigurations.foo"),
      None,
    );
    assert_eq!(candidate, Some("github:dauliac/widget".to_string()));
  }

  #[test]
  fn falls_back_to_git_remote_when_no_flake_ref_given() {
    let candidate = resolve_candidate(
      None,
      Some("git@gitlab.corp.example.com:team-x/widget.git"),
    );
    assert_eq!(
      candidate,
      Some("gitlab.corp.example.com/team-x/widget".to_string())
    );
  }

  #[test]
  fn glob_match_handles_bare_wildcard_suffix() {
    assert!(glob_match("github.com/dauliac/*", "github.com/dauliac/xi"));
    assert!(!glob_match("github.com/dauliac/*", "github.com/other/xi"));
  }

  #[test]
  fn glob_match_without_wildcard_requires_exact_match() {
    assert!(glob_match("github.com/dauliac/xi", "github.com/dauliac/xi"));
    assert!(!glob_match(
      "github.com/dauliac/xi",
      "github.com/dauliac/xi2"
    ));
  }

  #[test]
  fn github_shorthand_match_pattern_from_proposal() {
    let cache = target("personal", &["github:dauliac/*"], false);
    let targets = vec![cache];
    let candidate = resolve_candidate(Some("github:dauliac/xi"), None);
    let selected = select_targets(&targets, candidate.as_deref());
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "personal");
  }
}
