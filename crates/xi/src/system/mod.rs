pub mod args;

use std::path::PathBuf;

use args::{SystemArgs, SystemRebuildArgs, SystemSubcommand};
use color_eyre::{
  Result,
  eyre::{Context, bail},
};
use xi_core::installable::{CommandContext, Installable};
use xi_core::{
  args::DiffType,
  command::{Command, ElevationStrategy, find_real_nix_binary},
  update::update,
  util::get_hostname,
};
use xi_diff::print_dix_diff;
use tracing::{debug, info, warn};

/// Profile path used by system-manager for nix-env generations
const SYSTEM_PROFILE: &str =
  "/nix/var/nix/profiles/system-manager-profiles/system-manager";

impl SystemArgs {
  /// Run the `system` subcommand.
  ///
  /// # Errors
  ///
  /// Returns an error if build or activation operations fail.
  pub fn run(self, elevation: ElevationStrategy) -> Result<()> {
    use SystemRebuildVariant::{Build, Switch};
    match self.subcommand {
      SystemSubcommand::Switch(args) => args.rebuild(&Switch, elevation),
      SystemSubcommand::Build(args) => {
        if args.common.ask || args.common.dry {
          warn!("`--ask` and `--dry` have no effect for `xi system build`");
        }
        args.rebuild(&Build, elevation)
      },
    }
  }
}

enum SystemRebuildVariant {
  Switch,
  Build,
}

impl SystemRebuildArgs {
  fn rebuild(
    self,
    variant: &SystemRebuildVariant,
    elevation: ElevationStrategy,
  ) -> Result<()> {
    use SystemRebuildVariant::{Build, Switch};

    if nix::unistd::Uid::effective().is_root() && !self.bypass_root_check {
      bail!(
        "Don't run xi system as root. I will call sudo internally as \
         needed"
      );
    }

    let hostname = get_hostname(self.hostname)?;

    let (out_path, _tempdir_guard): (PathBuf, Option<tempfile::TempDir>) =
      if let Some(ref p) = self.common.out_link {
        (p.clone(), None)
      } else {
        let dir = tempfile::Builder::new().prefix("xi-system").tempdir()?;
        (dir.as_ref().join("result"), Some(dir))
      };

    debug!("Output path: {out_path:?}");

    let installable = self
      .common
      .installable
      .clone()
      .resolve_or_default(CommandContext::System)?;

    if self.update_args.update_all || self.update_args.update_input.is_some() {
      update(
        &installable,
        self.update_args.update_input,
        self.common.passthrough.commit_lock_file,
      )?;
    }

    let toplevel = toplevel_for(hostname, installable)?;

    xi_core::command::Build::new(toplevel)
      .extra_arg("--out-link")
      .extra_arg(&out_path)
      .extra_args(&self.extra_args)
      .passthrough(&self.common.passthrough)
      .message("Building system-manager configuration")
      .nom(!self.common.no_nom)
      .run()
      .wrap_err("Failed to build system-manager configuration")?;

    // Push to cache after successful build (best-effort)
    if xi_core::cache::is_push_configured(&self.common.cache) {
      xi_core::cache::push_to_cache(&self.common.cache, &out_path);
    }

    let target_profile = out_path.clone();

    target_profile.try_exists().context("Doesn't exist")?;

    // Compare changes between current and target generation
    if matches!(self.common.diff, DiffType::Never) {
      debug!("Not running dix as the --diff flag is set to never.");
    } else {
      debug!(
        "Comparing with target profile: {}",
        target_profile.display()
      );
      let _ = print_dix_diff(&PathBuf::from(SYSTEM_PROFILE), &target_profile);
    }

    if self.common.ask && !self.common.dry && !matches!(variant, Build) {
      let confirmation = inquire::Confirm::new("Apply the config?")
        .with_default(false)
        .prompt()?;

      if !confirmation {
        bail!("User rejected the new config");
      }
    }

    if matches!(variant, Switch) {
      // Register: set the system-manager nix profile to the new build
      // Resolve the symlink to the actual store path, otherwise nix
      // interprets the temp-dir path as a flake reference.
      let store_path = out_path
        .canonicalize()
        .context("Failed to resolve output path to store path")?;

      Command::new(find_real_nix_binary())
        .args(["build", "--no-link", "--profile", SYSTEM_PROFILE])
        .arg(&store_path)
        .elevate(Some(elevation.clone()))
        .dry(self.common.dry)
        .with_required_env()
        .run()
        .wrap_err("Failed to register system-manager profile")?;

      // Activate: run the activate script from the built store path
      let activate_script = out_path.join("bin/activate");

      Command::new(activate_script)
        .message("Activating system-manager configuration")
        .elevate(Some(elevation))
        .dry(self.common.dry)
        .show_output(self.show_activation_logs)
        .with_required_env()
        .run()
        .wrap_err("system-manager activation failed")?;
    }

    debug!("Completed operation with output path: {out_path:?}");

    Ok(())
  }
}

/// Resolve a system-manager installable to the correct flake attribute.
///
/// system-manager outputs are at `systemConfigs.<hostname>`. Unlike NixOS
/// or nix-darwin, the flake output IS the final derivation — there is no
/// `.config.system.build.toplevel` nesting.
///
/// # Errors
///
/// Returns an error if the installable is a store path.
pub fn toplevel_for<S: AsRef<str>>(
  hostname: S,
  installable: Installable,
) -> Result<Installable> {
  let mut res = installable;
  let hostname_str = hostname.as_ref();

  match res {
    Installable::Flake {
      ref mut attribute, ..
    } => {
      if attribute.is_empty() {
        attribute.push(String::from("systemConfigs"));
        attribute.push(hostname_str.to_owned());
      } else if attribute.len() == 1 && attribute[0] == "systemConfigs" {
        info!("Inferring hostname '{hostname_str}' for systemConfigs");
        attribute.push(hostname_str.to_owned());
      } else if attribute[0] == "systemConfigs" {
        if attribute.len() == 2 {
          // systemConfigs.hostname - fine
        } else if attribute.len() > 2 {
          bail!(
            "Attribute path is too specific: {}. Please either:\n  \
             1. Use the flake reference without attributes (e.g., \
             '.')\n  2. Specify only the configuration name (e.g., \
             '.#{}')",
            attribute.join("."),
            attribute[1]
          );
        }
      } else {
        // User provided ".#myhost" - prepend systemConfigs
        attribute.insert(0, String::from("systemConfigs"));
      }
    },
    Installable::File { .. } | Installable::Expression { .. } => {
      // For file/expression mode, keep attributes as-is
    },
    Installable::Store { .. } => {
      bail!("Store paths are not supported for system-manager.");
    },
  }

  Ok(res)
}
