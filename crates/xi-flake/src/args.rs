use std::path::PathBuf;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;
use xi_core::complete;
use xi_core::installable::InstallableArgs;

/// Build backend for `--all` / CI builds.
#[derive(Debug, Clone, Default, clap::ValueEnum)]
pub enum CiBackend {
  /// Auto-detect: use nix-fast-build if available, else devour-flake
  #[default]
  Auto,
  /// Single-evaluation build via devour-flake (always available)
  DevourFlake,
  /// Parallel eval + pipelined builds via nix-fast-build
  NixFastBuild,
}

/// Curated set of nix flags for xi flake commands.
///
/// Only the commonly useful flags are exposed as first-class options.
/// Everything else can be passed via `-- <extra_args>` at the end.
#[derive(Debug, Default, Args)]
pub struct NixPassthroughArgs {
  /// Number of concurrent jobs Nix should run
  #[arg(long, short = 'j', env = "XI_MAX_JOBS")]
  pub max_jobs: Option<usize>,

  /// Continue building despite encountering errors
  #[arg(long, short = 'k', env = "XI_KEEP_GOING", value_parser = clap::builder::BoolishValueParser::new())]
  pub keep_going: bool,

  /// Display tracebacks on errors
  #[arg(long, short = 't', env = "XI_SHOW_TRACE", value_parser = clap::builder::BoolishValueParser::new())]
  pub show_trace: bool,

  /// Allow impure builds
  #[arg(long, env = "XI_IMPURE", value_parser = clap::builder::BoolishValueParser::new())]
  pub impure: bool,

  /// Build without internet access
  #[arg(long, env = "XI_OFFLINE", value_parser = clap::builder::BoolishValueParser::new())]
  pub offline: bool,

  /// Accept configuration from flakes
  #[arg(long, env = "XI_ACCEPT_FLAKE_CONFIG", value_parser = clap::builder::BoolishValueParser::new())]
  pub accept_flake_config: bool,

  /// Refresh flakes to the latest revision
  #[arg(long)]
  pub refresh: bool,

  /// Override a specific flake input (may be given multiple times)
  #[arg(long, number_of_values = 2, value_names = ["INPUT", "FLAKE_URL"])]
  pub override_input: Vec<String>,

  /// Set a Nix configuration option (may be given multiple times)
  #[arg(long, number_of_values = 2, value_names = ["NAME", "VALUE"])]
  pub option: Vec<String>,

  /// Substituter connection timeout in seconds (injected from config).
  /// Not a CLI flag — applied via `apply_build_defaults`.
  #[arg(skip)]
  pub connect_timeout: Option<u64>,
}

impl NixPassthroughArgs {
  #[must_use]
  pub fn to_nix_args(&self) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(jobs) = self.max_jobs {
      args.push("--max-jobs".into());
      args.push(jobs.to_string());
    }
    if self.keep_going {
      args.push("--keep-going".into());
    }
    if self.show_trace {
      args.push("--show-trace".into());
    }
    if self.impure {
      args.push("--impure".into());
    }
    if self.offline {
      args.push("--offline".into());
    }
    if self.accept_flake_config {
      args.push("--accept-flake-config".into());
    }
    if self.refresh {
      args.push("--refresh".into());
    }
    for pair in self.override_input.chunks(2) {
      args.push("--override-input".into());
      args.push(pair[0].clone());
      if pair.len() > 1 {
        args.push(pair[1].clone());
      }
    }
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
      if pair.len() > 1 {
        args.push(pair[1].clone());
      }
    }

    args
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
}

#[derive(Debug, Args)]
/// Build a flake output (defaults to current directory)
pub struct BuildArgs {
  #[command(flatten)]
  pub installable: InstallableArgs,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  /// Path to save the result link
  #[arg(long, short)]
  pub out_link: Option<PathBuf>,

  /// Don't create a result symlink
  #[arg(long)]
  pub no_link: bool,

  /// Only print actions, without performing them
  #[arg(long, short = 'n', alias = "dry-run")]
  pub dry: bool,

  #[command(flatten)]
  pub passthrough: NixPassthroughArgs,

  /// Build all flake outputs (packages, checks, devShells, apps,
  /// nixosConfigurations, darwinConfigurations, homeConfigurations)
  /// using devour-flake for single-evaluation efficiency
  #[arg(long)]
  pub all: bool,

  /// Recursively discover and build all subflakes.
  /// Scans for flake.nix files in subdirectories and builds each.
  /// Implies --all.
  #[arg(long, requires = "all")]
  pub recursive: bool,

  /// Build backend for --all builds
  #[arg(long, value_enum, default_value_t, env = "XI_CI_BACKEND")]
  pub backend: CiBackend,

  /// Extra arguments passed to nix build
  #[arg(last = true)]
  pub extra_args: Vec<String>,

  #[command(flatten)]
  pub cache: xi_core::args::CacheArgs,
}

#[derive(Debug, Args)]
/// Check a flake for errors (defaults to current directory)
///
/// Without arguments, checks all flake outputs.
/// With a name, builds the specific check (e.g. `xi check xi` builds
/// `.#checks.<system>.xi`).
pub struct CheckArgs {
  /// Check name or flake reference.
  /// A bare name like `xi` builds `.#checks.<system>.xi`.
  /// A path like `../other` checks that flake entirely.
  #[arg(add = ArgValueCompleter::new(complete::complete_checks))]
  pub target: Option<String>,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  #[command(flatten)]
  pub passthrough: NixPassthroughArgs,

  /// Extra arguments passed to nix flake check
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

#[derive(Debug, Args)]
/// Run a flake app (defaults to current directory)
///
/// With `--locate` / `-l`, searches nixpkgs for a package providing the
/// given command (comma-style), builds it, then executes it directly.
pub struct RunArgs {
  #[command(flatten)]
  pub installable: InstallableArgs,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  /// Locate mode: search nixpkgs for a package providing the command
  /// via nix-index, build it, then run the binary directly.
  /// The installable argument becomes the command name.
  #[arg(long, short = 'l', env = "XI_RUN_LOCATE", value_parser = clap::builder::BoolishValueParser::new())]
  pub locate: bool,

  /// (locate mode) Open a nix shell with the package instead of running
  #[arg(long, requires = "locate")]
  pub shell: bool,

  /// (locate mode) Install the package into your nix profile
  #[arg(long, requires = "locate")]
  pub install: bool,

  /// (locate mode) Cache level: 0=disabled, 1=choice only, 2=full (default)
  #[arg(
    long,
    env = "XI_LOCATE_CACHE",
    value_name = "LEVEL",
    requires = "locate"
  )]
  pub cache_level: Option<u8>,

  #[command(flatten)]
  pub passthrough: NixPassthroughArgs,

  /// Extra arguments passed to the program
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

/// Formatter backend for `xi fmt`.
///
/// Well-known values: `auto`, `flake`, `nixfmt`, `alejandra`, `pedantix`.
/// Any other value is treated as an external command that receives `.nix`
/// file paths as arguments (Cargo-style extensibility).
#[derive(Debug, Clone)]
pub struct FmtBackend(pub String);

impl FmtBackend {
  pub const AUTO: &str = "auto";
  pub const FLAKE: &str = "flake";

  pub fn is_auto(&self) -> bool {
    self.0 == Self::AUTO
  }

  pub fn is_flake(&self) -> bool {
    self.0 == Self::FLAKE
  }

  /// Well-known backends suggested in completions.
  const WELL_KNOWN: &[&str] =
    &["auto", "flake", "nixfmt", "alejandra", "pedantix"];
}

impl Default for FmtBackend {
  fn default() -> Self {
    Self(Self::AUTO.to_string())
  }
}

impl std::fmt::Display for FmtBackend {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.0)
  }
}

impl From<String> for FmtBackend {
  fn from(s: String) -> Self {
    Self(s)
  }
}

impl clap::builder::ValueParserFactory for FmtBackend {
  type Parser = FmtBackendParser;

  fn value_parser() -> Self::Parser {
    FmtBackendParser
  }
}

#[derive(Clone, Debug)]
pub struct FmtBackendParser;

impl clap::builder::TypedValueParser for FmtBackendParser {
  type Value = FmtBackend;

  fn parse_ref(
    &self,
    _cmd: &clap::Command,
    _arg: Option<&clap::Arg>,
    value: &std::ffi::OsStr,
  ) -> Result<Self::Value, clap::Error> {
    let s = value
      .to_str()
      .ok_or_else(|| clap::Error::new(clap::error::ErrorKind::InvalidUtf8))?;
    Ok(FmtBackend(s.to_string()))
  }

  fn possible_values(
    &self,
  ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
    Some(Box::new(
      FmtBackend::WELL_KNOWN
        .iter()
        .map(|v| clap::builder::PossibleValue::new(*v)),
    ))
  }
}

#[derive(Debug, Args)]
/// Format source code using the flake's formatter (defaults to current directory)
///
/// Auto-detects the formatter: uses the flake's formatter output if declared,
/// otherwise falls back to nixfmt. Override with --backend or .xi.toml \[fmt\].
/// Any command on PATH can be used as a backend (e.g. pedantix, nixfmt, alejandra).
pub struct FmtArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Formatter backend to use (any command on PATH, or "auto"/"flake")
  #[arg(long, default_value_t, env = "XI_FMT_BACKEND")]
  pub backend: FmtBackend,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  #[command(flatten)]
  pub passthrough: NixPassthroughArgs,

  /// Extra arguments passed to the formatter
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

#[derive(Debug, Args)]
/// Show the outputs of a flake (defaults to current directory)
pub struct ShowArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Output results in JSON format
  #[arg(long)]
  pub json: bool,

  /// Use raw nix flake show output instead of the compact view
  #[arg(long)]
  pub raw: bool,

  /// Show all outputs including internal/opaque ones (debug, modules, etc.)
  #[arg(long, short = 'a')]
  pub all: bool,

  /// Display tracebacks on errors
  #[arg(long, short = 't', env = "XI_SHOW_TRACE", value_parser = clap::builder::BoolishValueParser::new())]
  pub show_trace: bool,

  /// Extra arguments passed to nix flake show
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

#[derive(Debug, Args)]
/// Update flake inputs (defaults to current directory)
///
/// Without arguments, updates all inputs.
/// With input names, updates only those specific inputs.
pub struct UpdateArgs {
  /// Flake reference (defaults to current directory)
  #[arg(long, short = 'f')]
  pub flake: Option<String>,

  /// Input names to update (updates all if none specified)
  #[arg(add = ArgValueCompleter::new(complete::complete_flake_inputs))]
  pub inputs: Vec<String>,

  /// Commit the updated lock file
  #[arg(long)]
  pub commit_lock_file: bool,

  #[command(flatten)]
  pub passthrough: NixPassthroughArgs,

  /// Extra arguments passed to nix flake update
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

#[derive(Debug, Args)]
/// Initialize a flake in the current directory from a template
pub struct InitArgs {
  /// Template flake reference (e.g. templates#full, github:user/repo)
  #[arg(short = 'T', long)]
  pub template: Option<String>,

  /// Extra arguments passed to nix flake init
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

#[derive(Debug, Args)]
/// Check flake health: input freshness, nixpkgs branch, source, formatter
///
/// Analyzes flake.lock to detect stale inputs, unsupported nixpkgs branches,
/// unofficial forks, and missing formatter declarations.
/// Configure thresholds via .xi.toml \[doctor\] section.
pub struct DoctorArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,
}

#[derive(Debug, Args)]
/// Run CI pipeline: validate, then build all flake outputs
///
/// Phase 1 (parallel): lock check, eval all systems, lib eval, format check.
/// Phase 2 (sequential): build all outputs with devour-flake,
/// then build extra outputs from .xi.toml.
pub struct CiArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Skip lock file verification
  #[arg(long)]
  pub no_lock_check: bool,

  /// Skip all-systems evaluation
  #[arg(long)]
  pub no_eval: bool,

  /// Skip building (validation-only mode)
  #[arg(long)]
  pub no_build: bool,

  /// Disallow import-from-derivation during evaluation
  #[arg(long)]
  pub no_ifd: bool,

  /// Only eval/build for the current system
  #[arg(long)]
  pub current_system_only: bool,

  /// Recursively discover and validate subflakes
  #[arg(long)]
  pub recursive: bool,

  /// Skip flake health checks (input freshness, branch, source)
  #[arg(long)]
  pub no_health_check: bool,

  /// Skip eval-time tests (runTests) during validation
  #[arg(long)]
  pub no_test: bool,

  /// Skip deep evaluation of lib outputs
  #[arg(long)]
  pub no_lib_eval: bool,

  /// Don't use nix-output-monitor for the build process
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  /// Only print actions, without performing them
  #[arg(long, short = 'n', alias = "dry-run")]
  pub dry: bool,

  /// Continue running CI steps after a failure instead of stopping
  #[arg(long = "continue-on-error")]
  pub continue_on_error: bool,

  /// Build backend for the build phase
  #[arg(long, value_enum, default_value_t, env = "XI_CI_BACKEND")]
  pub backend: CiBackend,

  #[command(flatten)]
  pub passthrough: NixPassthroughArgs,

  /// Extra arguments passed to nix
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

#[derive(Debug, Args)]
/// Inspect or evaluate flake lib outputs
///
/// Lists all attributes under the flake's `lib` output recursively.
/// Use --eval to deeply evaluate them and catch errors.
pub struct LibArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Deeply evaluate all lib attrs (catches type errors, missing attrs, infinite recursion)
  #[arg(long, short = 'E')]
  pub eval: bool,

  /// Display tracebacks on errors
  #[arg(long, short = 't', env = "XI_SHOW_TRACE", value_parser = clap::builder::BoolishValueParser::new())]
  pub show_trace: bool,
}

/// Output format for `xi test`.
#[derive(Debug, Clone, Default, clap::ValueEnum)]
pub enum TestFormat {
  /// Colored human-readable output
  #[default]
  Pretty,
  /// Machine-readable JSON output
  Json,
}

/// Well-known test backends for `xi test`.
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum TestBackend {
  /// Eval-time tests via `lib.runTests` (`{expected, expr}` pattern)
  RunTests,
  /// Build check derivations under `checks.<system>.*`
  Checks,
  /// Standalone nix-unit CLI
  NixUnit,
  /// Standalone nixt CLI
  Nixt,
  /// Snapshot testing with namaka
  Namaka,
}

#[derive(Debug, Args)]
/// Run Nix code tests across multiple backends
///
/// Auto-detects available test backends (runTests eval, checks build,
/// nix-unit, nixt, namaka) and runs them. Configure via .xi.toml [test].
pub struct TestArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Only run specific backends (can be repeated)
  #[arg(long, value_enum)]
  pub backend: Vec<TestBackend>,

  /// Filter test names (glob pattern)
  #[arg(long, short = 'F')]
  pub filter: Option<String>,

  /// Don't use nix-output-monitor for check builds
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new())]
  pub no_nom: bool,

  /// Output format
  #[arg(long, value_enum, default_value_t)]
  pub format: TestFormat,

  /// List detected tests without running them
  #[arg(long, short = 'l')]
  pub list: bool,

  /// Interactive snapshot review mode (namaka only)
  #[arg(long)]
  pub review: bool,

  /// Watch for .nix file changes and re-run tests
  #[arg(long, short = 'w')]
  pub watch: bool,

  #[command(flatten)]
  pub passthrough: NixPassthroughArgs,

  /// Extra arguments passed to nix
  #[arg(last = true)]
  pub extra_args: Vec<String>,
}

#[derive(Debug, Args)]
/// Materialize cached outputs from .xi.toml targets
///
/// Runs configured commands to produce output files, caching results
/// based on source file hashes. Configure targets in .xi.toml:
///
///   [[materialize.target]]
///   name = "cargo-hash"
///   command = "nix eval .#cargoHash --json"
///   output = "cargo-hash.json"
///   sources = \[`Cargo.lock`\]
pub struct MaterializeArgs {
  /// Flake reference (defaults to current directory)
  pub flake_ref: Option<String>,

  /// Only run specific target(s) by name
  pub targets: Vec<String>,

  /// Commit materialized files to git (writes to nix/materialized/)
  #[arg(long)]
  pub commit: bool,

  /// Verify all targets are fresh (exit 1 if stale)
  #[arg(long)]
  pub check: bool,

  /// List configured targets and their staleness
  #[arg(long, short = 'l')]
  pub list: bool,

  /// Re-run all targets, ignoring freshness cache
  #[arg(long)]
  pub force: bool,

  /// Set up git to hide materialized files from git status
  /// (applies skip-worktree + .gitattributes merge driver)
  #[arg(long)]
  pub setup: bool,

  /// Remove the .xi/materialized/ cache directory
  #[arg(long)]
  pub clean: bool,
}
