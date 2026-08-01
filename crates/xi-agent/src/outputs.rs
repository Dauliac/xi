use std::{
  process::{Command, Stdio},
  time::Instant,
};

use color_eyre::eyre::Result;
use nix_command::find_real_nix_binary;
use serde_json::Value;
use xi_core::flake_output::{FlakeOutput, FlakeOutputKind, current_nix_system};

use crate::{
  args::OutputsArgs,
  schema::{Diagnostic, Envelope, OutputEntry, OutputsPayload, elapsed_ms, emit},
};

pub const CMD: &str = "outputs";

pub fn run(args: OutputsArgs) -> Result<()> {
  let start = Instant::now();
  let system = args.system.clone().unwrap_or_else(current_nix_system);
  match collect(&args, &system) {
    Ok(payload) => emit(&Envelope::ok(CMD, payload, elapsed_ms(start)))?,
    Err(diag) => {
      let payload = OutputsPayload {
        system:  system.clone(),
        outputs: Vec::new(),
        hidden:  Vec::new(),
      };
      emit(&Envelope::partial(
        CMD,
        payload,
        elapsed_ms(start),
        vec![diag],
      ))?;
    },
  }
  Ok(())
}

pub fn collect(
  args: &OutputsArgs,
  system: &str,
) -> Result<OutputsPayload, Diagnostic> {
  let mut cmd = Command::new(find_real_nix_binary());
  cmd.args([
    "--extra-experimental-features",
    "nix-command flakes",
    "flake",
    "show",
    "--json",
  ]);
  if args.all_systems {
    cmd.arg("--all-systems");
  }
  cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
  let output = cmd.output().map_err(|e| {
    Diagnostic::new("flake.eval.spawn-failed", e.to_string())
      .with_source("nix flake show")
  })?;
  if !output.status.success() {
    return Err(
      Diagnostic::new(
        "flake.eval.failed",
        String::from_utf8_lossy(&output.stderr).into_owned(),
      )
      .with_source("nix flake show"),
    );
  }
  let root: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
    Diagnostic::new("flake.eval.parse-failed", e.to_string())
      .with_source("nix flake show")
  })?;

  let mut outputs: Vec<OutputEntry> = Vec::new();
  let mut hidden: Vec<String> = Vec::new();

  let Some(root_map) = root.as_object() else {
    return Ok(OutputsPayload {
      system:  system.to_owned(),
      outputs,
      hidden,
    });
  };

  for (top_name, top_val) in root_map {
    let category = FlakeOutput::from_nix_name(top_name);
    let category_str = category.map_or(top_name.clone(), |o| o.as_str().to_owned());

    if !args.include_hidden
      && xi_core::flake_output::is_hidden_by_default(top_name)
    {
      hidden.push(category_str);
      continue;
    }

    if category.is_some_and(FlakeOutput::is_per_system) {
      // {system}/{attr}
      let Some(per_system) = top_val.as_object() else {
        continue;
      };
      let Some(entries) = per_system.get(system).and_then(Value::as_object) else {
        continue;
      };
      for (attr, val) in entries {
        outputs.push(entry_from(&category_str, top_name, system, attr, val));
      }
    } else {
      let Some(attrs) = top_val.as_object() else {
        outputs.push(entry_from(&category_str, top_name, "", "default", top_val));
        continue;
      };
      for (attr, val) in attrs {
        outputs.push(entry_from(&category_str, top_name, "", attr, val));
      }
    }
  }

  outputs.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.name.cmp(&b.name)));
  Ok(OutputsPayload {
    system: system.to_owned(),
    outputs,
    hidden,
  })
}

fn entry_from(
  category_display: &str,
  category_nix: &str,
  system: &str,
  attr: &str,
  val: &Value,
) -> OutputEntry {
  let kind_str = val
    .get("type")
    .and_then(Value::as_str)
    .map_or_else(
      || {
        FlakeOutput::from_nix_name(category_nix)
          .and_then(FlakeOutput::inferred_kind)
          .unwrap_or(FlakeOutputKind::Unknown)
          .as_str()
          .to_owned()
      },
      ToOwned::to_owned,
    );
  let description = val
    .get("description")
    .and_then(Value::as_str)
    .map(ToOwned::to_owned);
  let installable = if system.is_empty() {
    format!(".#{category_nix}.{attr}")
  } else {
    format!(".#{category_nix}.{system}.{attr}")
  };
  OutputEntry {
    category:    category_display.to_owned(),
    kind:        kind_str,
    name:        attr.to_owned(),
    description,
    installable,
  }
}
