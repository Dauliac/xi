pub mod args;
pub mod ci;
pub(crate) mod doctor;
pub mod flake_lib;
pub(crate) mod locate;
pub(crate) mod materialize;
pub(crate) mod project_config;
pub(crate) mod show;
pub(crate) mod test;

use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::bail;
use nix_command::{CommandKind, NixCommand};
use subprocess::{Exec, Redirection};
use tracing::{debug, info, warn};
use walkdir::WalkDir;
use xi_core::command::ExitError;
use xi_core::installable::Installable;

use crate::args::{
  BuildArgs, CheckArgs, CiBackend, FmtArgs, FmtBackend, InitArgs, RunArgs,
  ShowArgs, UpdateArgs,
};

use xi_core::flake_output::FlakeOutput;

// ---------------------------------------------------------------------------
// Suggestion helpers
// ---------------------------------------------------------------------------

/// Extract the flake reference string from an installable, for suggestion
/// purposes. Returns `.` for unspecified or bare-name installables.
fn suggestion_flake_ref(
  installable: &xi_core::installable::InstallableArgs,
) -> String {
  match installable {
    xi_core::installable::InstallableArgs::Unspecified => ".".to_string(),
    xi_core::installable::InstallableArgs::Specified(inst) => match inst {
      xi_core::installable::Installable::Flake { reference, .. } => {
        if is_flake_ref(reference) {
          reference.clone()
        } else {
          ".".to_string()
        }
      },
      _ => String::new(),
    },
  }
}

/// Extract the leaf attribute the user was targeting from an installable.
fn suggestion_attr(
  installable: &xi_core::installable::InstallableArgs,
) -> Option<String> {
  match installable {
    xi_core::installable::InstallableArgs::Unspecified => None,
    xi_core::installable::InstallableArgs::Specified(inst) => match inst {
      xi_core::installable::Installable::Flake {
        reference,
        attribute,
      } => {
        if attribute.is_empty() && !is_flake_ref(reference) {
          // Bare name treated as attribute
          Some(reference.clone())
        } else {
          attribute.last().cloned()
        }
      },
      _ => None,
    },
  }
}

/// Pinned devour-flake revision for `--all` builds.
/// <https://github.com/srid/devour-flake>
pub(crate) const DEVOUR_FLAKE_REV: &str =
  "e65d15fd4ef46dbde90ac59be581b2a286c35d0f";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Return the current nix system string (e.g. `x86_64-linux`, `aarch64-darwin`).
pub(crate) fn current_nix_system() -> String {
  xi_core::flake_output::current_nix_system()
}

/// Resolve installable args to a nix argument list.
///
/// Bare names like `xi` (no `/`, `.`, `:`) are treated as attributes of the
/// current flake (`.#xi`) rather than flake registry lookups. This matches
/// the xi philosophy of defaulting to the local flake.
fn installable_to_args(
  installable: &xi_core::installable::InstallableArgs,
) -> Vec<String> {
  match installable {
    xi_core::installable::InstallableArgs::Unspecified => vec![],
    xi_core::installable::InstallableArgs::Specified(inst) => {
      match inst {
        // Bare flake ref with no attribute that doesn't look like a path/URL
        // → treat as attribute of the current flake
        Installable::Flake {
          reference,
          attribute,
        } if attribute.is_empty() && !is_flake_ref(reference) => {
          let resolved = Installable::Flake {
            reference: ".".to_string(),
            attribute: vec![reference.clone()],
          };
          resolved.to_args()
        },
        _ => inst.to_args(),
      }
    },
  }
}

/// Execute a nix build command, choosing between nom pipeline and streaming.
///
/// This is the shared build flow used by `build`, `check`, and `ci`.
pub(crate) fn execute_build(
  cmd: &NixCommand,
  no_nom: bool,
  dry: bool,
) -> Result<()> {
  let base = cmd.to_exec();
  if !no_nom && !dry {
    run_with_nom(base)
  } else {
    run_exec_streaming(base)
  }
}

/// Run a nix command interactively, inheriting stdio.
///
/// Used by `run`, `develop`, `fmt`, and other interactive commands.
/// Always inherits stdio so the child process sees the real TTY and
/// preserves colors/progress output from `nix`.
fn run_interactive(cmd: &NixCommand) -> Result<()> {
  use std::process::Stdio;

  debug!(argv = ?cmd.argv());

  let status = cmd
    .to_std_command()
    .stdin(Stdio::inherit())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .status()
    .map_err(|e| color_eyre::eyre::eyre!("nix command failed: {e}"))?;

  if !status.success() {
    bail!("nix command exited with status {status}");
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------

impl BuildArgs {
  /// Run the build command.
  ///
  /// # Errors
  ///
  /// Returns an error if the build fails.
  pub fn run(self) -> Result<()> {
    if self.all {
      return self.run_build_all();
    }

    let local_dir = resolve_local_flake_dir_from_installable(&self.installable);

    // Pre-build materialization (if configured in .xi.toml)
    if let Some(ref dir) = local_dir {
      materialize::run_pre_build_materialize(dir)?;
    }

    ensure_flake_locked(local_dir)?;

    // Capture suggestion info before moving installable
    let suggest_ref = suggestion_flake_ref(&self.installable);
    let suggest_attr = suggestion_attr(&self.installable);

    let installable_args = installable_to_args(&self.installable);

    let target_display = installable_args.first().cloned().unwrap_or_default();
    if target_display.is_empty() {
      info!("Building flake output");
    } else {
      info!("Building flake output \"{target_display}\"");
    }
    let passthrough_args = self.passthrough.to_nix_args();

    let mut cmd = NixCommand::new(CommandKind::Build)
      .print_build_logs(false)
      .args(&installable_args)
      .args(&passthrough_args);

    if self.no_link {
      cmd = cmd.arg("--no-link");
    }

    if let Some(ref out_link) = self.out_link {
      cmd = cmd
        .arg("--out-link")
        .arg(out_link.to_string_lossy().as_ref());
    }

    cmd = cmd.args(&self.extra_args);

    if self.dry {
      cmd = cmd.arg("--dry-run");
    }

    let result = execute_build(&cmd, self.no_nom, self.dry);

    if result.is_err()
      && let Some(ref attr) = suggest_attr
    {
      xi_core::suggest::print_suggestions_on_failure(&suggest_ref, attr, None);
    }

    result?;

    // Push to cache after successful build (best-effort)
    if let Some(ref out_link) = self.out_link
      && xi_core::cache::is_push_configured(&self.cache)
    {
      xi_core::cache::push_to_cache(&self.cache, out_link);
    }

    Ok(())
  }

  /// Build all flake outputs using the selected backend.
  fn run_build_all(self) -> Result<()> {
    let flake_ref = extract_flake_ref(&self.installable)?;

    if self.recursive {
      return self.run_build_all_recursive(&flake_ref);
    }

    let backend = resolve_backend(&self.backend, &CiBackend::Auto);

    if matches!(backend, CiBackend::NixFastBuild) {
      info!("Building all outputs with nix-fast-build");
      build_all_nix_fast_build(
        &flake_ref,
        &self.passthrough.to_nix_args(),
        &self.extra_args,
        self.no_nom,
        self.dry,
        false,
      )
    } else {
      info!("Building all outputs with devour-flake");
      build_all_for_flake_ref(
        &flake_ref,
        &self.passthrough.to_nix_args(),
        &self.extra_args,
        self.no_nom,
        self.dry,
      )
    }
  }

  /// Discover all subflakes recursively and build each with devour-flake.
  fn run_build_all_recursive(self, root_ref: &str) -> Result<()> {
    let local_dir = resolve_local_flake_dir(Some(root_ref));
    let Some(ref base_dir) = local_dir else {
      bail!(
        "--recursive requires a local flake reference, \
         got remote ref: {root_ref}"
      );
    };

    let subflake_dirs = discover_subflakes(base_dir)?;
    if subflake_dirs.is_empty() {
      bail!("No flake.nix files found under {}", base_dir.display());
    }

    info!(
      "Found {} flake(s) under {}",
      subflake_dirs.len(),
      base_dir.display()
    );

    let passthrough_args = self.passthrough.to_nix_args();
    let mut errors = Vec::new();

    for dir in &subflake_dirs {
      let flake_ref = if dir == Path::new(".") || dir == base_dir {
        root_ref.to_string()
      } else {
        let relative =
          dir.strip_prefix(base_dir).unwrap_or(dir).to_string_lossy();
        format!("path:{root_ref}?dir={relative}")
      };

      info!("Building subflake: {flake_ref}");

      if let Err(e) = build_all_for_flake_ref(
        &flake_ref,
        &passthrough_args,
        &self.extra_args,
        self.no_nom,
        self.dry,
      ) {
        errors.push((flake_ref, e));
      }
    }

    if errors.is_empty() {
      info!("All {} subflake(s) built successfully", subflake_dirs.len());
      Ok(())
    } else {
      for (ref_name, err) in &errors {
        tracing::error!("Failed to build {ref_name}: {err}");
      }
      bail!(
        "{} of {} subflake(s) failed to build",
        errors.len(),
        subflake_dirs.len()
      );
    }
  }
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

impl CheckArgs {
  /// Run the check command.
  ///
  /// - No target: `nix flake check .` (check everything)
  /// - Bare name like `xi`: `nix build .#checks.<system>.xi`
  /// - Path like `../other`: `nix flake check ../other`
  /// - `ref#attr`: `nix build ref#checks.<system>.attr`
  ///
  /// # Errors
  ///
  /// Returns an error if the check fails.
  pub fn run(self) -> Result<()> {
    let passthrough_args = self.passthrough.to_nix_args();

    let Some(ref target) = self.target else {
      // No target → check everything in current dir
      ensure_flake_locked(Some(PathBuf::from(".")))?;
      info!("Checking flake");

      let cmd = NixCommand::new(CommandKind::Flake)
        .print_build_logs(false)
        .arg("check")
        .args(&passthrough_args)
        .args(&self.extra_args);

      return execute_flake_check(&cmd, self.no_nom);
    };

    // Split on '#' to separate flake ref from attribute
    let (flake_ref, attr) = if let Some((r, a)) = target.split_once('#') {
      (r.to_string(), Some(a.to_string()))
    } else if is_flake_ref(target) {
      // Looks like a path/URL → treat as flake reference, no attribute
      (target.clone(), None)
    } else {
      // Bare name → treat as check attribute in current flake
      (".".to_string(), Some(target.clone()))
    };

    ensure_flake_locked(resolve_local_flake_dir(Some(&flake_ref)))?;

    if let Some(ref attr_name) = attr {
      // Build specific check: .#checks.<system>.<attr>
      let system = current_nix_system();
      let suggest_ref = flake_ref.clone();
      let check_installable = Installable::Flake {
        reference: flake_ref,
        attribute: vec![
          FlakeOutput::Checks.to_string(),
          system,
          attr_name.clone(),
        ],
      };

      let check_display = check_installable
        .to_args()
        .first()
        .cloned()
        .unwrap_or_default();
      info!("Checking \"{check_display}\"");

      let cmd = NixCommand::new(CommandKind::Build)
        .print_build_logs(false)
        .args(check_installable.to_args())
        .args(&passthrough_args)
        .args(&self.extra_args);

      let result = execute_build(&cmd, self.no_nom, false);
      if result.is_err() {
        xi_core::suggest::print_suggestions_on_failure(
          &suggest_ref,
          attr_name,
          Some(FlakeOutput::Checks.as_str()),
        );
      }
      result
    } else {
      // Check entire flake
      info!("Checking flake");

      let cmd = NixCommand::new(CommandKind::Flake)
        .print_build_logs(false)
        .arg("check")
        .arg(&flake_ref)
        .args(&passthrough_args)
        .args(&self.extra_args);

      execute_flake_check(&cmd, self.no_nom)
    }
  }
}

/// Returns true if the string looks like a flake reference (path or URL),
/// not a bare attribute name.
fn is_flake_ref(s: &str) -> bool {
  s.starts_with('.')
    || s.starts_with('/')
    || s.starts_with("path:")
    || s.starts_with("github:")
    || s.starts_with("git+")
    || s.starts_with("sourcehut:")
    || s.starts_with("gitlab:")
    || s.contains('/')
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

impl RunArgs {
  /// Run the run command.
  ///
  /// In normal mode: build the derivation with nom, then run interactively.
  /// In locate mode (`--locate`): search nixpkgs via nix-index, build, exec.
  ///
  /// # Errors
  ///
  /// Returns an error if the command fails.
  pub fn run(self) -> Result<()> {
    if self.locate {
      return self.run_locate();
    }

    ensure_flake_locked(resolve_local_flake_dir_from_installable(
      &self.installable,
    ))?;

    // Capture suggestion info before borrowing installable
    let suggest_ref = suggestion_flake_ref(&self.installable);
    let suggest_attr = suggestion_attr(&self.installable);

    let installable_args = installable_to_args(&self.installable);
    let passthrough_args = self.passthrough.to_nix_args();

    let target_display = installable_args.first().cloned().unwrap_or_default();

    if target_display.is_empty() {
      info!("Running flake app");
    } else {
      info!("Running flake app \"{target_display}\"");
    }

    // Use `nix run` directly — it resolves both apps and packages,
    // unlike `nix build` which only looks in packages/legacyPackages.
    // We can't pipe through nom here because the program's stdout/stdin
    // must pass through to the TTY.
    let mut cmd = NixCommand::new(CommandKind::Run)
      .args(&installable_args)
      .args(&passthrough_args);

    if !self.extra_args.is_empty() {
      cmd = cmd.arg("--").args(&self.extra_args);
    }

    let result = run_interactive(&cmd);
    if result.is_err()
      && let Some(ref attr) = suggest_attr
    {
      xi_core::suggest::print_suggestions_on_failure(&suggest_ref, attr, None);

      // Hint: suggest locate mode for bare names
      if !is_flake_ref(attr) {
        eprintln!(
          "\n{}",
          xi_core::style::dim(&format!(
            "hint: use `xi run -l {attr}` to search nixpkgs for a \
             package providing '{attr}'"
          ))
        );
      }
    }
    result
  }

  /// Run in locate mode: search nixpkgs via nix-index, build, exec.
  fn run_locate(self) -> Result<()> {
    // In locate mode, the installable positional arg is the command name.
    let command = match &self.installable {
      xi_core::installable::InstallableArgs::Specified(inst) => {
        match inst {
          xi_core::installable::Installable::Flake {
            reference,
            attribute,
          } => {
            if attribute.is_empty() {
              reference.clone()
            } else {
              // User passed something like "nixpkgs#cowsay" — use the
              // attribute as the command name
              attribute.last().cloned().unwrap_or_else(|| reference.clone())
            }
          },
          _ => {
            bail!(
              "locate mode expects a bare command name, not a file/expr/store \
               path"
            );
          },
        }
      },
      xi_core::installable::InstallableArgs::Unspecified => {
        bail!("locate mode requires a command name: xi run -l <command>");
      },
    };

    let cache_level = locate::CacheLevel::from_u8(self.cache_level.unwrap_or(2));
    let passthrough_args = self.passthrough.to_nix_args();

    locate::locate_and_run(
      &command,
      &self.extra_args,
      &passthrough_args,
      self.no_nom,
      cache_level,
      self.shell,
      self.install,
    )
  }
}

// ---------------------------------------------------------------------------
// fmt
// ---------------------------------------------------------------------------

impl FmtArgs {
  /// Run the fmt command.
  ///
  /// Resolves the formatter backend from CLI flag, .xi.toml, or
  /// auto-detection, then dispatches accordingly.
  ///
  /// # Errors
  ///
  /// Returns an error if formatting fails.
  pub fn run(self) -> Result<()> {
    let local_dir = resolve_local_flake_dir(self.flake_ref.as_deref());
    ensure_flake_locked(local_dir.clone())?;

    let passthrough_args = self.passthrough.to_nix_args();
    let flake_ref_owned =
      self.flake_ref.clone().unwrap_or_else(|| ".".to_string());
    let flake_ref_str = flake_ref_owned.as_str();

    let project_config =
      project_config::load_project_config(local_dir.as_deref());

    let backend = resolve_fmt_backend(
      &self.backend,
      &project_config.fmt.backend,
      flake_ref_str,
    );

    if backend.is_flake() {
      self.run_with_flake_formatter(flake_ref_str, &passthrough_args)
    } else {
      info!("Using {backend}");
      self.run_external_formatter(&backend.0, flake_ref_str)
    }
  }

  /// Run formatting via the flake's declared formatter.
  fn run_with_flake_formatter(
    self,
    flake_ref: &str,
    passthrough_args: &[String],
  ) -> Result<()> {
    // Phase 1: build the formatter with nom
    info!("Building formatter");

    let system = current_nix_system();

    let formatter_installable = Installable::Flake {
      reference: flake_ref.to_string(),
      attribute: vec![FlakeOutput::Formatter.to_string(), system],
    };

    let build_cmd = NixCommand::new(CommandKind::Build)
      .print_build_logs(false)
      .args(formatter_installable.to_args())
      .args(passthrough_args)
      .arg("--no-link");

    execute_build(&build_cmd, self.no_nom, false)?;

    // Phase 2: run fmt (instant, formatter is cached)
    info!("Formatting");

    let mut cmd = NixCommand::new(CommandKind::Fmt).args(passthrough_args);

    if let Some(ref flake_ref) = self.flake_ref {
      cmd = cmd.arg(flake_ref.as_str());
    }

    if !self.extra_args.is_empty() {
      cmd = cmd.arg("--").args(&self.extra_args);
    }

    run_interactive(&cmd)
  }

  /// Run an external formatter on discovered .nix files.
  fn run_external_formatter(
    self,
    command: &str,
    flake_ref: &str,
  ) -> Result<()> {
    let local_dir = resolve_local_flake_dir(Some(flake_ref));
    let dir = local_dir.as_deref().unwrap_or_else(|| Path::new("."));

    let nix_files = discover_nix_files(dir);

    if nix_files.is_empty() {
      info!("No .nix files found to format");
      return Ok(());
    }

    info!("Formatting {} .nix file(s) with {command}", nix_files.len());

    let mut cmd = std::process::Command::new(command);

    for arg in &self.extra_args {
      cmd.arg(arg);
    }

    for file in &nix_files {
      cmd.arg(file);
    }

    let status = cmd
      .stdin(std::process::Stdio::inherit())
      .stdout(std::process::Stdio::inherit())
      .stderr(std::process::Stdio::inherit())
      .status()
      .map_err(|e| {
        color_eyre::eyre::eyre!(
          "Failed to run {command} (is it installed?): {e}"
        )
      })?;

    if !status.success() {
      bail!("{command} exited with status {status}");
    }

    Ok(())
  }

}

/// Discover .nix files in a directory, skipping common build/cache dirs.
fn discover_nix_files(dir: &Path) -> Vec<PathBuf> {
  WalkDir::new(dir)
    .follow_links(false)
    .into_iter()
    .filter_entry(|e| {
      let name = e.file_name().to_string_lossy();
      !matches!(
        name.as_ref(),
        ".git" | ".direnv" | ".xi" | "node_modules" | "result" | ".devenv"
      )
    })
    .filter_map(std::result::Result::ok)
    .filter(|e| {
      e.file_type().is_file()
        && e.path().extension().is_some_and(|ext| ext == "nix")
    })
    .map(walkdir::DirEntry::into_path)
    .collect()
}

/// Check if the flake declares a formatter output for the current system.
fn has_flake_formatter(flake_ref: &str) -> bool {
  let system = current_nix_system();
  let output = NixCommand::new(CommandKind::Eval)
    .arg(format!("{flake_ref}#formatter.{system}"))
    .arg("--apply")
    .arg("_: true")
    .arg("--json")
    .output();

  match output {
    Ok(o) => o.status.success(),
    Err(_) => false,
  }
}

/// Resolve the effective fmt backend from CLI flag, config, and auto-detection.
///
/// Priority: CLI `--backend` > `.xi.toml` `[fmt] backend` > auto-detect.
fn resolve_fmt_backend(
  cli: &FmtBackend,
  config: &FmtBackend,
  flake_ref: &str,
) -> FmtBackend {
  if !cli.is_auto() {
    return cli.clone();
  }
  if !config.is_auto() {
    return config.clone();
  }
  // Auto-detect: flake formatter if present, else nixfmt
  if has_flake_formatter(flake_ref) {
    FmtBackend(FmtBackend::FLAKE.to_string())
  } else {
    FmtBackend("nixfmt".to_string())
  }
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

impl ShowArgs {
  /// Run the show command.
  ///
  /// # Errors
  ///
  /// Returns an error if the command fails.
  pub fn run(self) -> Result<()> {
    ensure_flake_locked(resolve_local_flake_dir(self.flake_ref.as_deref()))?;

    // Raw mode: pass through to nix flake show directly
    if self.raw {
      info!("Showing flake outputs (raw)");

      let mut cmd = NixCommand::new(CommandKind::Flake).arg("show");

      if self.json {
        cmd = cmd.arg("--json");
      }

      if self.show_trace {
        cmd = cmd.arg("--show-trace");
      }

      if let Some(ref flake_ref) = self.flake_ref {
        cmd = cmd.arg(flake_ref.as_str());
      }

      cmd = cmd.args(&self.extra_args);

      return run_interactive(&cmd);
    }

    // Compact mode: fetch JSON and render nicely
    if !self.json {
      info!("Showing flake outputs");
    }

    let mut cmd = NixCommand::new(CommandKind::Flake)
      .arg("show")
      .arg("--json");

    if self.show_trace {
      cmd = cmd.arg("--show-trace");
    }

    if let Some(ref flake_ref) = self.flake_ref {
      cmd = cmd.arg(flake_ref.as_str());
    }

    cmd = cmd.args(&self.extra_args);

    debug!(argv = ?cmd.argv());

    let output = cmd.output().map_err(|e| {
      color_eyre::eyre::eyre!("nix flake show --json failed: {e}")
    })?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      bail!("nix flake show failed:\n{stderr}");
    }

    if self.json {
      print!("{}", String::from_utf8_lossy(&output.stdout));
      return Ok(());
    }

    let mut json: serde_json::Value = serde_json::from_slice(&output.stdout)
      .map_err(|e| {
        color_eyre::eyre::eyre!("Failed to parse flake show output: {e}")
      })?;

    // Discover children for leaf categories (type=unknown) using nix eval
    let leaf_cats = show::leaf_categories_to_discover(&json);
    if !leaf_cats.is_empty() {
      let flake_ref_str = self.flake_ref.as_deref().unwrap_or(".");
      let discovered = discover_leaf_categories(flake_ref_str, &leaf_cats);
      show::enrich_flake_json(&mut json, &discovered);
    }

    show::render_flake_outputs(&json, self.all);
    Ok(())
  }
}

// ---------------------------------------------------------------------------
// develop
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

impl InitArgs {
  /// Run the init command.
  ///
  /// # Errors
  ///
  /// Returns an error if initialization fails.
  pub fn run(self) -> Result<()> {
    info!("Initializing flake");

    let mut cmd = NixCommand::new(CommandKind::Flake).arg("init");

    if let Some(ref template) = self.template {
      cmd = cmd.arg("--template").arg(template.as_str());
    }

    cmd = cmd.args(&self.extra_args);

    run_interactive(&cmd)
  }
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

impl UpdateArgs {
  /// Run the update command.
  ///
  /// - No inputs: `nix flake update` (updates all)
  /// - With inputs: `nix flake update <input1> <input2> ...`
  ///
  /// # Errors
  ///
  /// Returns an error if the update fails.
  pub fn run(self) -> Result<()> {
    let flake_ref = self.flake.as_deref().unwrap_or(".");

    // No ensure_flake_locked here — update creates/updates the lock itself

    if self.inputs.is_empty() {
      info!("Updating all flake inputs");
    } else {
      info!("Updating inputs: {}", self.inputs.join(", "));
    }

    let mut cmd = NixCommand::new(CommandKind::Flake).arg("update");

    // nix flake update takes input names as positional args
    for input in &self.inputs {
      cmd = cmd.arg(input.as_str());
    }

    // Flake ref via --flake flag (nix >= 2.19)
    cmd = cmd.arg("--flake").arg(flake_ref);

    if self.commit_lock_file {
      cmd = cmd.arg("--commit-lock-file");
    }

    // Forward curated nix flags (--refresh, --option, etc.)
    cmd = cmd.args(self.passthrough.to_nix_args());

    cmd = cmd.args(&self.extra_args);

    run_interactive(&cmd)
  }
}

// ---------------------------------------------------------------------------
// Flake directory & lock helpers
// ---------------------------------------------------------------------------

/// Resolve the local flake directory from an optional flake reference.
///
/// Returns `Some(path)` for local references (`.`, `./foo`, `/abs/path`,
/// `path:...`), `None` for remote references (`github:...`, `nixpkgs`, etc.)
/// or when no reference is given (defaults to current directory).
pub(crate) fn resolve_local_flake_dir(
  flake_ref: Option<&str>,
) -> Option<PathBuf> {
  let Some(reference) = flake_ref else {
    return Some(PathBuf::from("."));
  };

  if let Some(path) = reference.strip_prefix("path:") {
    let path = path.split_once('?').map_or(path, |(p, _)| p);
    return Some(PathBuf::from(path));
  }

  let path = Path::new(reference);

  if path.is_absolute()
    || matches!(reference, "." | "..")
    || reference.starts_with("./")
    || reference.starts_with("../")
  {
    return Some(path.to_path_buf());
  }

  None
}

/// Resolve the local flake directory from an installable args reference.
///
/// Bare names (not paths/URLs) resolve to `.` (current directory) since
/// they're treated as attributes of the local flake.
fn resolve_local_flake_dir_from_installable(
  installable: &xi_core::installable::InstallableArgs,
) -> Option<PathBuf> {
  match installable {
    xi_core::installable::InstallableArgs::Unspecified => {
      Some(PathBuf::from("."))
    },
    xi_core::installable::InstallableArgs::Specified(inst) => {
      match inst {
        xi_core::installable::Installable::Flake { reference, .. } => {
          if is_flake_ref(reference) {
            resolve_local_flake_dir(Some(reference.as_str()))
          } else {
            // Bare name like "xi" → local flake attribute
            Some(PathBuf::from("."))
          }
        },
        xi_core::installable::Installable::File { path, .. } => {
          path.parent().map(Path::to_path_buf)
        },
        _ => None,
      }
    },
  }
}

/// If the flake directory is local and `flake.lock` is missing, run
/// `nix flake lock` to generate it automatically (like cargo/mise do).
pub(crate) fn ensure_flake_locked(flake_dir: Option<PathBuf>) -> Result<()> {
  let Some(dir) = flake_dir else {
    return Ok(());
  };

  let lock_path = dir.join("flake.lock");
  let flake_path = dir.join("flake.nix");

  if !flake_path.exists() || lock_path.exists() {
    return Ok(());
  }

  info!(
    "flake.lock not found in {}, running nix flake lock",
    dir.display()
  );

  let cmd = NixCommand::new(CommandKind::Flake)
    .arg("lock")
    .arg(dir.to_string_lossy().as_ref());

  let status = cmd
    .run_with_logs()
    .map_err(|e| color_eyre::eyre::eyre!("nix flake lock failed: {e}"))?;

  if !status.success() {
    bail!("nix flake lock exited with status {status}");
  }

  info!("flake.lock created successfully");
  Ok(())
}

// ---------------------------------------------------------------------------
// Nom & streaming execution
// ---------------------------------------------------------------------------

/// Execute `nix flake check`, optionally piped through nix-output-monitor.
///
/// When nom is enabled, the build output goes through nom for pretty progress.
/// When nom is disabled, stderr is filtered to suppress the noisy
/// "incompatible systems" warning that nix emits.
fn execute_flake_check(cmd: &NixCommand, no_nom: bool) -> Result<()> {
  use std::io::{BufRead, BufReader, Write};
  use std::process::Stdio;

  if !no_nom {
    return run_with_nom(cmd.to_exec());
  }

  let mut child = cmd
    .to_std_command()
    .stdout(Stdio::inherit())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| color_eyre::eyre::eyre!("Failed to start nix: {e}"))?;

  let stderr = child
    .stderr
    .take()
    .ok_or_else(|| color_eyre::eyre::eyre!("Failed to capture stderr"))?;

  for line in BufReader::new(stderr).lines() {
    let Ok(line) = line else { continue };
    if should_filter_check_line(&line) {
      continue;
    }
    let _ = writeln!(std::io::stderr(), "{line}");
  }

  let status = child.wait()?;
  if !status.success() {
    bail!("nix flake check exited with status {status}");
  }

  Ok(())
}

/// Returns true if a stderr/output line from `nix flake check` should be
/// filtered out (noise that doesn't help the user).
fn should_filter_check_line(line: &str) -> bool {
  let trimmed = line.trim();
  // "warning: The check omitted these incompatible systems: ..."
  trimmed.starts_with("warning: The check omitted these incompatible systems")
    // "Use '--all-systems' to check all."
    || trimmed == "Use '--all-systems' to check all."
}

/// Run a build command piped through nix-output-monitor.
pub(crate) fn run_with_nom(base_command: Exec) -> Result<()> {
  let pipeline = {
    base_command
      .args(["--log-format", "internal-json", "--verbose"])
      .stderr(Redirection::Merge)
      .stdout(Redirection::Pipe)
      | Exec::cmd("nom").args(["--json"])
  }
  .stdout(Redirection::None);

  debug!(?pipeline);

  let job = pipeline.start()?;

  for proc in &job.processes {
    proc.wait()?;
  }

  if let Some(nix_proc) = job.processes.first() {
    let exit_status = nix_proc.wait()?;
    if !exit_status.success() {
      bail!(ExitError::new(exit_status));
    }
  }

  Ok(())
}

/// Run an exec command streaming output directly.
pub(crate) fn run_exec_streaming(base_command: Exec) -> Result<()> {
  let cmd = base_command
    .stderr(Redirection::Merge)
    .stdout(Redirection::None);

  debug!(?cmd);

  let exit_status = cmd.join()?;
  if !exit_status.success() {
    bail!(ExitError::new(exit_status));
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// Leaf category discovery (nix eval)
// ---------------------------------------------------------------------------

/// Discover children of leaf flake output categories using `nix eval`.
///
/// For each category name, runs `nix eval <flake>#<cat> --apply <expr> --json`
/// to recursively discover attribute names. Returns a list of
/// `(category_name, discovered_tree)` pairs.
fn discover_leaf_categories(
  flake_ref: &str,
  categories: &[String],
) -> Vec<(String, serde_json::Value)> {
  categories
    .iter()
    .filter_map(|cat_name| {
      let tree = discover_single_category(flake_ref, cat_name)?;
      Some((cat_name.clone(), tree))
    })
    .collect()
}

/// Discover children of a single leaf category using `nix eval`.
fn discover_single_category(
  flake_ref: &str,
  cat_name: &str,
) -> Option<serde_json::Value> {
  let attr = format!("{flake_ref}#{cat_name}");

  debug!(attr, "Discovering leaf category children");

  let cmd = NixCommand::new(CommandKind::Eval)
    .arg(&attr)
    .arg("--apply")
    .arg(show::DISCOVER_ATTRS_NIX)
    .arg("--json");

  let output = cmd.output().ok()?;
  if !output.status.success() {
    debug!(
      attr,
      stderr = %String::from_utf8_lossy(&output.stderr),
      "nix eval failed for leaf category, skipping"
    );
    return None;
  }

  let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

  // null means the category itself is a leaf (function/derivation)
  if value.is_null() {
    return None;
  }

  Some(value)
}

// ---------------------------------------------------------------------------
// devour-flake helpers
// ---------------------------------------------------------------------------

/// Extract the flake reference from an installable, warning if an attribute
/// path is present (ignored with `--all`).
fn extract_flake_ref(
  installable: &xi_core::installable::InstallableArgs,
) -> Result<String> {
  match installable {
    xi_core::installable::InstallableArgs::Specified(inst) => match inst {
      xi_core::installable::Installable::Flake {
        reference,
        attribute,
      } => {
        if !attribute.is_empty() {
          warn!("Ignoring attribute path with --all (building all outputs)");
        }
        Ok(reference.clone())
      },
      _ => bail!("--all requires a flake reference"),
    },
    xi_core::installable::InstallableArgs::Unspecified => Ok(".".to_string()),
  }
}

/// Build all outputs of a single flake ref using devour-flake.
pub(crate) fn build_all_for_flake_ref(
  flake_ref: &str,
  passthrough_args: &[String],
  extra_args: &[String],
  no_nom: bool,
  dry: bool,
) -> Result<()> {
  let local_dir = resolve_local_flake_dir(Some(flake_ref));
  ensure_flake_locked(local_dir.clone())?;

  if local_dir.is_some() {
    verify_flake_lock_in_sync(flake_ref)?;
  }

  let devour_installable =
    format!("github:srid/devour-flake/{DEVOUR_FLAKE_REV}#default");

  let extra_args = transform_override_inputs(extra_args);

  let mut cmd = NixCommand::new(CommandKind::Build)
    .print_build_logs(false)
    .arg(&devour_installable)
    .arg("--override-input")
    .arg("flake")
    .arg(flake_ref)
    .arg("--no-link")
    .args(passthrough_args)
    .args(&extra_args);

  if dry {
    cmd = cmd.arg("--dry-run");
  }

  execute_build(&cmd, no_nom, dry)
}

/// Discover all directories containing `flake.nix` under `base_dir`.
pub(crate) fn discover_subflakes(base_dir: &Path) -> Result<Vec<PathBuf>> {
  let mut dirs = Vec::new();

  for entry in WalkDir::new(base_dir)
    .follow_links(false)
    .into_iter()
    .filter_entry(|e| {
      let name = e.file_name().to_string_lossy();
      !matches!(
        name.as_ref(),
        ".git" | ".direnv" | ".xi" | "node_modules" | "result"
      )
    })
  {
    let entry = entry?;
    if entry.file_type().is_file()
      && entry.file_name() == "flake.nix"
      && let Some(parent) = entry.path().parent()
    {
      dirs.push(parent.to_path_buf());
    }
  }

  dirs.sort();
  Ok(dirs)
}

/// Verify that `flake.lock` is in sync with `flake.nix` inputs.
pub(crate) fn verify_flake_lock_in_sync(flake_ref: &str) -> Result<()> {
  debug!("Verifying flake.lock is in sync");

  let cmd = NixCommand::new(CommandKind::Flake)
    .arg("lock")
    .arg("--no-update-lock-file")
    .arg(flake_ref);

  let status = cmd.run_with_logs().map_err(|e| {
    color_eyre::eyre::eyre!("nix flake lock --no-update-lock-file failed: {e}")
  })?;

  if !status.success() {
    bail!(
      "flake.lock is out of sync with flake.nix inputs. \
       Run `nix flake lock` to update it."
    );
  }

  Ok(())
}

/// Transform `--override-input` arguments to use `flake/` prefix.
pub(crate) fn transform_override_inputs(args: &[String]) -> Vec<String> {
  let mut result = args.to_vec();
  let mut i = 0;
  while i < result.len() {
    if result[i] == "--override-input" {
      if let Some(name) = result.get_mut(i + 1) {
        *name = format!("flake/{name}");
      }
      i += 3;
    } else {
      i += 1;
    }
  }
  result
}

// ---------------------------------------------------------------------------
// nix-fast-build backend
// ---------------------------------------------------------------------------

/// Check whether `nix-fast-build` is available in `$PATH`.
pub(crate) fn detect_nix_fast_build() -> bool {
  which::which("nix-fast-build").is_ok()
}

/// Resolve the effective build backend from CLI flag and config.
///
/// Priority: CLI `--backend` > `.xi.toml` `[ci] backend` > auto-detect.
pub(crate) fn resolve_backend(
  cli: &CiBackend,
  config: &CiBackend,
) -> CiBackend {
  match cli {
    CiBackend::DevourFlake | CiBackend::NixFastBuild => cli.clone(),
    CiBackend::Auto => match config {
      CiBackend::DevourFlake | CiBackend::NixFastBuild => config.clone(),
      CiBackend::Auto => {
        if detect_nix_fast_build() {
          CiBackend::NixFastBuild
        } else {
          CiBackend::DevourFlake
        }
      },
    },
  }
}

/// Build all outputs of a single flake ref using `nix-fast-build`.
///
/// Unlike the devour-flake path, `nix-fast-build`:
/// - Evaluates your flake directly (no `flake/` prefix needed for overrides)
/// - Has built-in nom support (no external pipe needed)
/// - Can skip already-cached derivations (`--skip-cached`)
/// - Provides failure isolation per-output
pub(crate) fn build_all_nix_fast_build(
  flake_ref: &str,
  passthrough_args: &[String],
  extra_args: &[String],
  no_nom: bool,
  dry: bool,
  no_ifd: bool,
) -> Result<()> {
  let local_dir = resolve_local_flake_dir(Some(flake_ref));
  ensure_flake_locked(local_dir.clone())?;

  if local_dir.is_some() {
    verify_flake_lock_in_sync(flake_ref)?;
  }

  let mut cmd = std::process::Command::new("nix-fast-build");
  cmd.arg("--flake").arg(flake_ref);
  cmd.arg("--skip-cached");
  cmd.arg("--no-link");

  if no_nom {
    cmd.arg("--no-nom");
  }

  if dry {
    cmd.arg("--eval-only");
  }

  if no_ifd {
    cmd
      .arg("--option")
      .arg("allow-import-from-derivation")
      .arg("false");
  }

  // Passthrough args go directly — no flake/ prefix transformation needed
  for arg in passthrough_args {
    cmd.arg(arg);
  }
  for arg in extra_args {
    cmd.arg(arg);
  }

  debug!(cmd = ?cmd, "running nix-fast-build");

  let status = cmd
    .stdin(std::process::Stdio::inherit())
    .stdout(std::process::Stdio::inherit())
    .stderr(std::process::Stdio::inherit())
    .status()
    .map_err(|e| {
      color_eyre::eyre::eyre!(
        "Failed to run nix-fast-build (is it installed?): {e}"
      )
    })?;

  if !status.success() {
    bail!("nix-fast-build exited with status {status}");
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn resolve_none_defaults_to_current_dir() {
    assert_eq!(resolve_local_flake_dir(None), Some(PathBuf::from(".")));
  }

  #[test]
  fn resolve_dot_returns_current_dir() {
    assert_eq!(resolve_local_flake_dir(Some(".")), Some(PathBuf::from(".")));
  }

  #[test]
  fn resolve_dotdot_returns_parent() {
    assert_eq!(
      resolve_local_flake_dir(Some("..")),
      Some(PathBuf::from(".."))
    );
  }

  #[test]
  fn resolve_relative_path() {
    assert_eq!(
      resolve_local_flake_dir(Some("./my-flake")),
      Some(PathBuf::from("./my-flake"))
    );
  }

  #[test]
  fn resolve_parent_relative_path() {
    assert_eq!(
      resolve_local_flake_dir(Some("../other")),
      Some(PathBuf::from("../other"))
    );
  }

  #[test]
  fn resolve_absolute_path() {
    assert_eq!(
      resolve_local_flake_dir(Some("/home/user/flake")),
      Some(PathBuf::from("/home/user/flake"))
    );
  }

  #[test]
  fn resolve_path_scheme() {
    assert_eq!(
      resolve_local_flake_dir(Some("path:/home/user/flake")),
      Some(PathBuf::from("/home/user/flake"))
    );
  }

  #[test]
  fn resolve_path_scheme_strips_query() {
    assert_eq!(
      resolve_local_flake_dir(Some("path:./foo?dir=bar")),
      Some(PathBuf::from("./foo"))
    );
  }

  #[test]
  fn resolve_github_returns_none() {
    assert_eq!(resolve_local_flake_dir(Some("github:NixOS/nixpkgs")), None);
  }

  #[test]
  fn resolve_nixpkgs_returns_none() {
    assert_eq!(resolve_local_flake_dir(Some("nixpkgs")), None);
  }

  #[test]
  fn resolve_registry_with_attr_returns_none() {
    assert_eq!(resolve_local_flake_dir(Some("nixpkgs#hello")), None);
  }

  #[test]
  fn transform_override_inputs_prefixes_flake() {
    let args = vec![
      "--override-input".to_string(),
      "nixpkgs".to_string(),
      "github:NixOS/nixpkgs/unstable".to_string(),
    ];
    assert_eq!(
      transform_override_inputs(&args),
      vec![
        "--override-input",
        "flake/nixpkgs",
        "github:NixOS/nixpkgs/unstable",
      ]
    );
  }

  #[test]
  fn transform_override_inputs_multiple() {
    let args = vec![
      "--override-input".to_string(),
      "nixpkgs".to_string(),
      "github:NixOS/nixpkgs/unstable".to_string(),
      "--override-input".to_string(),
      "crane".to_string(),
      "github:ipetkov/crane".to_string(),
    ];
    assert_eq!(
      transform_override_inputs(&args),
      vec![
        "--override-input",
        "flake/nixpkgs",
        "github:NixOS/nixpkgs/unstable",
        "--override-input",
        "flake/crane",
        "github:ipetkov/crane",
      ]
    );
  }

  #[test]
  fn transform_override_inputs_preserves_other_args() {
    let args = vec![
      "--verbose".to_string(),
      "--override-input".to_string(),
      "nixpkgs".to_string(),
      "github:NixOS/nixpkgs".to_string(),
      "--keep-going".to_string(),
    ];
    assert_eq!(
      transform_override_inputs(&args),
      vec![
        "--verbose",
        "--override-input",
        "flake/nixpkgs",
        "github:NixOS/nixpkgs",
        "--keep-going",
      ]
    );
  }

  #[test]
  fn transform_override_inputs_empty() {
    let args: Vec<String> = vec![];
    assert_eq!(transform_override_inputs(&args), Vec::<String>::new());
  }

  #[test]
  fn current_system_is_valid() {
    let system = current_nix_system();
    assert!(system.contains('-'), "System should be arch-os: {system}");
    // Should be something like x86_64-linux or aarch64-darwin
    let parts: Vec<&str> = system.split('-').collect();
    assert_eq!(parts.len(), 2);
    assert!(!parts[0].is_empty());
    assert!(!parts[1].is_empty());
  }

  #[test]
  fn discover_subflakes_finds_nested_flakes() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = dir.path();

    std::fs::write(root.join("flake.nix"), "{}").expect("write root");

    std::fs::create_dir_all(root.join("backend")).expect("create backend");
    std::fs::write(root.join("backend/flake.nix"), "{}")
      .expect("write backend");

    std::fs::create_dir_all(root.join("frontend")).expect("create frontend");
    std::fs::write(root.join("frontend/flake.nix"), "{}")
      .expect("write frontend");

    std::fs::create_dir_all(root.join(".git")).expect("create .git");
    std::fs::write(root.join(".git/flake.nix"), "{}").expect("write .git");
    std::fs::create_dir_all(root.join("node_modules/pkg"))
      .expect("create node_modules");
    std::fs::write(root.join("node_modules/pkg/flake.nix"), "{}")
      .expect("write node_modules");

    let found = discover_subflakes(root).expect("discover");
    assert_eq!(found.len(), 3);
    assert!(found.contains(&root.to_path_buf()));
    assert!(found.contains(&root.join("backend")));
    assert!(found.contains(&root.join("frontend")));
  }

  #[test]
  fn discover_subflakes_empty_dir() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let found = discover_subflakes(dir.path()).expect("discover");
    assert!(found.is_empty());
  }
}
