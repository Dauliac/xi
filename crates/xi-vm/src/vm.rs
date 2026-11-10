//! Top-level `xi vm` dispatch, wired from `crates/xi/src/interface.rs`.

use std::path::PathBuf;

use color_eyre::Result;
use serde::Serialize;
use xi_core::style::{self, Icon};

use crate::{
  args::{
    VmArgs,
    VmBuildArgs,
    VmCheckArgs,
    VmConnectArgs,
    VmStartArgs,
    VmStatusArgs,
    VmStopArgs,
    VmSubcommand,
  },
  build::{self, BuildVariant},
  manifest::{self, Manifest},
  runtime,
};

/// `$XDG_STATE_HOME/xi`, the same state dir `xi-sources` uses, scoped
/// under a `vm/` subdirectory by [`crate::registry`].
#[must_use]
pub fn state_dir() -> PathBuf {
  xi_core::dirs::xdg_state_dir()
}

/// Entry point for `XiCommand::Vm`.
///
/// # Errors
///
/// Returns an error from whichever subcommand ran; see each of
/// [`build`], [`runtime`] and [`manifest`] for specifics.
pub fn run(args: VmArgs) -> Result<()> {
  let manifest = manifest::fetch(&args.flake)?;
  let state_dir = state_dir();

  match args.subcommand {
    VmSubcommand::List => list(&manifest, &state_dir),
    VmSubcommand::Build(sub) => run_build(&args.flake, &manifest, &sub),
    VmSubcommand::Check(sub) => run_check(&args.flake, &manifest, sub),
    VmSubcommand::Start(sub) => {
      run_start(&args.flake, &manifest, &state_dir, sub)
    },
    VmSubcommand::Stop(sub) => run_stop(&manifest, &state_dir, &sub),
    VmSubcommand::Connect(sub) => run_connect(&manifest, &state_dir, &sub),
    VmSubcommand::Status(sub) => run_status(&manifest, &state_dir, &sub),
  }
}

#[derive(Serialize)]
struct ListEntry {
  name:    String,
  kind:    &'static str,
  running: &'static str,
}

fn list(manifest: &Manifest, state_dir: &std::path::Path) -> Result<()> {
  let mut rows = Vec::with_capacity(manifest.len());
  for (name, entry) in manifest {
    let kind = match entry.kind {
      manifest::VmKind::Nixos => "nixos",
      manifest::VmKind::Distro => "distro",
    };
    let running = if entry.kind == manifest::VmKind::Nixos {
      if runtime::status(manifest, state_dir, name)?.running {
        "running"
      } else {
        "stopped"
      }
    } else {
      "unknown"
    };
    rows.push(ListEntry {
      name: name.clone(),
      kind,
      running,
    });
  }

  if rows.is_empty() {
    println!(
      "{}",
      style::dim("No VMs declared. See `xi.vm.<name>` in docs/guides/vm.md.")
    );
    return Ok(());
  }

  println!(
    "{} {:<10} {:<10}",
    style::bold(&format!("{:<20}", "NAME")),
    "KIND",
    "STATUS"
  );
  for row in rows {
    let status_icon = match row.running {
      "running" => Icon::Success,
      "stopped" => Icon::Info,
      _ => Icon::Warn,
    };
    println!(
      "{:<20} {:<10} {} {}",
      row.name,
      row.kind,
      status_icon.render(),
      row.running
    );
  }
  Ok(())
}

/// Resolve `--all`/`<name>` into the set of VM names to act on.
///
/// `name` is returned verbatim even if unknown to `manifest` - the
/// downstream build/runtime call is what reports "no VM named '...'",
/// so the error always comes from one place.
fn require_name<'a>(
  manifest: &'a Manifest,
  name: Option<&'a str>,
  all: bool,
) -> Result<Vec<&'a str>> {
  if all {
    return Ok(manifest.keys().map(String::as_str).collect());
  }
  let name =
    name.ok_or_else(|| color_eyre::eyre::eyre!("pass a VM name, or --all"))?;
  Ok(vec![name])
}

fn run_build(
  flakeref: &str,
  manifest: &Manifest,
  args: &VmBuildArgs,
) -> Result<()> {
  let names = require_name(manifest, args.name.as_deref(), args.all)?;
  let variant = if args.test {
    BuildVariant::Test
  } else {
    BuildVariant::Dev
  };
  for name in names {
    let entry = manifest.get(name);
    let path = match entry.map(|e| e.kind) {
      Some(manifest::VmKind::Distro) => {
        build::build_distro(flakeref, manifest, name)?
      },
      _ => {
        build::build_one(
          flakeref,
          manifest,
          name,
          variant,
          args.with_bootloader,
        )?
      },
    };
    println!("{}", path.display());
  }
  Ok(())
}

fn run_check(
  flakeref: &str,
  manifest: &Manifest,
  args: VmCheckArgs,
) -> Result<()> {
  let outcomes = if args.all {
    build::check_all(flakeref, manifest, args.build_only)?
  } else {
    let name = args
      .name
      .ok_or_else(|| color_eyre::eyre::eyre!("pass a VM name, or --all"))?;
    vec![build::check_one(
      flakeref,
      manifest,
      &name,
      args.build_only,
    )?]
  };

  if args.json {
    let envelope = serde_json::json!({
      "schema_version": 1,
      "status": if outcomes.iter().all(|o| o.status == "ok") { "ok" } else { "failed" },
      "results": outcomes,
    });
    println!("{}", serde_json::to_string_pretty(&envelope)?);
  } else {
    for outcome in &outcomes {
      match outcome.reason.as_deref() {
        None => {
          println!("{} {}: ok", Icon::Success.render(), outcome.name);
        },
        Some(reason) => {
          println!(
            "{} {}: failed ({reason}){}",
            Icon::Error.render(),
            outcome.name,
            outcome
              .detail
              .as_deref()
              .map(|d| format!("\n  {}", style::dim(d)))
              .unwrap_or_default()
          )
        },
      }
    }
  }

  if outcomes.iter().any(|o| o.status != "ok") {
    color_eyre::eyre::bail!("one or more VM checks failed");
  }
  Ok(())
}

fn run_start(
  flakeref: &str,
  manifest: &Manifest,
  state_dir: &std::path::Path,
  args: VmStartArgs,
) -> Result<()> {
  if args.all {
    let headless_default_for_all = !args.graphical;
    for (name, entry) in manifest {
      if entry.kind != manifest::VmKind::Nixos {
        println!(
          "{name}: skipped (kind = \"distro\" has no --background instance to \
           start under --all)"
        );
        continue;
      }
      runtime::start(
        flakeref,
        manifest,
        state_dir,
        name,
        args.graphical,
        headless_default_for_all,
        true,
        args.allow_port_fallback,
        args.with_bootloader,
      )?;
    }
    return Ok(());
  }

  let name = args
    .name
    .ok_or_else(|| color_eyre::eyre::eyre!("pass a VM name, or --all"))?;
  runtime::start(
    flakeref,
    manifest,
    state_dir,
    &name,
    args.graphical,
    args.headless,
    args.background,
    args.allow_port_fallback,
    args.with_bootloader,
  )
}

fn run_stop(
  manifest: &Manifest,
  state_dir: &std::path::Path,
  args: &VmStopArgs,
) -> Result<()> {
  let names = require_name(manifest, args.name.as_deref(), args.all)?;
  for name in names {
    runtime::stop(manifest, state_dir, name)?;
  }
  Ok(())
}

fn run_connect(
  manifest: &Manifest,
  state_dir: &std::path::Path,
  args: &VmConnectArgs,
) -> Result<()> {
  runtime::connect(manifest, state_dir, &args.name, args.console)
}

fn run_status(
  manifest: &Manifest,
  state_dir: &std::path::Path,
  args: &VmStatusArgs,
) -> Result<()> {
  let names = require_name(manifest, args.name.as_deref(), args.all)?;
  let statuses: Vec<_> = names
    .into_iter()
    .map(|name| runtime::status(manifest, state_dir, name))
    .collect::<Result<_>>()?;

  if args.json {
    let envelope = serde_json::json!({ "schema_version": 1, "vms": statuses });
    println!("{}", serde_json::to_string_pretty(&envelope)?);
  } else {
    for status in &statuses {
      let (icon, running) = if status.running {
        (Icon::Success, "running")
      } else {
        (Icon::Info, "stopped")
      };
      println!("{} {}: {running}", icon.render(), status.name);
    }
  }
  Ok(())
}
