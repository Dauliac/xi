use clap::{Args, Subcommand};

/// Manage Nix `access-tokens` via `nix-auth` (OAuth device flow).
///
/// Delegates to the upstream `nix-auth` binary (github:numtide/nix-auth)
/// bundled at xi build time. Tokens are stored in
/// `~/.config/nix/access-tokens.conf` (0600) and included from `nix.conf`
/// via `!include access-tokens.conf`.
#[derive(Debug, Args)]
pub struct AuthArgs {
  #[command(subcommand)]
  pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
  /// Log in to a git forge (GitHub, GitLab, Gitea, Forgejo, ...).
  ///
  /// Runs the OAuth device flow when supported, or prompts for a
  /// Personal Access Token as a fallback. The resulting token is written
  /// to `~/.config/nix/access-tokens.conf`.
  ///
  /// Extra positional and flag arguments are forwarded to `nix-auth login`
  /// verbatim — e.g. `xi auth login gitlab.company.com --provider gitlab
  /// --client-id <id>`.
  Login {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
  },

  /// Show configured tokens and their validity.
  ///
  /// Extra arguments are forwarded to `nix-auth status`.
  Status {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
  },

  /// Remove a stored token.
  ///
  /// Extra arguments are forwarded to `nix-auth logout`.
  Logout {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
  },
}

impl AuthCommand {
  #[must_use]
  pub fn subcommand_name(&self) -> &'static str {
    match self {
      Self::Login { .. } => "login",
      Self::Status { .. } => "status",
      Self::Logout { .. } => "logout",
    }
  }

  #[must_use]
  pub fn passthrough_args(&self) -> &[String] {
    match self {
      Self::Login { args } | Self::Status { args } | Self::Logout { args } => {
        args
      },
    }
  }
}
