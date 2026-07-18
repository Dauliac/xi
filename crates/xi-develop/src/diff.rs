use std::collections::HashMap;
use std::time::Duration;

use crate::store_path::PackageInfo;

/// Computed diff between two package lists.
pub struct PackageDiff {
  pub added: Vec<PackageInfo>,
  pub removed: Vec<PackageInfo>,
  pub updated: Vec<(PackageInfo, PackageInfo)>,
}

impl PackageDiff {
  /// Compute the diff between old and new package lists.
  pub fn compute(old: &[PackageInfo], new: &[PackageInfo]) -> Self {
    let old_by_name: HashMap<&str, &PackageInfo> =
      old.iter().map(|p| (p.name.as_str(), p)).collect();
    let new_by_name: HashMap<&str, &PackageInfo> =
      new.iter().map(|p| (p.name.as_str(), p)).collect();

    let added = new
      .iter()
      .filter(|p| !old_by_name.contains_key(p.name.as_str()))
      .cloned()
      .collect();

    let removed = old
      .iter()
      .filter(|p| !new_by_name.contains_key(p.name.as_str()))
      .cloned()
      .collect();

    let updated = new
      .iter()
      .filter_map(|new_pkg| {
        let old_pkg = old_by_name.get(new_pkg.name.as_str())?;
        if old_pkg.version == new_pkg.version {
          None
        } else {
          Some(((*old_pkg).clone(), new_pkg.clone()))
        }
      })
      .collect();

    Self {
      added,
      removed,
      updated,
    }
  }

  /// Returns true if there are no changes.
  pub const fn is_empty(&self) -> bool {
    self.added.is_empty() && self.removed.is_empty() && self.updated.is_empty()
  }

  /// Format as plain text for notification (`Notification::render` adds prefix/color).
  pub fn to_notification_raw(
    &self,
    target: &str,
    duration: Duration,
  ) -> String {
    let target_str = if target == "default" {
      String::new()
    } else {
      format!(" '{target}'")
    };

    let mut lines = vec![format!(
      "devshell{target_str} updated ({:.1}s):",
      duration.as_secs_f64()
    )];

    let max_display = 15;
    let mut count = 0;
    let total = self.added.len() + self.updated.len() + self.removed.len();

    for pkg in &self.added {
      if count >= max_display {
        break;
      }
      let ver = pkg.version.as_deref().unwrap_or("");
      lines.push(format!(
        "  {} {:<24} {ver}",
        xi_core::style::Icon::Added.render(),
        pkg.name
      ));
      count += 1;
    }
    for (old, new) in &self.updated {
      if count >= max_display {
        break;
      }
      let old_v = old.version.as_deref().unwrap_or("?");
      let new_v = new.version.as_deref().unwrap_or("?");
      lines.push(format!(
        "  {} {:<24} {old_v} → {new_v}",
        xi_core::style::Icon::Changed.render(),
        new.name
      ));
      count += 1;
    }
    for pkg in &self.removed {
      if count >= max_display {
        break;
      }
      let ver = pkg.version.as_deref().unwrap_or("");
      lines.push(format!(
        "  {} {:<24} {ver}",
        xi_core::style::Icon::Removed.render(),
        pkg.name
      ));
      count += 1;
    }
    if total > max_display {
      lines.push(format!("  ... and {} more", total - max_display));
    }
    lines.join("\n")
  }

  /// Format for sync entry (full `dix`-style with yansi).
  pub fn print_full(&self) {
    use yansi::Paint;

    if self.is_empty() {
      return;
    }

    let total = self.added.len() + self.removed.len() + self.updated.len();
    crate::ui::info(format!("devshell changed ({total} packages)"));

    for pkg in &self.added {
      let ver = pkg.version.as_deref().unwrap_or("");
      eprintln!("         {} {:<28} {ver}", Paint::green("+"), pkg.name);
    }

    for (old, new) in &self.updated {
      let old_v = old.version.as_deref().unwrap_or("?");
      let new_v = new.version.as_deref().unwrap_or("?");
      eprintln!(
        "         {} {:<28} {old_v} → {new_v}",
        Paint::yellow("~"),
        new.name
      );
    }

    for pkg in &self.removed {
      let ver = pkg.version.as_deref().unwrap_or("");
      eprintln!("         {} {:<28} {ver}", Paint::red("-"), pkg.name);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn pkg(name: &str, version: &str) -> PackageInfo {
    PackageInfo {
      name: name.into(),
      version: Some(version.into()),
      store_path: format!("/nix/store/xxx-{name}-{version}/bin"),
    }
  }

  #[test]
  fn empty_diff() {
    let pkgs = vec![pkg("cargo", "1.95.0")];
    let diff = PackageDiff::compute(&pkgs, &pkgs);
    assert!(diff.is_empty());
  }

  #[test]
  fn added_package() {
    let old = vec![pkg("cargo", "1.95.0")];
    let new = vec![pkg("cargo", "1.95.0"), pkg("python", "3.12")];
    let diff = PackageDiff::compute(&old, &new);
    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.added[0].name, "python");
    assert!(diff.removed.is_empty());
    assert!(diff.updated.is_empty());
  }

  #[test]
  fn removed_package() {
    let old = vec![pkg("cargo", "1.95.0"), pkg("gcc", "13.3")];
    let new = vec![pkg("cargo", "1.95.0")];
    let diff = PackageDiff::compute(&old, &new);
    assert!(diff.added.is_empty());
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.removed[0].name, "gcc");
  }

  #[test]
  fn updated_package() {
    let old = vec![pkg("cargo", "1.94.0")];
    let new = vec![pkg("cargo", "1.95.0")];
    let diff = PackageDiff::compute(&old, &new);
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    assert_eq!(diff.updated.len(), 1);
    assert_eq!(diff.updated[0].0.version.as_deref(), Some("1.94.0"));
    assert_eq!(diff.updated[0].1.version.as_deref(), Some("1.95.0"));
  }

  #[test]
  fn notification_format() {
    let old = vec![pkg("cargo", "1.94.0")];
    let new = vec![pkg("cargo", "1.95.0"), pkg("python", "3.12")];
    let diff = PackageDiff::compute(&old, &new);
    let notif =
      diff.to_notification_raw("default", Duration::from_secs_f64(2.3));
    assert!(notif.contains("updated"));
    assert!(notif.contains("python"));
    assert!(notif.contains("1.94.0"));
    assert!(notif.contains("1.95.0"));
  }
}
