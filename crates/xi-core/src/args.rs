use std::{env, path::PathBuf};

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::installable::InstallableArgs;

/// Closed set of S3-compatible provider presets.
///
/// `provider` only pre-fills `endpoint`/`region` before the existing
/// `s3://bucket?...` URI is assembled; push/pull mechanics are identical
/// across all variants (one generic `s3-binary-cache-store`, not a
/// per-vendor plugin model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheProvider {
  /// Cloudflare R2: endpoint derived from `account_id`, region is always
  /// `auto`.
  CloudflareR2 { account_id: String },
  /// AWS S3: `region` is passed through; omitted entirely when absent
  /// (letting `s3://` fall back to its own built-in default).
  AwsS3 { region: Option<String> },
  /// Any other S3-compatible endpoint (Backblaze B2, Wasabi, `MinIO`, ...).
  Generic {
    endpoint: Option<String>,
    region:   Option<String>,
  },
}

impl CacheProvider {
  /// Parse the closed `provider = "<kebab-case>"` TOML value.
  ///
  /// `account_id`/`region`/`endpoint` are read from the same
  /// `[cache.<name>]` table as plain sibling keys, not nested under the
  /// provider value itself.
  ///
  /// # Errors
  /// Returns a message naming the invalid value and the closed set of
  /// accepted ones.
  pub fn parse(
    value: &str,
    account_id: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
  ) -> Result<Self, String> {
    match value {
      "cloudflare-r2" => {
        let account_id = account_id.ok_or_else(|| {
          "provider = \"cloudflare-r2\" requires 'account_id'".to_string()
        })?;
        Ok(Self::CloudflareR2 { account_id })
      },
      "aws-s3" => Ok(Self::AwsS3 { region }),
      "generic" => Ok(Self::Generic { endpoint, region }),
      other => {
        Err(format!(
          "unknown provider '{other}' (expected one of: cloudflare-r2, \
           aws-s3, generic)"
        ))
      },
    }
  }
}

/// `[cache.<name>.credentials]`: file-backed S3 push credentials.
///
/// Values are paths read through
/// [`crate::sources::credentials::Secret::from_file`] and injected only as
/// environment variables on the single `nix copy` child process; they are
/// never exported to xi's own process environment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheCredentials {
  /// Path to a file holding the S3 access key id.
  pub access_key_id_file:     Option<String>,
  /// Path to a file holding the S3 secret access key.
  pub secret_access_key_file: Option<String>,
}

impl CacheCredentials {
  /// Whether any credential file is configured.
  #[must_use]
  pub const fn is_empty(&self) -> bool {
    self.access_key_id_file.is_none() && self.secret_access_key_file.is_none()
  }
}

/// A single named cache push/pull target.
#[derive(Debug, Clone, Default)]
pub struct CacheTarget {
  /// Display name for this cache (TOML key or derived from URL).
  pub name:            String,
  /// Nix store URI for push (s3://, ssh://, file://, etc.)
  ///
  /// Takes precedence over `provider`/`bucket` when both are present  -  an
  /// escape hatch for anything the preset table doesn't yet cover.
  pub push_url:        Option<String>,
  /// Path to a file holding the push URL, read at push time only -
  /// never written to `config.toml`/the Nix store.
  ///
  /// For a `push_url` sensitive enough to be worth encrypting (it can
  /// embed an account id, a bucket naming scheme, etc.), the plain
  /// `push_url` field is the wrong fit: it is baked verbatim into
  /// `config.toml`, which lands world-readable in `/nix/store`. This
  /// mirrors [`CacheCredentials`]'s own file-backed fields and the
  /// same sops-nix / agenix "secret not mounted yet" handling via
  /// [`crate::sources::credentials::Secret::from_file`]. Only
  /// consulted by [`crate::cache::push_single_target`]'s synchronous
  /// path - like credentials, not yet read by the async daemon/detached
  /// dispatch in [`crate::cache::push_to_cache_async`], which would
  /// otherwise have to pass the decrypted value through a socket
  /// message or a spawned shell command line instead of a scoped child
  /// env var. Ignored when `push_url` is also set.
  pub push_url_file:   Option<String>,
  /// Path to secret signing key.
  pub signing_key:     Option<String>,
  /// External push command (e.g. `["cachix", "push", "mycache"]`).
  pub push_command:    Vec<String>,
  /// Provider preset used to pre-fill `endpoint`/`region` for the `s3://`
  /// URI assembled from `bucket`.
  pub provider:        Option<CacheProvider>,
  /// Bucket name, used together with `provider` to build the `s3://` URI.
  pub bucket:          Option<String>,
  /// Native file-backed push credentials.
  pub credentials:     Option<CacheCredentials>,
  /// Substituter URL used for pull (`--option extra-substituters`).
  pub pull_url:        Option<String>,
  /// Trusted public key used for pull
  /// (`--option extra-trusted-public-keys`). When absent and `signing_key`
  /// is set, derived from it (see
  /// [`crate::cache::resolve_pull_public_key`]).
  pub pull_public_key: Option<String>,
  /// Known-cache registry preset name (resolves `pull_url`/
  /// `pull_public_key` when they are not set explicitly).
  pub preset:          Option<String>,
  /// `host/path` glob patterns scoping this target to specific git
  /// remotes / flake-refs. Empty means "matches everything".
  pub match_patterns:  Vec<String>,
  /// Catch-all target used when no configured target's `match` matches
  /// the current remote.
  pub default:         bool,
  /// A failed push to this target makes the invoking `xi ... switch`'s
  /// own exit code non-zero instead of silently queuing.
  pub required:        bool,
}

impl CacheTarget {
  /// Whether this target has a push method configured.
  #[must_use]
  #[allow(
    clippy::suspicious_operation_groupings,
    reason = "three genuinely independent push methods, not a typo'd pair"
  )]
  pub const fn is_configured(&self) -> bool {
    self.push_url.is_some()
      || self.push_url_file.is_some()
      || !self.push_command.is_empty()
      || (self.provider.is_some() && self.bucket.is_some())
  }

  /// Build the `s3://bucket?region=...&endpoint=...` URI from `provider` +
  /// `bucket`, ignoring any explicit `push_url` escape hatch.
  ///
  /// Returns an empty string when `bucket` is unset; callers only reach
  /// this once `provider`+`bucket` presence has already been checked (see
  /// [`Self::effective_push_url`]).
  #[must_use]
  pub fn resolved_s3_uri(&self) -> String {
    let Some(bucket) = &self.bucket else {
      return String::new();
    };
    let Some(provider) = &self.provider else {
      return format!("s3://{bucket}");
    };

    let mut params: Vec<(&str, String)> = Vec::new();
    match provider {
      CacheProvider::CloudflareR2 { account_id } => {
        params.push(("region", "auto".to_string()));
        params.push((
          "endpoint",
          format!("https://{account_id}.r2.cloudflarestorage.com"),
        ));
      },
      CacheProvider::AwsS3 { region } => {
        if let Some(region) = region {
          params.push(("region", region.clone()));
        }
      },
      CacheProvider::Generic { endpoint, region } => {
        if let Some(region) = region {
          params.push(("region", region.clone()));
        }
        if let Some(endpoint) = endpoint {
          params.push(("endpoint", endpoint.clone()));
        }
      },
    }

    if params.is_empty() {
      return format!("s3://{bucket}");
    }

    let query = params
      .iter()
      .map(|(k, v)| format!("{k}={v}"))
      .collect::<Vec<_>>()
      .join("&");
    format!("s3://{bucket}?{query}")
  }

  /// The URL xi actually pushes to: an explicit `push_url` wins verbatim
  /// over `provider`+`bucket`; otherwise the provider preset is resolved.
  #[must_use]
  pub fn effective_push_url(&self) -> Option<String> {
    if let Some(url) = &self.push_url {
      return Some(url.clone());
    }
    if self.provider.is_some() && self.bucket.is_some() {
      return Some(self.resolved_s3_uri());
    }
    None
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

  /// Bound on concurrent `nix copy` child processes a single push cycle
  /// (current build + queue drain) may run. CLI > env > config.toml
  /// `[cache] max_parallel_jobs` > `min(available_parallelism(), 8)`.
  #[arg(long = "max-parallel-jobs", env = "XI_CACHE_MAX_PARALLEL_JOBS")]
  pub max_parallel_jobs: Option<usize>,

  /// Named cache targets from config file.
  #[arg(skip)]
  pub config_targets: Vec<CacheTarget>,

  /// The resolved git-remote / flake-ref match candidate for this
  /// invocation (see `xi-binary-cache-match`). `None` means "no
  /// candidate resolved"  -  targets with a non-empty `match` list are
  /// then only selected via `default = true`; targets with an empty
  /// `match` list (the back-compat default) always apply.
  #[arg(skip)]
  pub match_candidate: Option<String>,
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
        ..Default::default()
      }];
    }

    // Use config targets, applying env signing key as fallback
    let targets: Vec<CacheTarget> = self
      .config_targets
      .iter()
      .map(|t| {
        let mut t = t.clone();
        if t.signing_key.is_none() {
          t.signing_key.clone_from(&cli_key);
        }
        t
      })
      .collect();

    crate::cache_match::select_targets(
      &targets,
      self.match_candidate.as_deref(),
    )
    .into_iter()
    .cloned()
    .collect()
  }

  /// Set the resolved match candidate for this invocation. See
  /// [`Self::match_candidate`].
  pub fn set_match_candidate(&mut self, candidate: Option<String>) {
    self.match_candidate = candidate;
  }

  /// Apply defaults from a config file.
  ///
  /// Priority: CLI flag > env var > config file > default
  pub fn apply_config_defaults(
    &mut self,
    targets: &[CacheTarget],
    async_push: bool,
    max_parallel_jobs: Option<usize>,
  ) {
    // async_push: CLI/env > config
    if !self.async_push && env::var("XI_CACHE_ASYNC").is_err() {
      self.async_push = async_push;
    }

    // config_targets: only set if no CLI override
    if self.config_targets.is_empty() && !targets.is_empty() {
      self.config_targets = targets.to_vec();
    }

    // max_parallel_jobs: CLI/env (already merged by clap) > config.toml
    if self.max_parallel_jobs.is_none() {
      self.max_parallel_jobs = max_parallel_jobs;
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

  #[command(flatten)]
  pub sources: crate::sources::switch::SwitchSourcesArgs,

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

#[derive(Debug, Default, Clone, Args)]
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

  /// Resolved substituter URLs for every matched cache target.
  /// Injected as additive `--option extra-substituters`. Not a CLI
  /// flag; applied via `apply_cache_pull_flags`.
  #[arg(skip)]
  pub extra_substituters: Vec<String>,

  /// Resolved trusted public keys for every matched cache target.
  /// Injected as additive `--option extra-trusted-public-keys`. Not a
  /// CLI flag; applied via `apply_cache_pull_flags`.
  #[arg(skip)]
  pub extra_trusted_public_keys: Vec<String>,

  /// Overrides resolved from the config cascade (global `config.toml`,
  /// `.xi.toml`, `.xi.local.toml`, `XI_OVERRIDE_INPUT`), keyed by input
  /// name and already normalized to flakerefs.
  ///
  /// Not a CLI flag  -  injected via `apply_override_input_defaults`. Kept
  /// separate from `override_input` so a `--override-input` flag can
  /// replace a configured entry by name at emission time.
  #[arg(skip)]
  pub config_override_input: std::collections::BTreeMap<String, String>,
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
    if !user_sets_timeout && let Some(timeout) = self.connect_timeout {
      args.push("--option".into());
      args.push("connect-timeout".into());
      args.push(timeout.to_string());
    }
    for pair in self.option.chunks(2) {
      args.push("--option".into());
      args.push(pair[0].clone());
      args.push(pair[1].clone());
    }
    if !self.extra_substituters.is_empty() {
      args.push("--option".into());
      args.push("extra-substituters".into());
      args.push(self.extra_substituters.join(" "));
    }
    if !self.extra_trusted_public_keys.is_empty() {
      args.push("--option".into());
      args.push("extra-trusted-public-keys".into());
      args.push(self.extra_trusted_public_keys.join(" "));
    }
    for (name, flakeref) in self.effective_override_input() {
      args.push("--override-input".into());
      args.push(name);
      args.push(flakeref);
    }

    args
  }

  /// Resolve `--override-input` flags against config-derived overrides,
  /// merging by input name so a flag replaces only the entry it names and
  /// leaves the rest of the configured set applied.
  ///
  /// Sorted, so the emitted argv and the report are deterministic.
  #[must_use]
  pub fn effective_override_input(&self) -> Vec<(String, String)> {
    let mut merged: std::collections::BTreeMap<String, String> =
      self.config_override_input.clone();

    for pair in self.override_input.chunks(2) {
      if pair.len() == 2 {
        merged.insert(pair[0].clone(), pair[1].clone());
      }
    }

    merged.into_iter().collect()
  }

  /// Install overrides resolved from the config cascade.
  ///
  /// A separate method rather than a parameter on `apply_build_defaults`:
  /// that one is `pub const fn` and merges scalars by field, and a map
  /// merge cannot happen in a const fn.
  pub fn apply_override_input_defaults(
    &mut self,
    overrides: &std::collections::BTreeMap<String, String>,
  ) {
    self.config_override_input = overrides.clone();
  }

  /// Apply defaults from config file. Config values only take effect
  /// when the corresponding CLI flag / env var was not provided.
  ///
  /// Priority: CLI flag > env var > config file > default
  #[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
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

  /// Apply the resolved pull configuration from every matched
  /// `[cache.<name>]` target (see `xi-binary-cache-match`). Additive by
  /// construction: callers pass the full resolved set, and
  /// `generate_passthrough_args` emits them as `extra-substituters`/
  /// `extra-trusted-public-keys`, which Nix itself appends to rather
  /// than replaces.
  pub fn apply_cache_pull_flags(
    &mut self,
    substituters: Vec<String>,
    trusted_public_keys: Vec<String>,
  ) {
    self.extra_substituters = substituters;
    self.extra_trusted_public_keys = trusted_public_keys;
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

/// Trait for command args that accept an [`InstallableArgs`].
///
/// Implement this on any args struct that embeds one, so
/// `xi-binary-cache-match`'s resolution ladder (explicit flake-ref
/// outranks the git remote of the current working directory) can read
/// it generically without matching on every command variant.
pub trait HasInstallableArgs {
  /// Return the embedded installable args, or `None` if this command
  /// variant doesn't have one.
  fn installable(&self) -> Option<&InstallableArgs>;
}

impl HasInstallableArgs for CommonRebuildArgs {
  fn installable(&self) -> Option<&InstallableArgs> {
    Some(&self.installable)
  }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "Fine in tests")]
mod cache_target_tests {
  use super::{CacheProvider, CacheTarget};

  #[test]
  fn cloudflare_r2_preset_derives_its_endpoint() {
    let target = CacheTarget {
      provider: Some(CacheProvider::CloudflareR2 {
        account_id: "abc123".to_string(),
      }),
      bucket: Some("nix-cache".to_string()),
      ..Default::default()
    };
    assert_eq!(
      target.resolved_s3_uri(),
      "s3://nix-cache?region=auto&endpoint=https://abc123.r2.cloudflarestorage.com"
    );
  }

  #[test]
  fn aws_s3_preset_omits_endpoint() {
    let target = CacheTarget {
      provider: Some(CacheProvider::AwsS3 {
        region: Some("eu-west-3".to_string()),
      }),
      bucket: Some("nix-cache".to_string()),
      ..Default::default()
    };
    let uri = target.resolved_s3_uri();
    assert_eq!(uri, "s3://nix-cache?region=eu-west-3");
    assert!(!uri.contains("endpoint="));
  }

  #[test]
  fn hand_written_push_url_overrides_the_preset() {
    let target = CacheTarget {
      provider: Some(CacheProvider::CloudflareR2 {
        account_id: "abc123".to_string(),
      }),
      bucket: Some("nix-cache".to_string()),
      push_url: Some("s3://explicit-bucket".to_string()),
      ..Default::default()
    };
    assert_eq!(
      target.effective_push_url(),
      Some("s3://explicit-bucket".to_string())
    );
  }

  #[test]
  fn unknown_provider_is_rejected() {
    let err = CacheProvider::parse("not-a-real-provider", None, None, None)
      .unwrap_err();
    assert!(err.contains("not-a-real-provider"));
    assert!(err.contains("cloudflare-r2"));
  }

  #[test]
  fn cloudflare_r2_without_account_id_is_rejected() {
    let err =
      CacheProvider::parse("cloudflare-r2", None, None, None).unwrap_err();
    assert!(err.contains("account_id"));
  }

  #[test]
  fn generic_preset_carries_endpoint_and_region() {
    let target = CacheTarget {
      provider: Some(CacheProvider::Generic {
        endpoint: Some("https://minio.example.com".to_string()),
        region:   Some("us-east-1".to_string()),
      }),
      bucket: Some("nix-cache".to_string()),
      ..Default::default()
    };
    let uri = target.resolved_s3_uri();
    assert!(uri.contains("region=us-east-1"));
    assert!(uri.contains("endpoint=https://minio.example.com"));
  }

  #[test]
  fn is_configured_recognises_provider_and_bucket_without_push_url() {
    let target = CacheTarget {
      provider: Some(CacheProvider::AwsS3 { region: None }),
      bucket: Some("nix-cache".to_string()),
      ..Default::default()
    };
    assert!(target.is_configured());
  }

  #[test]
  fn not_configured_without_any_push_method() {
    assert!(!CacheTarget::default().is_configured());
  }

  #[test]
  fn is_configured_recognises_push_url_file_alone() {
    let target = CacheTarget {
      push_url_file: Some("/run/secrets/cache-url".to_string()),
      ..Default::default()
    };
    assert!(target.is_configured());
  }
}

#[cfg(test)]
mod tests {
  use super::{CacheArgs, NixBuildPassthroughArgs};

  #[test]
  fn max_parallel_jobs_falls_back_to_config_toml_when_cli_and_env_are_unset() {
    let mut cache = CacheArgs::default();
    cache.apply_config_defaults(&[], false, Some(3));
    assert_eq!(cache.max_parallel_jobs, Some(3));
  }

  #[test]
  fn max_parallel_jobs_cli_flag_outranks_config_toml() {
    let mut cache = CacheArgs {
      max_parallel_jobs: Some(1),
      ..Default::default()
    };
    cache.apply_config_defaults(&[], false, Some(3));
    assert_eq!(cache.max_parallel_jobs, Some(1));
  }

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

    assert_eq!(args.generate_passthrough_args(), [
      "--option", "sandbox", "false", "--option", "cores", "4"
    ]);
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
  fn cache_pull_flags_are_additive_extra_options() {
    let mut args = NixBuildPassthroughArgs::default();
    args.apply_cache_pull_flags(
      vec!["https://cache.example.com".to_string()],
      vec!["cache.example.com:abc123=".to_string()],
    );

    let generated = args.generate_passthrough_args();
    assert_eq!(generated, [
      "--option",
      "extra-substituters",
      "https://cache.example.com",
      "--option",
      "extra-trusted-public-keys",
      "cache.example.com:abc123=",
    ]);
  }

  #[test]
  fn no_matched_cache_targets_emits_no_pull_flags() {
    let mut args = NixBuildPassthroughArgs::default();
    args.apply_cache_pull_flags(vec![], vec![]);
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

    assert_eq!(args.generate_passthrough_args(), [
      "--override-input",
      "nixpkgs",
      "github:NixOS/nixpkgs/nixos-unstable"
    ]);
  }

  /// SC-012: with nothing configured anywhere, the generated argv must be
  /// identical to what it was before this capability existed. That is what
  /// makes the change inert by default.
  #[test]
  fn nothing_configured_changes_nothing() {
    let args = NixBuildPassthroughArgs {
      connect_timeout: None,
      ..Default::default()
    };
    assert!(args.config_override_input.is_empty());
    assert!(args.generate_passthrough_args().is_empty());
  }

  #[test]
  fn config_derived_override_is_emitted() {
    let mut args = NixBuildPassthroughArgs::default();
    let mut map = std::collections::BTreeMap::new();
    map.insert("nix-lib".to_string(), "path:/abs/nix-lib".to_string());
    args.apply_override_input_defaults(&map);

    assert_eq!(args.generate_passthrough_args(), [
      "--override-input",
      "nix-lib",
      "path:/abs/nix-lib"
    ]);
  }

  /// SC-007: precedence holds per input name.
  #[test]
  fn a_flag_replaces_only_the_input_it_names() {
    let mut args = NixBuildPassthroughArgs {
      override_input: vec!["nix-lib".into(), "path:/from/flag".into()],
      ..Default::default()
    };
    let mut map = std::collections::BTreeMap::new();
    map.insert("nix-lib".to_string(), "path:/from/config".to_string());
    map.insert("nix-vm-test".to_string(), "path:/vm".to_string());
    args.apply_override_input_defaults(&map);

    // nix-lib takes the flag's value, nix-vm-test keeps the configured one.
    assert_eq!(args.generate_passthrough_args(), [
      "--override-input",
      "nix-lib",
      "path:/from/flag",
      "--override-input",
      "nix-vm-test",
      "path:/vm"
    ]);
  }

  /// A shadowed entry must not be emitted twice, so the report cannot
  /// advertise a value that nix's silent last-wins would discard.
  #[test]
  fn a_shadowed_entry_is_emitted_once() {
    let mut args = NixBuildPassthroughArgs {
      override_input: vec!["nix-lib".into(), "path:/winner".into()],
      ..Default::default()
    };
    let mut map = std::collections::BTreeMap::new();
    map.insert("nix-lib".to_string(), "path:/loser".to_string());
    args.apply_override_input_defaults(&map);

    let generated = args.generate_passthrough_args();
    assert_eq!(
      generated
        .iter()
        .filter(|a| *a == "--override-input")
        .count(),
      1
    );
    assert!(!generated.iter().any(|a| a == "path:/loser"));
  }

  #[test]
  fn emitted_override_order_is_deterministic() {
    let mut args = NixBuildPassthroughArgs::default();
    let mut map = std::collections::BTreeMap::new();
    map.insert("z".to_string(), "path:/z".to_string());
    map.insert("a".to_string(), "path:/a".to_string());
    args.apply_override_input_defaults(&map);

    assert_eq!(args.generate_passthrough_args(), [
      "--override-input",
      "a",
      "path:/a",
      "--override-input",
      "z",
      "path:/z"
    ]);
  }
}
