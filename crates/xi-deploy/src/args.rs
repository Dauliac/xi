use clap::Args;

/// Deploy backend selection.
#[derive(Debug, Clone, Default, clap::ValueEnum)]
pub enum DeployBackendChoice {
  /// Auto-detect from flake outputs (deploy-rs → colmena → builtin)
  #[default]
  Auto,
  /// deploy-rs: multi-profile deployment with rollback
  DeployRs,
  /// Colmena: parallel fleet deployment
  Colmena,
  /// xi built-in: remote nixos-rebuild via SSH
  Builtin,
}

#[derive(Debug, Args)]
/// Deploy configurations to remote machines
///
/// Auto-detects the deployment backend from flake outputs:
///   `deploy` output → deploy-rs
///   `colmenaHive` output → colmena
///   `nixosConfigurations` → xi built-in remote rebuild
///
/// Override with --backend to force a specific tool.
pub struct DeployArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Target nodes to deploy (deploys all if none specified)
  #[arg(trailing_var_arg = true)]
  pub targets: Vec<String>,

  /// Filter targets by tag (colmena-style, prefix with @)
  #[arg(long)]
  pub on: Option<String>,

  /// Force a specific deployment backend
  #[arg(long, value_enum)]
  pub backend: Option<String>,

  /// Only print what would be deployed, without executing
  #[arg(long, short = 'n', alias = "dry-run")]
  pub dry: bool,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  /// Skip deployment checks (deploy-rs deployChecks)
  #[arg(long)]
  pub skip_checks: bool,

  /// Disable magic rollback (deploy-rs)
  #[arg(long)]
  pub no_magic_rollback: bool,

  /// Seconds to wait for deployment confirmation before rollback
  #[arg(long, default_value = "30")]
  pub confirm_timeout: u64,

  /// Display tracebacks on errors
  #[arg(long, short = 't', env = "XI_SHOW_TRACE", value_parser = clap::builder::BoolishValueParser::new())]
  pub show_trace: bool,

  /// Extra arguments passed to the underlying tool
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}
