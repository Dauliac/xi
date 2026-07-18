use std::{env, path::PathBuf};

use crate::installable::InstallableArgs;
use clap::{Args, ValueEnum};
use tracing::warn;

/// A single named cache push target.
#[derive(Debug, Clone)]
pub struct CacheTarget {
  /// Display name for this cache (TOML key or derived from URL).
  pub name: String,
  /// Nix store URI for push (s3://, ssh://, file://, etc.)
  pub push_url: Option<String>,
  /// Path to secret signing key.
  pub signing_key: Option<String>,
  /// External push command (e.g. `["cachix", "push", "mycache"]`).
  pub push_command: Vec<String>,
}

impl CacheTarget {
  /// Whether this target has a push method configured.
  #[must_use]
  pub const fn is_configured(&self) -> bool {
    self.push_url.is_some() || !self.push_command.is_empty()
  }
}

/// Cache push configuration.
///
/// Controls whether and how xi pushes built store paths to a binary cache
/// after a successful build. Supports two modes:
///
/// - `push_to`: uses `nix copy --to` with auto-tuned compression
/// - `push_command`: delegates to an external tool (cachix, attic, etc.)
#[derive(Debug, Default, Args, Clone)]
pub struct CacheArgs {
  /// Push build output to a binary cache via `nix copy --to`.
  /// Accepts any Nix store URI (s3://, ssh://, file://, etc.)
  #[arg(long, env = "XI_CACHE_URL")]
  pub push_to: Option<String>,

  /// Push build output using an external command.
  /// The store path is appended as the last argument.
  /// Example: --push-cmd cachix --push-cmd push --push-cmd mycache
  #[arg(long = "push-cmd", num_args = 1)]
  pub push_command: Vec<String>,

  /// Path to a secret key file for signing store paths before push
  #[arg(long, env = "XI_SIGNING_KEY")]
  pub sign_key: Option<String>,

  /// Disable cache push even if configured
  #[arg(long)]
  pub no_push: bool,

  /// Push to cache asynchronously in the background.
  /// The CLI returns immediately after build; push runs detached.
  /// Can also be enabled via `XI_CACHE_ASYNC=1` or xi config.
  #[arg(long, env = "XI_CACHE_ASYNC", value_parser = clap::builder::BoolishValueParser::new())]
  pub async_push: bool,

  /// Named cache targets from config file.
  #[arg(skip)]
  pub config_targets: Vec<CacheTarget>,
}

impl CacheArgs {
  /// Resolve the effective list of cache targets.
  ///
  /// CLI flags override config: if `--push-to` or `--push-cmd` is given,
  /// only that single target is used. Otherwise, config targets are returned.
  #[must_use]
  pub fn resolve_targets(&self) -> Vec<CacheTarget> {
    if self.no_push {
      return vec![];
    }

    let cli_url = self
      .push_to
      .clone()
      .or_else(|| env::var("XI_CACHE_URL").ok().filter(|v| !v.is_empty()));
    let cli_key = self
      .sign_key
      .clone()
      .or_else(|| env::var("XI_SIGNING_KEY").ok().filter(|v| !v.is_empty()));

    // CLI flags override all config targets
    if cli_url.is_some() || !self.push_command.is_empty() {
      let name = cli_url
        .as_deref()
        .map(display_cache_name)
        .or_else(|| self.push_command.first().cloned())
        .unwrap_or_else(|| "cache".to_string());

      return vec![CacheTarget {
        name,
        push_url: cli_url,
        signing_key: cli_key,
        push_command: self.push_command.clone(),
      }];
    }

    // Use config targets, applying env signing key as fallback
    self
      .config_targets
      .iter()
      .map(|t| {
        let mut t = t.clone();
        if t.signing_key.is_none() {
          t.signing_key.clone_from(&cli_key);
        }
        t
      })
      .collect()
  }

  /// Apply defaults from a config file.
  ///
  /// Priority: CLI flag > env var > config file > default
  pub fn apply_config_defaults(
    &mut self,
    targets: &[CacheTarget],
    async_push: bool,
  ) {
    // async_push: CLI/env > config
    if !self.async_push && env::var("XI_CACHE_ASYNC").is_err() {
      self.async_push = async_push;
    }

    // config_targets: only set if no CLI override
    if self.config_targets.is_empty() && !targets.is_empty() {
      self.config_targets = targets.to_vec();
    }
  }
}

/// Strip query parameters from a URL for display.
fn display_cache_name(url: &str) -> String {
  url.split('?').next().unwrap_or(url).to_string()
}

#[derive(Debug, Args)]
pub struct CommonRebuildArgs {
  /// Only print actions, without performing them
  #[arg(long, short = 'n', alias = "dry-run")]
  pub dry: bool,

  /// Ask for confirmation
  #[arg(long, short)]
  pub ask: bool,

  #[command(flatten)]
  pub installable: InstallableArgs,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  /// Path to save the result link, defaults to using a temporary directory
  #[arg(long, short)]
  pub out_link: Option<PathBuf>,

  /// Whether to display a package diff
  #[arg(long, short, value_enum, default_value_t = DiffType::Auto, env = "XI_DIFF")]
  pub diff: DiffType,

  #[command(flatten)]
  pub passthrough: NixBuildPassthroughArgs,

  #[command(flatten)]
  pub cache: CacheArgs,
}

#[derive(ValueEnum, Clone, Default, Debug)]
pub enum DiffType {
  /// Display package diff only if the of the
  /// current and the deployed configuration matches
  #[default]
  Auto,
  /// Always display package diff
  Always,
  /// Never display package diff
  Never,
}

#[derive(Debug, Default, Args)]
pub struct NixBuildPassthroughArgs {
  /// Number of concurrent jobs Nix should run
  #[arg(long, short = 'j', env = "XI_MAX_JOBS")]
  pub max_jobs: Option<usize>,

  /// Number of cores Nix should utilize
  #[arg(long)]
  pub cores: Option<usize>,

  /// Logging format used by Nix
  #[arg(long)]
  pub log_format: Option<String>,

  /// Continue building despite encountering errors
  #[arg(long, short = 'k', env = "XI_KEEP_GOING", value_parser = clap::builder::BoolishValueParser::new())]
  pub keep_going: bool,

  /// Keep build outputs from failed builds
  #[arg(long, short = 'K')]
  pub keep_failed: bool,

  /// Attempt to build locally if substituters fail
  #[arg(long)]
  pub fallback: bool,

  /// Repair corrupted store paths
  #[arg(long)]
  pub repair: bool,

  /// Explicitly define remote builders
  #[arg(long)]
  pub builders: Option<String>,

  /// Paths to include
  #[arg(long, short = 'I')]
  pub include: Vec<String>,

  /// Print build logs directly to stdout
  #[arg(long, short = 'L')]
  pub print_build_logs: bool,

  /// Display tracebacks on errors
  #[arg(long, short = 't', env = "XI_SHOW_TRACE", value_parser = clap::builder::BoolishValueParser::new())]
  pub show_trace: bool,

  /// Accept configuration from flakes
  #[arg(long, env = "XI_ACCEPT_FLAKE_CONFIG", value_parser = clap::builder::BoolishValueParser::new())]
  pub accept_flake_config: bool,

  /// Refresh flakes to the latest revision
  #[arg(long)]
  pub refresh: bool,

  /// Allow impure builds
  #[arg(long, env = "XI_IMPURE", value_parser = clap::builder::BoolishValueParser::new())]
  pub impure: bool,

  /// Build without internet access
  #[arg(long, env = "XI_OFFLINE", value_parser = clap::builder::BoolishValueParser::new())]
  pub offline: bool,

  /// Prohibit network usage
  #[arg(long)]
  pub no_net: bool,

  /// Recreate the flake.lock file entirely
  #[arg(long)]
  pub recreate_lock_file: bool,

  /// Do not update the flake.lock file
  #[arg(long)]
  pub no_update_lock_file: bool,

  /// Do not write a lock file
  #[arg(long)]
  pub no_write_lock_file: bool,

  /// Do not use registries
  #[arg(long = "no-use-registries")]
  pub no_use_registries: bool,

  /// Do not use registries (deprecated, use --no-use-registries)
  #[arg(long, alias = "no-registries")]
  pub no_registries: bool,

  /// Commit the lock file after updates
  #[arg(long)]
  pub commit_lock_file: bool,

  /// Suppress build output
  #[arg(long, short = 'Q')]
  pub no_build_output: bool,

  /// Use substitutes when copying
  #[arg(long)]
  pub use_substitutes: bool,

  /// Output results in JSON format
  #[arg(long)]
  pub json: bool,

  /// Set a Nix configuration option (may be given multiple times)
  #[arg(long, number_of_values = 2, value_names = ["NAME", "VALUE"])]
  pub option: Vec<String>,

  /// Override a specific flake input (may be given multiple times)
  #[arg(long, number_of_values = 2, value_names = ["INPUT", "FLAKE_URL"])]
  pub override_input: Vec<String>,

  /// Substituter connection timeout in seconds (injected from config).
  /// Not a CLI flag — applied via `apply_build_defaults`.
  #[arg(skip)]
  pub connect_timeout: Option<u64>,
}

/// Push `--$flag` onto `$args` when `$self.$field` is `true`.
macro_rules! bool_flags {
  ($self:expr, $args:expr, $( $field:ident => $flag:expr ),* $(,)?) => {
    $( if $self.$field { $args.push($flag.into()); } )*
  };
}

impl NixBuildPassthroughArgs {
  #[must_use]
  pub fn generate_passthrough_args(&self) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(jobs) = self.max_jobs {
      args.push("--max-jobs".into());
      args.push(jobs.to_string());
    }
    if let Some(cores) = self.cores {
      args.push("--cores".into());
      args.push(cores.to_string());
    }
    if let Some(ref format) = self.log_format {
      args.push("--log-format".into());
      args.push(format.clone());
    }
    bool_flags!(self, args,
      keep_going           => "--keep-going",
      keep_failed          => "--keep-failed",
      fallback             => "--fallback",
      repair               => "--repair",
    );
    if let Some(ref builders) = self.builders {
      args.push("--builders".into());
      args.push(builders.clone());
    }
    for inc in &self.include {
      args.push("--include".into());
      args.push(inc.clone());
    }
    bool_flags!(self, args,
      print_build_logs     => "--print-build-logs",
      show_trace           => "--show-trace",
      accept_flake_config  => "--accept-flake-config",
      refresh              => "--refresh",
      impure               => "--impure",
      offline              => "--offline",
      no_net               => "--no-net",
      recreate_lock_file   => "--recreate-lock-file",
      no_update_lock_file  => "--no-update-lock-file",
      no_write_lock_file   => "--no-write-lock-file",
      no_use_registries    => "--no-use-registries",
    );
    if self.no_registries {
      warn!("--no-registries is deprecated, use --no-use-registries instead");
      args.push("--no-use-registries".into());
    }
    if self.no_build_output {
      args.push("--quiet".into());
    }
    bool_flags!(self, args,
      commit_lock_file     => "--commit-lock-file",
      use_substitutes      => "--use-substitutes",
      json                 => "--json",
    );
    // Inject connect-timeout before user --option pairs so explicit
    // `--option connect-timeout N` from the CLI takes precedence.
    let user_sets_timeout = self
      .option
      .chunks(2)
      .any(|pair| pair[0] == "connect-timeout");
    if !user_sets_timeout {
      if let Some(timeout) = self.connect_timeout {
        args.push("--option".into());
        args.push("connect-timeout".into());
        args.push(timeout.to_string());
      }
    }
    for pair in self.option.chunks(2) {
      args.push("--option".into());
      args.push(pair[0].clone());
      args.push(pair[1].clone());
    }
    for pair in self.override_input.chunks(2) {
      args.push("--override-input".into());
      args.push(pair[0].clone());
      args.push(pair[1].clone());
    }

    args
  }

  /// Apply defaults from config file. Config values only take effect
  /// when the corresponding CLI flag / env var was not provided.
  ///
  /// Priority: CLI flag > env var > config file > default
  #[allow(clippy::fn_params_excessive_bools)]
  pub const fn apply_build_defaults(
    &mut self,
    show_trace: bool,
    keep_going: bool,
    impure: bool,
    accept_flake_config: bool,
    offline: bool,
    max_jobs: Option<usize>,
    connect_timeout: Option<u64>,
  ) {
    if !self.show_trace {
      self.show_trace = show_trace;
    }
    if !self.keep_going {
      self.keep_going = keep_going;
    }
    if !self.impure {
      self.impure = impure;
    }
    if !self.accept_flake_config {
      self.accept_flake_config = accept_flake_config;
    }
    if !self.offline {
      self.offline = offline;
    }
    if self.max_jobs.is_none() {
      self.max_jobs = max_jobs;
    }
    if self.connect_timeout.is_none() {
      self.connect_timeout = connect_timeout;
    }
  }
}

impl HasCacheArgs for CommonRebuildArgs {
  fn cache_args_mut(&mut self) -> Option<&mut CacheArgs> {
    Some(&mut self.cache)
  }
}

impl HasBuildArgs for CommonRebuildArgs {
  fn build_passthrough_mut(&mut self) -> Option<&mut NixBuildPassthroughArgs> {
    Some(&mut self.passthrough)
  }

  fn no_nom_mut(&mut self) -> Option<&mut bool> {
    Some(&mut self.no_nom)
  }
}

/// Trait for command args that contain cache push configuration.
///
/// Implement this on any args struct that embeds [`CacheArgs`] so that
/// config-file defaults can be applied generically without matching on
/// every command variant.
pub trait HasCacheArgs {
  /// Return a mutable reference to the embedded cache args, or `None`
  /// if this command variant doesn't support cache push.
  fn cache_args_mut(&mut self) -> Option<&mut CacheArgs>;
}

/// Trait for command args that accept Nix build passthrough flags.
///
/// Implement this on any args struct that embeds
/// [`NixBuildPassthroughArgs`] and/or a `no_nom` toggle so that
/// config-file build defaults can be applied generically.
pub trait HasBuildArgs {
  /// Return a mutable reference to the embedded passthrough args, or
  /// `None` if this command variant doesn't support build passthroughs.
  fn build_passthrough_mut(&mut self) -> Option<&mut NixBuildPassthroughArgs>;

  /// Return a mutable reference to the `no_nom` flag, or `None` if
  /// this command variant doesn't have one.
  fn no_nom_mut(&mut self) -> Option<&mut bool>;
}

#[cfg(test)]
mod tests {
  use super::NixBuildPassthroughArgs;

  #[test]
  fn no_build_output_maps_to_nix_quiet_flag() {
    let args = NixBuildPassthroughArgs {
      no_build_output: true,
      ..Default::default()
    };

    assert_eq!(args.generate_passthrough_args(), ["--quiet"]);
  }

  #[test]
  fn option_pairs_are_emitted() {
    let args = NixBuildPassthroughArgs {
      option: vec![
        "sandbox".into(),
        "false".into(),
        "cores".into(),
        "4".into(),
      ],
      ..Default::default()
    };

    assert_eq!(
      args.generate_passthrough_args(),
      ["--option", "sandbox", "false", "--option", "cores", "4"]
    );
  }

  #[test]
  fn connect_timeout_injected_by_default() {
    let args = NixBuildPassthroughArgs {
      connect_timeout: Some(5),
      ..Default::default()
    };
    let generated = args.generate_passthrough_args();
    assert_eq!(generated, ["--option", "connect-timeout", "5"]);
  }

  #[test]
  fn connect_timeout_skipped_when_user_sets_option() {
    let args = NixBuildPassthroughArgs {
      connect_timeout: Some(5),
      option: vec!["connect-timeout".into(), "30".into()],
      ..Default::default()
    };
    let generated = args.generate_passthrough_args();
    assert_eq!(generated, ["--option", "connect-timeout", "30"]);
  }

  #[test]
  fn connect_timeout_none_emits_nothing() {
    let args = NixBuildPassthroughArgs {
      connect_timeout: None,
      ..Default::default()
    };
    assert!(args.generate_passthrough_args().is_empty());
  }

  #[test]
  fn override_input_pairs_are_emitted() {
    let args = NixBuildPassthroughArgs {
      override_input: vec![
        "nixpkgs".into(),
        "github:NixOS/nixpkgs/nixos-unstable".into(),
      ],
      ..Default::default()
    };

    assert_eq!(
      args.generate_passthrough_args(),
      [
        "--override-input",
        "nixpkgs",
        "github:NixOS/nixpkgs/nixos-unstable"
      ]
    );
  }
}
