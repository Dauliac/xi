//! Centralized definitions for Nix flake output categories and types.
//!
//! This module is the single source of truth for:
//! - Flake output category names (`packages`, `devShells`, `nixosConfigurations`, …)
//! - Per-system vs flat classification
//! - Display ordering for `xi show`
//! - Output kind/type labels (`derivation`, `module`, `overlay`, …)
//! - The current Nix system string (`x86_64-linux`, `aarch64-darwin`, …)

use std::fmt;

// ---------------------------------------------------------------------------
// FlakeOutput — well-known flake output categories
// ---------------------------------------------------------------------------

/// Well-known Nix flake output categories.
///
/// Each variant maps to a top-level attribute key in a flake's `outputs`.
/// The enum centralises metadata that was previously scattered as string
/// constants across `show.rs`, `suggest.rs`, `complete.rs`, `ci.rs`, and
/// the per-platform crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlakeOutput {
  Packages,
  DevShells,
  Checks,
  Apps,
  Formatter,
  LegacyPackages,
  Overlays,
  NixosModules,
  NixosConfigurations,
  HomeConfigurations,
  HomeModules,
  DarwinModules,
  DarwinConfigurations,
  SystemConfigs,
  Templates,
  Lib,
}

impl FlakeOutput {
  /// All known variants in preferred display order for `xi show`.
  pub const DISPLAY_ORDER: &[Self] = &[
    Self::Packages,
    Self::DevShells,
    Self::Checks,
    Self::Apps,
    Self::Formatter,
    Self::Overlays,
    Self::NixosModules,
    Self::NixosConfigurations,
    Self::HomeConfigurations,
    Self::HomeModules,
    Self::DarwinModules,
    Self::DarwinConfigurations,
    Self::SystemConfigs,
    Self::Templates,
    Self::Lib,
    Self::LegacyPackages,
  ];

  /// The Nix attribute name for this output category.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Packages => "packages",
      Self::DevShells => "devShells",
      Self::Checks => "checks",
      Self::Apps => "apps",
      Self::Formatter => "formatter",
      Self::LegacyPackages => "legacyPackages",
      Self::Overlays => "overlays",
      Self::NixosModules => "nixosModules",
      Self::NixosConfigurations => "nixosConfigurations",
      Self::HomeConfigurations => "homeConfigurations",
      Self::HomeModules => "homeModules",
      Self::DarwinModules => "darwinModules",
      Self::DarwinConfigurations => "darwinConfigurations",
      Self::SystemConfigs => "systemConfigs",
      Self::Templates => "templates",
      Self::Lib => "lib",
    }
  }

  /// Whether outputs in this category are nested per-system
  /// (e.g. `packages.<system>.<name>`).
  #[must_use]
  pub const fn is_per_system(self) -> bool {
    matches!(
      self,
      Self::Packages
        | Self::DevShells
        | Self::Checks
        | Self::Apps
        | Self::Formatter
        | Self::LegacyPackages
    )
  }

  /// Whether devour-flake already handles this category when building
  /// all outputs.
  #[must_use]
  pub const fn is_devour_handled(self) -> bool {
    matches!(
      self,
      Self::Packages
        | Self::Checks
        | Self::DevShells
        | Self::Apps
        | Self::NixosConfigurations
        | Self::DarwinConfigurations
        | Self::LegacyPackages
    )
  }

  /// The inferred [`FlakeOutputKind`] for entries in this category,
  /// if it can be determined from the category name alone.
  #[must_use]
  pub const fn inferred_kind(self) -> Option<FlakeOutputKind> {
    match self {
      Self::Packages | Self::DevShells | Self::Checks | Self::LegacyPackages => {
        Some(FlakeOutputKind::Derivation)
      },
      Self::Apps => Some(FlakeOutputKind::App),
      Self::Formatter => Some(FlakeOutputKind::Derivation),
      Self::Overlays => Some(FlakeOutputKind::Overlay),
      Self::NixosModules | Self::HomeModules | Self::DarwinModules => {
        Some(FlakeOutputKind::Module)
      },
      Self::NixosConfigurations
      | Self::HomeConfigurations
      | Self::DarwinConfigurations
      | Self::SystemConfigs => Some(FlakeOutputKind::Configuration),
      Self::Templates => Some(FlakeOutputKind::Template),
      Self::Lib => Some(FlakeOutputKind::Lib),
    }
  }

  /// Parse a Nix attribute name to a known output category.
  ///
  /// Returns `None` for unknown/custom category names.
  #[must_use]
  pub fn from_nix_name(s: &str) -> Option<Self> {
    match s {
      "packages" => Some(Self::Packages),
      "devShells" => Some(Self::DevShells),
      "checks" => Some(Self::Checks),
      "apps" => Some(Self::Apps),
      "formatter" => Some(Self::Formatter),
      "legacyPackages" => Some(Self::LegacyPackages),
      "overlays" => Some(Self::Overlays),
      "nixosModules" => Some(Self::NixosModules),
      "nixosConfigurations" => Some(Self::NixosConfigurations),
      "homeConfigurations" => Some(Self::HomeConfigurations),
      "homeModules" => Some(Self::HomeModules),
      "darwinModules" => Some(Self::DarwinModules),
      "darwinConfigurations" => Some(Self::DarwinConfigurations),
      "systemConfigs" => Some(Self::SystemConfigs),
      "templates" => Some(Self::Templates),
      "lib" => Some(Self::Lib),
      _ => None,
    }
  }

  /// Whether this category appears in [`Self::DISPLAY_ORDER`].
  #[must_use]
  pub fn is_in_display_order(name: &str) -> bool {
    Self::DISPLAY_ORDER
      .iter()
      .any(|output| output.as_str() == name)
  }

  /// Check if the given category name is per-system.
  ///
  /// Works for both known and unknown categories (unknown → `false`).
  #[must_use]
  pub fn is_name_per_system(name: &str) -> bool {
    Self::from_nix_name(name).is_some_and(Self::is_per_system)
  }
}

impl fmt::Display for FlakeOutput {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

// ---------------------------------------------------------------------------
// FlakeOutputKind — semantic type of a flake output entry
// ---------------------------------------------------------------------------

/// The semantic kind/type of a flake output entry.
///
/// Nix's `nix flake show --json` returns a `type` field for each entry.
/// This enum covers those known types plus labels inferred from naming
/// conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlakeOutputKind {
  /// A Nix derivation (buildable output).
  Derivation,
  /// A Nix app definition (`{ type = "app"; program = …; }`).
  App,
  /// A NixOS configuration (`type: "nixos-configuration"`).
  NixosConfiguration,
  /// A nixpkgs overlay (`type: "nixpkgs-overlay"`).
  Overlay,
  /// A NixOS / Home Manager / Darwin module.
  Module,
  /// A system configuration (home, darwin, system-manager, …).
  Configuration,
  /// A library attribute set.
  Lib,
  /// A flake template.
  Template,
  /// An opaque Nix function.
  Function,
  /// A test case (`{ expected = …; expr = …; }` pattern).
  Test,
  /// Unknown / opaque type.
  Unknown,
}

impl FlakeOutputKind {
  /// Human-readable display label.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Derivation => "derivation",
      Self::App => "app",
      Self::NixosConfiguration => "nixos-configuration",
      Self::Overlay => "overlay",
      Self::Module => "module",
      Self::Configuration => "configuration",
      Self::Lib => "lib",
      Self::Template => "template",
      Self::Function => "function",
      Self::Test => "test",
      Self::Unknown => "unknown",
    }
  }

  /// Parse from nix's JSON `type` field value.
  #[must_use]
  pub fn from_nix_type(s: &str) -> Self {
    match s {
      "derivation" => Self::Derivation,
      "nixos-configuration" => Self::NixosConfiguration,
      "nixpkgs-overlay" => Self::Overlay,
      _ => Self::Unknown,
    }
  }

  /// Try to infer the kind from a category name using naming conventions.
  ///
  /// Checks known [`FlakeOutput`] variants first, then falls back to
  /// suffix-based heuristics for custom/unknown category names.
  #[must_use]
  pub fn infer_from_category(name: &str) -> Option<Self> {
    // Known categories have a definitive answer.
    if let Some(output) = FlakeOutput::from_nix_name(name) {
      return output.inferred_kind();
    }

    // Heuristics for unknown/custom category names.
    if name.ends_with("Modules")
      || name.ends_with("modules")
      || name == "modules"
    {
      Some(Self::Module)
    } else if name.ends_with("Configurations")
      || name.ends_with("configurations")
      || name.ends_with("Configs")
      || name.ends_with("configs")
    {
      Some(Self::Configuration)
    } else if name == "lib"
      || name.ends_with("Lib")
      || name.ends_with("libs")
      || name.ends_with("Libs")
    {
      Some(Self::Lib)
    } else {
      None
    }
  }

  /// Returns `true` if a category name matches a known output pattern
  /// (either a known [`FlakeOutput`] or a naming-convention match).
  #[must_use]
  pub fn is_known_pattern(name: &str) -> bool {
    Self::infer_from_category(name).is_some()
  }
}

impl fmt::Display for FlakeOutputKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

// ---------------------------------------------------------------------------
// Hidden categories
// ---------------------------------------------------------------------------

/// Category names hidden by default in `xi show`.
///
/// These are flake-parts internals or debug outputs, not standard flake
/// output categories.
pub const HIDDEN_CATEGORIES: &[&str] = &["debug", "allSystems"];

/// Returns `true` if this category name is hidden by default.
#[must_use]
pub fn is_hidden_by_default(name: &str) -> bool {
  HIDDEN_CATEGORIES.contains(&name)
}

// ---------------------------------------------------------------------------
// System helpers
// ---------------------------------------------------------------------------

/// Return the current Nix system string (e.g. `x86_64-linux`, `aarch64-darwin`).
///
/// This is the canonical implementation — other modules should call this
/// instead of reimplementing the arch/os mapping.
#[must_use]
pub fn current_nix_system() -> String {
  let arch = std::env::consts::ARCH;
  let os = match std::env::consts::OS {
    "macos" => "darwin",
    other => other,
  };
  format!("{arch}-{os}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn round_trip_all_variants() {
    for &output in FlakeOutput::DISPLAY_ORDER {
      let name = output.as_str();
      let parsed = FlakeOutput::from_nix_name(name);
      assert_eq!(
        parsed,
        Some(output),
        "round-trip failed for {name}"
      );
    }
  }

  #[test]
  fn per_system_variants() {
    assert!(FlakeOutput::Packages.is_per_system());
    assert!(FlakeOutput::DevShells.is_per_system());
    assert!(FlakeOutput::Checks.is_per_system());
    assert!(FlakeOutput::Apps.is_per_system());
    assert!(FlakeOutput::Formatter.is_per_system());
    assert!(FlakeOutput::LegacyPackages.is_per_system());

    assert!(!FlakeOutput::Overlays.is_per_system());
    assert!(!FlakeOutput::NixosConfigurations.is_per_system());
    assert!(!FlakeOutput::HomeConfigurations.is_per_system());
    assert!(!FlakeOutput::Lib.is_per_system());
  }

  #[test]
  fn devour_handled() {
    assert!(FlakeOutput::Packages.is_devour_handled());
    assert!(FlakeOutput::NixosConfigurations.is_devour_handled());
    assert!(!FlakeOutput::Overlays.is_devour_handled());
    assert!(!FlakeOutput::Lib.is_devour_handled());
    assert!(!FlakeOutput::HomeModules.is_devour_handled());
  }

  #[test]
  fn unknown_category_returns_none() {
    assert_eq!(FlakeOutput::from_nix_name("myCustomOutput"), None);
    assert_eq!(FlakeOutput::from_nix_name("debug"), None);
  }

  #[test]
  fn inferred_kinds() {
    assert_eq!(
      FlakeOutput::Packages.inferred_kind(),
      Some(FlakeOutputKind::Derivation)
    );
    assert_eq!(
      FlakeOutput::NixosModules.inferred_kind(),
      Some(FlakeOutputKind::Module)
    );
    assert_eq!(
      FlakeOutput::Overlays.inferred_kind(),
      Some(FlakeOutputKind::Overlay)
    );
    assert_eq!(
      FlakeOutput::Lib.inferred_kind(),
      Some(FlakeOutputKind::Lib)
    );
  }

  #[test]
  fn infer_kind_from_unknown_category() {
    assert_eq!(
      FlakeOutputKind::infer_from_category("customModules"),
      Some(FlakeOutputKind::Module)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("myConfigurations"),
      Some(FlakeOutputKind::Configuration)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("evalLib"),
      Some(FlakeOutputKind::Lib)
    );
    assert_eq!(
      FlakeOutputKind::infer_from_category("randomThing"),
      None
    );
  }

  #[test]
  fn nix_type_parsing() {
    assert_eq!(
      FlakeOutputKind::from_nix_type("derivation"),
      FlakeOutputKind::Derivation
    );
    assert_eq!(
      FlakeOutputKind::from_nix_type("nixos-configuration"),
      FlakeOutputKind::NixosConfiguration
    );
    assert_eq!(
      FlakeOutputKind::from_nix_type("nixpkgs-overlay"),
      FlakeOutputKind::Overlay
    );
    assert_eq!(
      FlakeOutputKind::from_nix_type("unknown"),
      FlakeOutputKind::Unknown
    );
    assert_eq!(
      FlakeOutputKind::from_nix_type("something-else"),
      FlakeOutputKind::Unknown
    );
  }

  #[test]
  fn hidden_categories() {
    assert!(is_hidden_by_default("debug"));
    assert!(is_hidden_by_default("allSystems"));
    assert!(!is_hidden_by_default("packages"));
    assert!(!is_hidden_by_default("nixosModules"));
  }

  #[test]
  fn current_system_format() {
    let sys = current_nix_system();
    assert!(sys.contains('-'), "system should be arch-os: {sys}");
    let parts: Vec<&str> = sys.split('-').collect();
    assert_eq!(parts.len(), 2);
    assert!(!parts[0].is_empty());
    assert!(!parts[1].is_empty());
  }

  #[test]
  fn display_impls() {
    assert_eq!(FlakeOutput::Packages.to_string(), "packages");
    assert_eq!(FlakeOutputKind::Module.to_string(), "module");
  }

  #[test]
  fn is_known_pattern_covers_known_and_heuristic() {
    assert!(FlakeOutputKind::is_known_pattern("packages"));
    assert!(FlakeOutputKind::is_known_pattern("nixosModules"));
    assert!(FlakeOutputKind::is_known_pattern("customModules"));
    assert!(FlakeOutputKind::is_known_pattern("systemConfigs"));
    assert!(!FlakeOutputKind::is_known_pattern("randomThing"));
  }
}
