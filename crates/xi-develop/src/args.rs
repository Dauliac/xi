use clap::{Args, Parser, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use xi_core::args::NixBuildPassthroughArgs;
use xi_core::complete;

#[derive(Parser, Debug)]
#[command(args_conflicts_with_subcommands = true)]
/// Enter or manage development shells
pub struct DevelopArgs {
  /// Target devShell attribute (defaults to "default", implies .# prefix)
  #[arg(add = ArgValueCompleter::new(complete::complete_devshells))]
  pub target: Option<String>,

  /// Command to run inside the development shell
  #[arg(long, short)]
  pub command: Option<String>,

  /// Flake reference (defaults to current directory)
  #[arg(long, env = "XI_FLAKE")]
  pub flake: Option<String>,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  /// Shell to use (defaults to $SHELL, e.g. zsh, bash, fish).
  /// Unlike `nix develop`, this preserves your prompt, aliases, and rc files.
  #[arg(long, short)]
  pub shell: Option<String>,

  #[command(flatten)]
  pub passthrough: NixBuildPassthroughArgs,

  /// Extra arguments passed to nix develop
  #[arg(last = true)]
  pub extra_args: Vec<String>,

  #[command(subcommand)]
  pub action: Option<DevelopAction>,
}

#[derive(Subcommand, Debug)]
pub enum DevelopAction {
  /// Trust a flake for automatic devshell activation
  Trust(TargetArgs),
  /// Revoke trust for a flake
  Untrust(TargetArgs),
  /// Generate shell activation script (add to your shell config)
  Activate(ActivateArgs),
  /// Switch the active async devshell target
  Switch(SwitchArgs),
  /// Remove cached state for the current flake (env files, meta, GC roots)
  Clean(CleanArgs),
  /// Manage the background daemon
  Daemon(DaemonArgs),
  /// Run a command with the devshell environment
  Exec(ExecArgs),
  /// List packages in the active devshell
  List(ListArgs),
  /// Show status of active devshells
  Status,
  /// Shell prompt hook (internal, called by activation script)
  #[command(hide = true)]
  Prompt(PromptArgs),
}

#[derive(Args, Debug)]
pub struct SwitchArgs {
  /// Target devShell to switch to
  #[arg(add = ArgValueCompleter::new(complete::complete_devshells))]
  pub target: String,
  /// Flake reference (defaults to current directory)
  #[arg(long)]
  pub flake: Option<String>,
  /// Shell type (for env file selection)
  #[arg(long, short)]
  pub shell: Option<String>,
}

#[derive(Args, Debug)]
pub struct TargetArgs {
  /// Target devShell attribute (defaults to "default")
  #[arg(add = ArgValueCompleter::new(complete::complete_devshells))]
  pub target: Option<String>,
  /// Flake reference (defaults to current directory)
  #[arg(long)]
  pub flake: Option<String>,
}

#[derive(Args, Debug)]
pub struct ActivateArgs {
  /// Shell type: bash, zsh, or fish
  pub shell: String,
}

#[derive(Args, Debug)]
pub struct ExecArgs {
  /// Target devShell attribute (defaults to "default")
  #[arg(long, short, default_value = "default")]
  pub target: String,
  /// Flake reference (defaults to current directory)
  #[arg(long)]
  pub flake: Option<String>,
  /// Command and arguments to run
  #[arg(required = true, trailing_var_arg = true)]
  pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ListArgs {
  /// Flake reference (defaults to current directory)
  #[arg(long)]
  pub flake: Option<String>,
  /// Show full store paths
  #[arg(long)]
  pub paths: bool,
  /// Output as JSON
  #[arg(long)]
  pub json: bool,
}

#[derive(Args, Debug)]
pub struct CleanArgs {
  /// Flake reference (defaults to current directory)
  #[arg(long)]
  pub flake: Option<String>,
  /// Remove state for ALL flakes (not just the current one)
  #[arg(long)]
  pub all: bool,
}

#[derive(Args, Debug)]
pub struct PromptArgs {
  /// Shell type: bash, zsh, or fish (optional for --exit mode)
  #[arg(long, short, default_value = "bash")]
  pub shell: String,
  /// Run in subshell mode (inside devshell, handles re-source/exit)
  #[arg(long)]
  pub subshell: bool,
  /// Run in exit mode (deregister consumer on subshell EXIT trap)
  #[arg(long)]
  pub exit: bool,
  /// Shell PID (for per-consumer tracking)
  #[arg(long)]
  pub pid: Option<u32>,
  /// Target devShell attribute
  #[arg(long, default_value = "default")]
  pub target: String,
}

#[derive(Args, Debug)]
pub struct DaemonArgs {
  #[command(subcommand)]
  pub action: DaemonAction,
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
  /// Start the daemon (foreground, used internally)
  Start(DaemonStartArgs),
  /// Stop the daemon
  Stop(DaemonFlakeArgs),
  /// Show daemon status
  Status(DaemonFlakeArgs),
}

#[derive(Args, Debug)]
pub struct DaemonStartArgs {
  /// Flake root directory
  #[arg(long)]
  pub flake: String,
  /// Seconds between eval attempts (from config)
  #[arg(long, default_value = "5", env = "XI_EVAL_INTERVAL")]
  pub eval_interval: u64,
  /// Extra file patterns to watch (from config, repeatable)
  #[arg(long, env = "XI_WATCH_EXTRA", value_delimiter = ':')]
  pub watch_extra: Vec<String>,
  /// Eval cache mode: none, lock (default), inputs
  #[arg(long, default_value = "lock", env = "XI_EVAL_CACHE")]
  pub eval_cache: String,
}

#[derive(Args, Debug)]
pub struct DaemonFlakeArgs {
  /// Flake reference (defaults to current directory)
  #[arg(long)]
  pub flake: Option<String>,
}
