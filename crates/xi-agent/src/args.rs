use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
/// Structured project context for AI coding agents.
///
/// Every subcommand emits one JSON envelope on stdout and human
/// progress on stderr. Schema: `xi.agent/v1`.
pub struct AgentArgs {
  #[command(subcommand)]
  pub command: AgentCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
  /// One-shot context: outputs + devshell + git + validation plan.
  Context(ContextArgs),
  /// Enumerate flake outputs classified by kind.
  Outputs(OutputsArgs),
  /// Report the xi-develop daemon's view of the current devshell.
  Devshell(DevshellArgs),
  /// List files that would not be visible to a Nix build.
  Stage(StageArgs),
  /// List files reachable from `flake.nix` (the flake manifest).
  Manifest(ManifestArgs),
  /// Emit or execute the validation plan for this project.
  Validate(ValidateArgs),
  /// Install embedded skills into agent runtimes.
  Install(InstallArgs),
}

#[derive(Debug, Args)]
pub struct ContextArgs {
  /// Override the current system (defaults to running system).
  #[arg(long)]
  pub system: Option<String>,
  /// Do not open the daemon socket; devshell.state will be `not-running`.
  #[arg(long)]
  pub no_daemon: bool,
}

#[derive(Debug, Args)]
pub struct OutputsArgs {
  /// Enumerate outputs for every system declared by the flake.
  #[arg(long)]
  pub all_systems: bool,
  /// Restrict enumeration to this system.
  #[arg(long)]
  pub system: Option<String>,
  /// Include categories normally hidden from `xi flake show`.
  #[arg(long)]
  pub include_hidden: bool,
}

#[derive(Debug, Args)]
pub struct DevshellArgs {
  /// Milliseconds to wait for the daemon before degrading the response.
  #[arg(long, default_value = "500")]
  pub timeout_ms: u64,
}

#[derive(Debug, Args)]
pub struct StageArgs {}

#[derive(Debug, Args)]
pub struct ManifestArgs {}

#[derive(Debug, Args)]
pub struct ValidateArgs {
  /// Execute the plan instead of emitting it.
  #[arg(long)]
  pub run: bool,
  /// Stop at the first blocking failure when running.
  #[arg(long)]
  pub fail_fast: bool,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
  /// Scope of install.
  #[arg(long, value_enum, default_value = "user")]
  pub scope: InstallScope,
  /// Agent runtimes to target.
  #[arg(long, value_enum, default_value = "all")]
  pub target: InstallTarget,
  /// Rewrite files even when their SHA-256 already matches.
  #[arg(long)]
  pub force: bool,
  /// Print the plan without writing anything.
  #[arg(long)]
  pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InstallScope {
  /// Write to the user's home (`~/.claude`, `~/.codex`).
  User,
  /// Write to the current workspace (`./.claude`, `./.codex`).
  Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InstallTarget {
  All,
  ClaudeCode,
  Codex,
}
