use std::{path::PathBuf, process::Command, time::Instant};

use color_eyre::eyre::Result;
use xi_core::flake_output::current_nix_system;

use crate::{
  args::{ContextArgs, DevshellArgs, OutputsArgs},
  devshell,
  outputs,
  schema::{
    AgentContext, DevshellPayload, DevshellState, Envelope, FlakeSummary, GitState,
    WorkspaceInfo, XiConfigSummary, elapsed_ms, emit,
  },
  validate,
};

pub const CMD: &str = "context";

pub fn run(args: ContextArgs) -> Result<()> {
  let start = Instant::now();
  let workspace = collect_workspace(args.system.clone());
  let cwd = workspace.root.clone();
  let flake = collect_flake_summary(&cwd);
  let devshell = if args.no_daemon {
    DevshellPayload {
      state:               DevshellState::NotRunning,
      target:              None,
      package_count:       0,
      daemon_state:        None,
      active_cache_pushes: Vec::new(),
      entered_command:     "xi develop".to_owned(),
    }
  } else {
    devshell::collect(&DevshellArgs { timeout_ms: 500 })
  };
  let git = collect_git_state(&cwd);
  let validation = validate::plan_only();
  let xi_config = collect_xi_config(&cwd);

  let mut errors = Vec::new();
  // outputs are the most likely to fail (nix eval) — collect but do not
  // fail the whole context if they do.
  let outputs_args = OutputsArgs {
    all_systems:    false,
    system:         Some(workspace.current_system.clone()),
    include_hidden: false,
  };
  if let Err(diag) = outputs::collect(&outputs_args, &workspace.current_system) {
    errors.push(diag);
  }

  let payload = AgentContext {
    workspace,
    flake,
    devshell,
    git,
    validation,
    xi_config,
  };
  if errors.is_empty() {
    emit(&Envelope::ok(CMD, payload, elapsed_ms(start)))?;
  } else {
    emit(&Envelope::partial(CMD, payload, elapsed_ms(start), errors))?;
  }
  Ok(())
}

fn collect_workspace(system_override: Option<String>) -> WorkspaceInfo {
  let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
  let current_system = system_override.unwrap_or_else(current_nix_system);
  let xi_version = env!("CARGO_PKG_VERSION").to_owned();
  WorkspaceInfo {
    root,
    current_system,
    xi_version,
  }
}

fn collect_flake_summary(cwd: &std::path::Path) -> Option<FlakeSummary> {
  let flake = cwd.join("flake.nix");
  if !flake.exists() {
    return None;
  }
  let lock_hash = std::fs::read_to_string(cwd.join("flake.lock"))
    .ok()
    .and_then(|s| {
      serde_json::from_str::<serde_json::Value>(&s)
        .ok()
        .and_then(|v| v.get("root").and_then(|r| r.as_str()).map(ToOwned::to_owned))
    });
  Some(FlakeSummary {
    path: flake,
    lock_hash,
    systems: Vec::new(),
  })
}

fn collect_git_state(cwd: &std::path::Path) -> GitState {
  let head = run_git(cwd, &["rev-parse", "HEAD"]);
  let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
  let status = run_git(cwd, &["status", "--porcelain=v1", "-uall"]).unwrap_or_default();
  let mut untracked_count = 0u32;
  let mut dirty = false;
  for line in status.lines() {
    if line.starts_with("??") {
      untracked_count = untracked_count.saturating_add(1);
    } else if !line.trim().is_empty() {
      dirty = true;
    }
  }
  GitState {
    head,
    branch,
    dirty,
    untracked_count,
    ahead_behind: None,
  }
}

fn run_git(cwd: &std::path::Path, args: &[&str]) -> Option<String> {
  let out = Command::new("git")
    .args(args)
    .current_dir(cwd)
    .output()
    .ok()?;
  if !out.status.success() {
    return None;
  }
  let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
  if s.is_empty() { None } else { Some(s) }
}

fn collect_xi_config(cwd: &std::path::Path) -> Option<XiConfigSummary> {
  let path = cwd.join(".xi.toml");
  let contents = std::fs::read_to_string(&path).ok()?;
  let mut sections = Vec::new();
  let mut fmt_backend: Option<String> = None;
  let mut in_fmt = false;
  for line in contents.lines() {
    let l = line.trim();
    if let Some(name) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
      sections.push(name.to_owned());
      in_fmt = name == "fmt";
      continue;
    }
    if in_fmt
      && let Some(rhs) = l.strip_prefix("backend")
      && let Some(val) = rhs.split('=').nth(1)
    {
      fmt_backend = Some(val.trim().trim_matches('"').to_owned());
    }
  }
  Some(XiConfigSummary {
    path,
    sections,
    fmt_backend,
  })
}
