pub mod args;

use std::{convert::Into, path::PathBuf};

use args::{DarwinArgs, DarwinRebuildArgs, DarwinReplArgs, DarwinSubcommand};
use color_eyre::{
  Result,
  eyre::{Context, bail},
};
use tracing::{debug, info, warn};
use xi_core::installable::{CommandContext, Installable, resolve_toplevel};
use xi_core::{
  args::DiffType,
  command::{
    Command, CommandKind, ElevationStrategy, NixCommand, find_real_nix_binary,
  },
  update::update,
  util::get_hostname,
};
use xi_diff::print_dix_diff;
use xi_remote::{self, RemoteBuildConfig};

const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";
const CURRENT_PROFILE: &str = "/run/current-system";

impl DarwinArgs {
  /// Run the `darwin` subcommand.
  ///
  /// # Parameters
  ///
  /// * `self` - The Darwin operation arguments
  /// * `elevation` - The privilege elevation strategy (sudo/doas/none)
  ///
  /// # Returns
  ///
  /// Returns `Ok(())` if the operation succeeds.
  ///
  /// # Errors
  ///
  /// Returns an error if:
  ///
  /// - Build or activation operations fail
  /// - Remote operations encounter network or SSH issues
  /// - Nix evaluation or building fails
  /// - File system operations fail
  pub fn run(self, elevation: ElevationStrategy) -> Result<()> {
    use DarwinRebuildVariant::{Build, Switch};
    match self.subcommand {
      DarwinSubcommand::Switch(args) => args.rebuild(&Switch, elevation),
      DarwinSubcommand::Build(args) => {
        if args.common.ask || args.common.dry {
          warn!("`--ask` and `--dry` have no effect for `xi darwin build`");
        }
        args.rebuild(&Build, elevation)
      },
      DarwinSubcommand::Repl(args) => args.run(),
    }
  }
}

enum DarwinRebuildVariant {
  Switch,
  Build,
}

impl DarwinRebuildArgs {
  fn rebuild(
    self,
    variant: &DarwinRebuildVariant,
    elevation: ElevationStrategy,
  ) -> Result<()> {
    use DarwinRebuildVariant::{Build, Switch};

    if nix::unistd::Uid::effective().is_root() && !self.bypass_root_check {
      bail!(
        "Don't run xi darwin as root. I will call sudo internally as needed"
      );
    }

    let hostname = get_hostname(self.hostname)?;

    let (out_path, _tempdir_guard): (PathBuf, Option<tempfile::TempDir>) =
      if let Some(ref p) = self.common.out_link {
        (p.clone(), None)
      } else {
        let dir = tempfile::Builder::new().prefix("xi-darwin").tempdir()?;
        (dir.as_ref().join("result"), Some(dir))
      };

    debug!("Output path: {out_path:?}");

    let installable = self
      .common
      .installable
      .clone()
      .resolve_or_default(CommandContext::Darwin)?;

    if self.update_args.update_all || self.update_args.update_input.is_some() {
      update(
        &installable,
        self.update_args.update_input,
        self.common.passthrough.commit_lock_file,
      )?;
    }

    let toplevel = toplevel_for(hostname, installable, "toplevel")?;

    // Extract suggestion info before build consumes the installable
    let (suggest_ref, suggest_hostname) = toplevel.suggestion_context();

    // If a build host is specified, use remote build semantics
    if let Some(build_host) = self.build_host.clone() {
      info!("Building Darwin configuration");

      let config = RemoteBuildConfig {
        build_host,
        target_host: None,
        use_nom: !self.common.no_nom,
        use_substitutes: self.common.passthrough.use_substitutes,
        extra_args: self
          .extra_args
          .iter()
          .map(Into::into)
          .chain(
            self
              .common
              .passthrough
              .generate_passthrough_args()
              .into_iter()
              .map(Into::into),
          )
          .collect(),
      };

      // Initialize SSH control - guard will cleanup connections on drop
      let _ssh_guard = xi_remote::init_ssh_control();

      let result = xi_remote::build_remote(&toplevel, &config, Some(&out_path))
        .wrap_err("Failed to build Darwin configuration");
      if result.is_err()
        && !suggest_ref.is_empty()
        && !suggest_hostname.is_empty()
      {
        xi_core::suggest::print_suggestions_on_failure(
          &suggest_ref,
          &suggest_hostname,
          Some("darwinConfigurations"),
        );
      }
      result?;
    } else {
      let result = xi_core::command::Build::new(toplevel)
        .extra_arg("--out-link")
        .extra_arg(&out_path)
        .extra_args(&self.extra_args)
        .passthrough(&self.common.passthrough)
        .message("Building Darwin configuration")
        .nom(!self.common.no_nom)
        .run()
        .wrap_err("Failed to build Darwin configuration");
      if result.is_err()
        && !suggest_ref.is_empty()
        && !suggest_hostname.is_empty()
      {
        xi_core::suggest::print_suggestions_on_failure(
          &suggest_ref,
          &suggest_hostname,
          Some("darwinConfigurations"),
        );
      }
      result?;
    }

    // Push to cache after successful build (best-effort)
    if xi_core::cache::is_push_configured(&self.common.cache) {
      xi_core::cache::push_to_cache(&self.common.cache, &out_path);
    }

    let target_profile = out_path.clone();

    target_profile.try_exists().context("Doesn't exist")?;

    debug!(
      "Comparing with target profile: {}",
      target_profile.display()
    );

    // Compare changes between current and target generation
    if matches!(self.common.diff, DiffType::Never) {
      debug!("Not running dix as the --diff flag is set to never.");
    } else {
      debug!(
        "Comparing with target profile: {}",
        target_profile.display()
      );
      let _ = print_dix_diff(&PathBuf::from(CURRENT_PROFILE), &target_profile);
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
      Command::new(find_real_nix_binary())
        .args(["build", "--no-link", "--profile", SYSTEM_PROFILE])
        .arg(&out_path)
        .elevate(Some(elevation.clone()))
        .dry(self.common.dry)
        .with_required_env()
        .run()
        .wrap_err("Failed to set Darwin system profile")?;

      let darwin_rebuild = out_path.join("sw/bin/darwin-rebuild");
      let activate_user = out_path.join("activate-user");

      // Determine if we need to elevate privileges
      let needs_elevation = !activate_user
        .try_exists()
        .context("Failed to check if activate-user file exists")?
        || std::fs::read_to_string(&activate_user)
          .context("Failed to read activate-user file")?
          .contains("# nix-darwin: deprecated");

      // Create and run the activation command with or without elevation
      Command::new(darwin_rebuild)
        .arg("activate")
        .message("Activating configuration")
        .elevate(needs_elevation.then_some(elevation))
        .dry(self.common.dry)
        .show_output(self.show_activation_logs)
        .with_required_env()
        .run()
        .wrap_err("Darwin activation failed")?;
    }

    debug!("Completed operation with output path: {out_path:?}");

    Ok(())
  }
}

impl DarwinReplArgs {
  fn run(self) -> Result<()> {
    let mut target_installable = self
      .installable
      .resolve_or_default(CommandContext::Darwin)?;

    if matches!(target_installable, Installable::Store { .. }) {
      bail!("Nix doesn't support nix store installables.");
    }

    let hostname = get_hostname(self.hostname)?;

    if let Installable::Flake {
      ref mut attribute, ..
    } = target_installable
      && attribute.is_empty()
    {
      attribute.push(String::from("darwinConfigurations"));
      attribute.push(hostname);
    }

    let status = NixCommand::new(CommandKind::Repl)
      .args(target_installable.to_args())
      .with_required_env()
      .run_with_logs()?;
    if !status.success() {
      bail!("nix repl failed (exit status {status:?})");
    }

    Ok(())
  }
}

/// Resolve a nix-darwin installable to the requested system build attribute.
///
/// Delegates to [`resolve_toplevel`] with `darwinConfigurations` as the
/// config key and `["config", "system", "build", final_attr]` as the
/// build path.
///
/// # Errors
///
/// Returns an error if the installable is a store path or if the flake
/// attribute path is too specific to infer the requested build attribute.
pub fn toplevel_for<S: AsRef<str>>(
  hostname: S,
  installable: Installable,
  final_attr: &str,
) -> Result<Installable> {
  resolve_toplevel(
    hostname.as_ref(),
    installable,
    "darwinConfigurations",
    &["config", "system", "build", final_attr],
  )
}
