//! The `xi-binary-cache-registry` capability: compiled-in presets for
//! well-known public binary caches.
//!
//! `cache.nixos.org` is deliberately excluded  -  it's already Nix's own
//! default substituter, nothing to opt into. Strictly opt-in: no entry
//! here is ever added to anyone's substituters without a `[cache.<name>]`
//! target naming it via `preset = "<name>"`.

/// One compiled-in `{name -> (url, public_key)}` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownCache {
  /// The `preset = "<name>"` value that selects this entry.
  pub name:       &'static str,
  /// Substituter URL.
  pub url:        &'static str,
  /// Trusted public key for the substituter.
  pub public_key: &'static str,
}

/// The compiled-in registry, at minimum covering `cache.lix.systems`,
/// `nix-community.cachix.org`, `devenv.cachix.org`, and
/// `cache.garnix.io`.
pub const KNOWN_CACHES: &[KnownCache] = &[
  KnownCache {
    name:       "lix",
    url:        "https://cache.lix.systems",
    public_key: "cache.lix.systems:\
                 aBnZUw8zA7H35Cz2RyKFVs3H4PlGTLawyY5KRbvJR8o=",
  },
  KnownCache {
    name:       "nix-community",
    url:        "https://nix-community.cachix.org",
    public_key: "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/\
                 rkCWyvRCYg3Fs=",
  },
  KnownCache {
    name:       "devenv",
    url:        "https://devenv.cachix.org",
    public_key: "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/\
                 Hm9XgU50cw=",
  },
  KnownCache {
    name:       "garnix",
    url:        "https://cache.garnix.io",
    public_key: "cache.garnix.io:CTFPyKSLcx5RMJKfLo5EEPUObbA78b0YQ2DTCJXqr9g=",
  },
];

/// Look up a preset by its `preset = "<name>"` value.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static KnownCache> {
  KNOWN_CACHES.iter().find(|entry| entry.name == name)
}

/// The names of every known preset, in declaration order  -  used to build
/// the "expected one of: ..." message for an unknown preset name.
#[must_use]
pub fn known_names() -> Vec<&'static str> {
  KNOWN_CACHES.iter().map(|entry| entry.name).collect()
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::{known_names, lookup};

  #[test]
  fn resolves_a_known_preset() {
    let entry = lookup("nix-community").expect("nix-community is known");
    assert_eq!(entry.url, "https://nix-community.cachix.org");
    assert!(entry.public_key.starts_with("nix-community.cachix.org-1:"));
  }

  #[test]
  fn unknown_preset_resolves_to_none() {
    assert!(lookup("not-a-real-preset").is_none());
  }

  #[test]
  fn known_names_lists_every_entry() {
    let names = known_names();
    assert!(names.contains(&"lix"));
    assert!(names.contains(&"nix-community"));
    assert!(names.contains(&"devenv"));
    assert!(names.contains(&"garnix"));
    assert!(!names.contains(&"cache.nixos.org"));
  }
}
