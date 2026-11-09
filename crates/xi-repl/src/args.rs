use clap::Args;
use color_eyre::{Result, eyre::eyre};

#[derive(Debug, Args)]
/// Open an interactive repl against a flake's Nix evaluation context
///
/// Phase 0 (`openspec/changes/xi-repl`): a plain read-eval-print loop. Each
/// line is sent to the `xi-develop` daemon's `ExprEval` query and the typed,
/// classified result is printed with basic color by kind  -  no completion
/// menu, no syntax highlighting yet.
pub struct ReplArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Never spawn a session-scoped daemon.
  ///
  /// Evaluation still works if a daemon for this flake happens to already
  /// be reachable (e.g. you are inside an `xi develop` shell already);
  /// otherwise each evaluation reports a named "daemon unreachable" error
  /// instead of hanging.
  #[arg(long)]
  pub no_daemon: bool,

  /// Color theme for rendered values and syntax highlighting.
  ///
  /// One of: default, dracula, catppuccin-mocha, nord, gruvbox-dark,
  /// solarized-dark. Defaults to the `[repl]` section of `config.toml`
  /// when set there, otherwise `default`.
  #[arg(long)]
  pub theme: Option<crate::theme::ThemeName>,
}

impl ReplArgs {
  /// Run the repl.
  ///
  /// # Errors
  ///
  /// Returns an error if the flake path cannot be resolved, or (only when
  /// `--no-daemon` is not set) a session-scoped daemon needs to be spawned
  /// and fails to start.
  pub fn run(self) -> Result<()> {
    crate::theme::set(self.theme.unwrap_or_default());

    let flake_ref = self.flake_ref.as_deref().unwrap_or(".");
    let flake_root = std::fs::canonicalize(flake_ref)
      .map_err(|e| eyre!("cannot resolve flake path '{flake_ref}': {e}"))?;

    let socket_path =
      crate::session::resolve_socket(&flake_root, self.no_daemon)?;
    crate::session::run_loop(&socket_path, &flake_root)
  }
}
