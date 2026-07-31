//! Nix CLI proxy — intercepts known `nix` subcommands and routes them to
//! xi's enhanced implementations, falling back to raw `nix` for everything
//! else.
//!
//! Usage: `xi nix build .#hello` or via alias `alias nix="xi nix"`

use std::process::Stdio;

use color_eyre::Result;
use tracing::{debug, info};

/// Raw arguments captured from `xi nix <args...>`.
#[derive(Debug, clap::Args)]
/// Nix CLI proxy — enhanced UX for known commands, passthrough for the rest
pub struct NixProxyArgs {
  /// Bypass xi enhancements (nom, prechecks, etc.) and run the bare nix CLI.
  /// Can also be set via `XI_UNWRAP=1` environment variable.
  #[arg(long, alias = "raw")]
  pub unwrap: bool,

  /// Arguments to pass to nix (or intercept for enhanced UX)
  #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
  pub args: Vec<String>,
}

/// The result of analyzing the nix CLI arguments.
#[derive(Debug)]
enum Route {
  /// Route to xi's enhanced build implementation
  Build { args: Vec<String> },
  /// Route to xi's enhanced check implementation
  Check { args: Vec<String> },
  /// Route to xi's enhanced fmt implementation
  Fmt { args: Vec<String> },
  /// Route to xi's enhanced show implementation
  Show { args: Vec<String> },
  /// Route to xi's enhanced develop implementation
  Develop { args: Vec<String> },
  /// Route to xi's enhanced run implementation
  Run { args: Vec<String> },
  /// Route to xi's enhanced init implementation
  Init { args: Vec<String> },
  /// Route to xi's enhanced update implementation
  Update { args: Vec<String> },
  /// Passthrough: run `nix` directly with all arguments
  Passthrough { args: Vec<String> },
}

impl NixProxyArgs {
  /// Run the proxy, routing to enhanced xi implementations or falling back
  /// to raw nix.
  ///
  /// # Errors
  ///
  /// Returns an error if the routed command fails.
  pub fn run(self) -> Result<()> {
    let unwrap = self.unwrap
      || std::env::var("XI_UNWRAP").is_ok_and(|v| v == "1" || v == "true");

    if unwrap {
      debug!("--unwrap: bypassing xi enhancements, passing through to nix");
      return run_nix_passthrough(&self.args);
    }

    let route = analyze_args(&self.args);

    match route {
      Route::Build { args } => {
        info!("nix build → xi build");
        run_xi_command("build", &args)
      },
      Route::Check { args } => {
        info!("nix flake check → xi check");
        run_xi_command("check", &args)
      },
      Route::Fmt { args } => {
        info!("nix fmt → xi fmt");
        run_xi_command("fmt", &args)
      },
      Route::Show { args } => {
        info!("nix flake show → xi show");
        run_xi_command("show", &args)
      },
      Route::Develop { args } => {
        info!("nix develop → xi develop");
        run_xi_command("develop", &args)
      },
      Route::Run { args } => {
        info!("nix run → xi run");
        run_xi_command("run", &args)
      },
      Route::Init { args } => {
        info!("nix flake init → xi init");
        run_xi_command("init", &args)
      },
      Route::Update { args } => {
        info!("nix flake update → xi update");
        run_xi_command("update", &args)
      },
      Route::Passthrough { args } => {
        debug!("Passing through to nix: {:?}", args);
        run_nix_passthrough(&args)
      },
    }
  }
}

/// Analyze the nix CLI arguments and determine the route.
///
/// Handles both top-level commands (`nix build`, `nix fmt`) and nested
/// flake subcommands (`nix flake check`, `nix flake show`).
///
/// Global flags appearing before the subcommand are preserved and
/// prepended to the routed command's arguments.
fn analyze_args(args: &[String]) -> Route {
  let mut i = 0;
  let mut global_prefix: Vec<String> = Vec::new();

  while i < args.len() {
    let arg = &args[i];

    // Try to match a long flag (--name)
    if let Some(flag_name) = arg.strip_prefix("--") {
      let arity = resolve_global_flag_arity(flag_name);
      global_prefix.push(arg.clone());
      let skip = arity as usize;
      for j in 1..=skip {
        if i + j < args.len() {
          global_prefix.push(args[i + j].clone());
        }
      }
      i += 1 + skip;
      continue;
    }

    // Short flag (single dash) — treat as arity 0
    if arg.starts_with('-') {
      global_prefix.push(arg.clone());
      i += 1;
      continue;
    }

    // Found the subcommand
    let rest: Vec<String> = args[i + 1..].to_vec();
    let combined = combine_global_and_rest(&global_prefix, &rest);

    return match arg.as_str() {
      "build" => Route::Build { args: combined },
      "develop" => Route::Develop { args: combined },
      "fmt" => Route::Fmt { args: combined },
      "run" => Route::Run { args: combined },
      "flake" => analyze_flake_subcommand(&global_prefix, &rest),
      _ => Route::Passthrough {
        args: args.to_vec(),
      },
    };
  }

  // No subcommand found — passthrough (e.g., `nix --version`)
  Route::Passthrough {
    args: args.to_vec(),
  }
}

/// Resolve the arity of a global nix flag.
///
/// Uses the build-time generated schema when available, falling back to a
/// hardcoded list of the most common flags that take values.
fn resolve_global_flag_arity(flag_name: &str) -> u8 {
  if nix_command::schema::SCHEMA_AVAILABLE {
    return nix_command::schema::global_flag_arity(flag_name).unwrap_or(0);
  }

  // Legacy fallback when schema was not generated at build time
  match flag_name {
    "option" => 2,
    "extra-experimental-features"
    | "log-format"
    | "builders"
    | "max-jobs"
    | "cores"
    | "store"
    | "eval-store"
    | "system"
    | "access-tokens"
    | "extra-access-tokens"
    | "extra-substituters"
    | "extra-trusted-public-keys"
    | "extra-nix-path"
    | "extra-platforms"
    | "extra-sandbox-paths" => 1,
    _ => 0,
  }
}

/// Combine collected global flags with the per-command args.
fn combine_global_and_rest(global: &[String], rest: &[String]) -> Vec<String> {
  let mut combined = Vec::with_capacity(global.len() + rest.len());
  combined.extend_from_slice(global);
  combined.extend_from_slice(rest);
  combined
}

/// Analyze `nix flake <subcommand>` arguments.
fn analyze_flake_subcommand(
  global_prefix: &[String],
  args: &[String],
) -> Route {
  // Find the flake subcommand, skipping any flags between `flake` and the
  // subcommand name.
  let mut i = 0;
  while i < args.len() {
    let arg = &args[i];

    if arg.starts_with('-') {
      i += 1;
      continue;
    }

    let rest: Vec<String> = args[i + 1..].to_vec();
    let combined = combine_global_and_rest(global_prefix, &rest);

    return match arg.as_str() {
      "check" => Route::Check { args: combined },
      "show" => Route::Show { args: combined },
      "init" => Route::Init { args: combined },
      "update" => Route::Update { args: combined },
      _ => {
        // Unknown flake subcommand (lock, metadata, etc.)
        // Reconstruct: flake <subcommand> <rest>
        let mut full = vec!["flake".to_string()];
        full.extend(args.iter().cloned());
        Route::Passthrough { args: full }
      },
    };
  }

  // `nix flake` with no subcommand
  let mut full = vec!["flake".to_string()];
  full.extend(args.iter().cloned());
  Route::Passthrough { args: full }
}

/// Re-invoke `xi` with the enhanced command, passing remaining args.
fn run_xi_command(subcommand: &str, args: &[String]) -> Result<()> {
  let current_exe = std::env::current_exe()
    .map_err(|e| color_eyre::eyre::eyre!("Failed to get current exe: {e}"))?;

  debug!(
    "Re-invoking: {} {} {}",
    current_exe.display(),
    subcommand,
    args.join(" ")
  );

  let status = std::process::Command::new(&current_exe)
    .arg(subcommand)
    .args(args)
    .stdin(Stdio::inherit())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .status()
    .map_err(|e| {
      color_eyre::eyre::eyre!("Failed to run xi {subcommand}: {e}")
    })?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  Ok(())
}

/// Run `nix` directly with the given arguments (passthrough).
///
/// Uses `nix_command::find_real_nix_binary()` to skip shell-script wrappers
/// (like xi's own nix-alias) and find the real nix ELF binary, preventing
/// infinite recursion.
fn run_nix_passthrough(args: &[String]) -> Result<()> {
  let nix_bin = nix_command::find_real_nix_binary();
  debug!("Running: {:?} {}", nix_bin, args.join(" "));

  let status = std::process::Command::new(&nix_bin)
    .args(args)
    .stdin(Stdio::inherit())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .status()
    .map_err(|e| color_eyre::eyre::eyre!("Failed to run nix: {e}"))?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
  }

  #[test]
  fn build_is_intercepted() {
    let route = analyze_args(&args("build .#hello"));
    assert!(matches!(route, Route::Build { .. }));
    if let Route::Build { args: a } = route {
      assert_eq!(a, vec![".#hello"]);
    }
  }

  #[test]
  fn build_bare_is_intercepted() {
    let route = analyze_args(&args("build"));
    assert!(matches!(route, Route::Build { args: a } if a.is_empty()));
  }

  #[test]
  fn develop_is_intercepted() {
    let route = analyze_args(&args("develop"));
    assert!(matches!(route, Route::Develop { .. }));
  }

  #[test]
  fn fmt_is_intercepted() {
    let route = analyze_args(&args("fmt"));
    assert!(matches!(route, Route::Fmt { .. }));
  }

  #[test]
  fn run_is_intercepted() {
    let route = analyze_args(&args("run .#hello"));
    assert!(matches!(route, Route::Run { .. }));
    if let Route::Run { args: a } = route {
      assert_eq!(a, vec![".#hello"]);
    }
  }

  #[test]
  fn flake_check_is_intercepted() {
    let route = analyze_args(&args("flake check"));
    assert!(matches!(route, Route::Check { .. }));
  }

  #[test]
  fn flake_check_with_ref_is_intercepted() {
    let route = analyze_args(&args("flake check ."));
    assert!(matches!(route, Route::Check { .. }));
    if let Route::Check { args: a } = route {
      assert_eq!(a, vec!["."]);
    }
  }

  #[test]
  fn flake_show_is_intercepted() {
    let route = analyze_args(&args("flake show"));
    assert!(matches!(route, Route::Show { .. }));
  }

  #[test]
  fn flake_init_is_intercepted() {
    let route = analyze_args(&args("flake init"));
    assert!(matches!(route, Route::Init { .. }));
  }

  #[test]
  fn flake_update_is_intercepted() {
    let route = analyze_args(&args("flake update"));
    assert!(matches!(route, Route::Update { .. }));
  }

  #[test]
  fn flake_update_with_input_is_intercepted() {
    let route = analyze_args(&args("flake update nixpkgs"));
    assert!(matches!(route, Route::Update { .. }));
    if let Route::Update { args: a } = route {
      assert_eq!(a, vec!["nixpkgs"]);
    }
  }

  #[test]
  fn flake_lock_is_passthrough() {
    let route = analyze_args(&args("flake lock"));
    assert!(matches!(route, Route::Passthrough { .. }));
  }

  #[test]
  fn eval_is_passthrough() {
    let route = analyze_args(&args("eval .#foo"));
    assert!(matches!(route, Route::Passthrough { .. }));
    if let Route::Passthrough { args: a } = route {
      assert_eq!(a, vec!["eval", ".#foo"]);
    }
  }

  #[test]
  fn store_is_passthrough() {
    let route = analyze_args(&args("store gc"));
    assert!(matches!(route, Route::Passthrough { .. }));
  }

  #[test]
  fn copy_is_passthrough() {
    let route = analyze_args(&args("copy --to ssh://host /nix/store/foo"));
    assert!(matches!(route, Route::Passthrough { .. }));
  }

  #[test]
  fn version_flag_is_passthrough() {
    let route = analyze_args(&args("--version"));
    assert!(matches!(route, Route::Passthrough { .. }));
  }

  #[test]
  fn empty_is_passthrough() {
    let route = analyze_args(&[]);
    assert!(matches!(route, Route::Passthrough { .. }));
  }

  #[test]
  fn build_with_global_flags_preserved() {
    let route =
      analyze_args(&args("--extra-experimental-features flakes build .#hello"));
    assert!(matches!(route, Route::Build { .. }));
    if let Route::Build { args: a } = route {
      assert_eq!(
        a,
        vec!["--extra-experimental-features", "flakes", ".#hello"]
      );
    }
  }

  #[test]
  fn build_with_flags_after() {
    let route = analyze_args(&args("build .#hello --no-link --json"));
    assert!(matches!(route, Route::Build { .. }));
    if let Route::Build { args: a } = route {
      assert_eq!(a, vec![".#hello", "--no-link", "--json"]);
    }
  }

  #[test]
  fn global_boolean_flag_preserved() {
    let route = analyze_args(&args("--offline build .#hello"));
    assert!(matches!(route, Route::Build { .. }));
    if let Route::Build { args: a } = route {
      assert_eq!(a, vec!["--offline", ".#hello"]);
    }
  }

  #[test]
  fn global_arity2_flag_preserved() {
    let route = analyze_args(&args("--option sandbox false build .#hello"));
    assert!(matches!(route, Route::Build { .. }));
    if let Route::Build { args: a } = route {
      assert_eq!(a, vec!["--option", "sandbox", "false", ".#hello"]);
    }
  }

  #[test]
  fn multiple_global_flags_preserved() {
    let route =
      analyze_args(&args("--offline --max-jobs 4 --show-trace build .#hello"));
    assert!(matches!(route, Route::Build { .. }));
    if let Route::Build { args: a } = route {
      assert_eq!(
        a,
        vec!["--offline", "--max-jobs", "4", "--show-trace", ".#hello"]
      );
    }
  }

  #[test]
  fn global_flags_preserved_for_flake_check() {
    let route = analyze_args(&args("--offline flake check ."));
    assert!(matches!(route, Route::Check { .. }));
    if let Route::Check { args: a } = route {
      assert_eq!(a, vec!["--offline", "."]);
    }
  }

  #[test]
  fn unwrap_flag_parsed_by_clap() {
    use clap::Parser;

    // Simulate: xi nix --unwrap build .#hello
    #[derive(clap::Parser)]
    struct Cli {
      #[command(subcommand)]
      cmd: Cmd,
    }
    #[derive(clap::Subcommand)]
    enum Cmd {
      Nix(NixProxyArgs),
    }

    let cli = Cli::parse_from(["xi", "nix", "--unwrap", "build", ".#hello"]);
    let Cmd::Nix(proxy) = cli.cmd;
    assert!(proxy.unwrap);
    assert_eq!(proxy.args, vec!["build", ".#hello"]);
  }

  #[test]
  fn raw_alias_parsed_by_clap() {
    use clap::Parser;

    #[derive(clap::Parser)]
    struct Cli {
      #[command(subcommand)]
      cmd: Cmd,
    }
    #[derive(clap::Subcommand)]
    enum Cmd {
      Nix(NixProxyArgs),
    }

    let cli = Cli::parse_from(["xi", "nix", "--raw", "build", ".#hello"]);
    let Cmd::Nix(proxy) = cli.cmd;
    assert!(proxy.unwrap);
    assert_eq!(proxy.args, vec!["build", ".#hello"]);
  }

  // ── Schema-driven e2e tests ──────────────────────────────────────
  //
  // These tests exercise real nix flag combinations against the
  // schema-driven proxy parser. They verify that flags of every arity
  // are correctly parsed, preserved, and forwarded to the right route.
  // All tests are pure Rust — no CLI execution needed.

  /// Helper: extract args from a Route, panicking on wrong variant.
  fn expect_build(route: Route) -> Vec<String> {
    match route {
      Route::Build { args } => args,
      other => panic!("expected Route::Build, got {other:?}"),
    }
  }

  fn expect_develop(route: Route) -> Vec<String> {
    match route {
      Route::Develop { args } => args,
      other => panic!("expected Route::Develop, got {other:?}"),
    }
  }

  fn expect_check(route: Route) -> Vec<String> {
    match route {
      Route::Check { args } => args,
      other => panic!("expected Route::Check, got {other:?}"),
    }
  }

  fn expect_update(route: Route) -> Vec<String> {
    match route {
      Route::Update { args } => args,
      other => panic!("expected Route::Update, got {other:?}"),
    }
  }

  fn expect_passthrough(route: Route) -> Vec<String> {
    match route {
      Route::Passthrough { args } => args,
      other => panic!("expected Route::Passthrough, got {other:?}"),
    }
  }

  // ── Arity-0 (boolean) global flags ─────────────────────────────

  #[test]
  fn e2e_arity0_show_trace_before_build() {
    let a = expect_build(analyze_args(&args("--show-trace build .")));
    assert_eq!(a, vec!["--show-trace", "."]);
  }

  #[test]
  fn e2e_arity0_keep_going_before_develop() {
    let a = expect_develop(analyze_args(&args("--keep-going develop")));
    assert_eq!(a, vec!["--keep-going"]);
  }

  #[test]
  fn e2e_arity0_verbose_and_offline() {
    let a =
      expect_build(analyze_args(&args("--verbose --offline build .#pkg")));
    assert_eq!(a, vec!["--verbose", "--offline", ".#pkg"]);
  }

  #[test]
  fn e2e_arity0_accept_flake_config() {
    let a = expect_build(analyze_args(&args("--accept-flake-config build .")));
    assert_eq!(a, vec!["--accept-flake-config", "."]);
  }

  #[test]
  fn e2e_arity0_refresh() {
    let a = expect_build(analyze_args(&args("--refresh build .#hello")));
    assert_eq!(a, vec!["--refresh", ".#hello"]);
  }

  #[test]
  fn e2e_arity0_no_negation_flags() {
    let a = expect_build(analyze_args(&args(
      "--no-keep-going --no-show-trace build .",
    )));
    assert_eq!(a, vec!["--no-keep-going", "--no-show-trace", "."]);
  }

  // ── Arity-1 (single value) global flags ────────────────────────

  #[test]
  fn e2e_arity1_max_jobs() {
    let a = expect_build(analyze_args(&args("--max-jobs 8 build .")));
    assert_eq!(a, vec!["--max-jobs", "8", "."]);
  }

  #[test]
  fn e2e_arity1_cores() {
    let a = expect_build(analyze_args(&args("--cores 4 build .#hello")));
    assert_eq!(a, vec!["--cores", "4", ".#hello"]);
  }

  #[test]
  fn e2e_arity1_log_format() {
    let a = expect_build(analyze_args(&args("--log-format bar build .")));
    assert_eq!(a, vec!["--log-format", "bar", "."]);
  }

  #[test]
  fn e2e_arity1_builders() {
    let a =
      expect_build(analyze_args(&args("--builders ssh://builder build .")));
    assert_eq!(a, vec!["--builders", "ssh://builder", "."]);
  }

  #[test]
  fn e2e_arity1_store() {
    let a = expect_build(analyze_args(&args("--store /tmp/nix-store build .")));
    assert_eq!(a, vec!["--store", "/tmp/nix-store", "."]);
  }

  #[test]
  fn e2e_arity1_extra_substituters() {
    let a = expect_build(analyze_args(&args(
      "--extra-substituters https://cache.example.com build .",
    )));
    assert_eq!(
      a,
      vec!["--extra-substituters", "https://cache.example.com", "."]
    );
  }

  #[test]
  fn e2e_arity1_extra_experimental_features() {
    let a = expect_build(analyze_args(&args(
      "--extra-experimental-features nix-command build .",
    )));
    assert_eq!(a, vec!["--extra-experimental-features", "nix-command", "."]);
  }

  #[test]
  fn e2e_arity1_system() {
    let a = expect_build(analyze_args(&args("--system x86_64-linux build .")));
    assert_eq!(a, vec!["--system", "x86_64-linux", "."]);
  }

  #[test]
  fn e2e_arity1_access_tokens() {
    let a = expect_build(analyze_args(&args(
      "--access-tokens github.com=ghp_xxx build .",
    )));
    assert_eq!(a, vec!["--access-tokens", "github.com=ghp_xxx", "."]);
  }

  // ── Arity-2 (pair) global flags ────────────────────────────────

  #[test]
  fn e2e_arity2_option() {
    let a =
      expect_build(analyze_args(&args("--option sandbox false build .#hello")));
    assert_eq!(a, vec!["--option", "sandbox", "false", ".#hello"]);
  }

  #[test]
  fn e2e_arity2_option_multiple() {
    let a = expect_build(analyze_args(&args(
      "--option sandbox false --option cores 4 build .",
    )));
    assert_eq!(
      a,
      vec![
        "--option", "sandbox", "false", "--option", "cores", "4", "."
      ]
    );
  }

  // ── Mixed arities ──────────────────────────────────────────────

  #[test]
  fn e2e_mixed_arities_before_build() {
    let a = expect_build(analyze_args(&args(
      "--offline --max-jobs 4 --option sandbox false --show-trace build .#hello",
    )));
    assert_eq!(
      a,
      vec![
        "--offline",
        "--max-jobs",
        "4",
        "--option",
        "sandbox",
        "false",
        "--show-trace",
        ".#hello"
      ]
    );
  }

  #[test]
  fn e2e_mixed_arities_before_develop() {
    let a = expect_develop(analyze_args(&args(
      "--show-trace --max-jobs 2 develop .#devShells.x86_64-linux.default",
    )));
    assert_eq!(
      a,
      vec![
        "--show-trace",
        "--max-jobs",
        "2",
        ".#devShells.x86_64-linux.default"
      ]
    );
  }

  // ── Flake subcommand routing with globals ──────────────────────

  #[test]
  fn e2e_global_flags_with_flake_show() {
    let route = analyze_args(&args("--offline flake show ."));
    match route {
      Route::Show { args: a } => {
        assert_eq!(a, vec!["--offline", "."]);
      },
      other => panic!("expected Route::Show, got {other:?}"),
    }
  }

  #[test]
  fn e2e_global_flags_with_flake_init() {
    let route =
      analyze_args(&args("--show-trace flake init --template templates#full"));
    match route {
      Route::Init { args: a } => {
        assert_eq!(a, vec!["--show-trace", "--template", "templates#full"]);
      },
      other => panic!("expected Route::Init, got {other:?}"),
    }
  }

  #[test]
  fn e2e_global_flags_with_flake_update() {
    let a =
      expect_update(analyze_args(&args("--max-jobs 8 flake update nixpkgs")));
    assert_eq!(a, vec!["--max-jobs", "8", "nixpkgs"]);
  }

  #[test]
  fn e2e_global_flags_with_flake_check() {
    let a = expect_check(analyze_args(&args(
      "--show-trace --keep-going flake check .",
    )));
    assert_eq!(a, vec!["--show-trace", "--keep-going", "."]);
  }

  // ── Passthrough commands preserve all args ─────────────────────

  #[test]
  fn e2e_eval_passthrough_preserves_everything() {
    let a =
      expect_passthrough(analyze_args(&args("--show-trace eval --json .#foo")));
    // Passthrough routes preserve ALL original args as-is
    assert_eq!(a, vec!["--show-trace", "eval", "--json", ".#foo"]);
  }

  #[test]
  fn e2e_store_passthrough_preserves_everything() {
    let a = expect_passthrough(analyze_args(&args("--offline store gc")));
    assert_eq!(a, vec!["--offline", "store", "gc"]);
  }

  #[test]
  fn e2e_flake_lock_passthrough_preserves_everything() {
    let a = expect_passthrough(analyze_args(&args(
      "flake lock --update-input nixpkgs",
    )));
    assert_eq!(a, vec!["flake", "lock", "--update-input", "nixpkgs"]);
  }

  // ── Short flags ────────────────────────────────────────────────

  #[test]
  fn e2e_short_flag_v() {
    let a = expect_build(analyze_args(&args("-v build .")));
    assert_eq!(a, vec!["-v", "."]);
  }

  #[test]
  fn e2e_short_flag_print_build_logs() {
    let a = expect_build(analyze_args(&args("-L build .#hello")));
    assert_eq!(a, vec!["-L", ".#hello"]);
  }

  #[test]
  fn e2e_multiple_short_flags() {
    let a = expect_build(analyze_args(&args("-v -L build .#hello")));
    assert_eq!(a, vec!["-v", "-L", ".#hello"]);
  }

  // ── Edge cases ─────────────────────────────────────────────────

  #[test]
  fn e2e_no_flags_just_subcommand() {
    let a = expect_build(analyze_args(&args("build")));
    assert!(a.is_empty());
  }

  #[test]
  fn e2e_flags_only_after_subcommand_not_global() {
    // Flags after the subcommand are rest args, not global
    let a = expect_build(analyze_args(&args("build .#hello --no-link --json")));
    assert_eq!(a, vec![".#hello", "--no-link", "--json"]);
  }

  #[test]
  fn e2e_global_and_subcommand_flags_combined() {
    let a = expect_build(analyze_args(&args(
      "--offline --max-jobs 4 build .#hello --no-link --json",
    )));
    assert_eq!(
      a,
      vec![
        "--offline",
        "--max-jobs",
        "4",
        ".#hello",
        "--no-link",
        "--json"
      ]
    );
  }

  #[test]
  fn e2e_unknown_global_flag_treated_as_boolean() {
    // Unknown flags default to arity 0 (boolean)
    let a = expect_build(analyze_args(&args("--some-future-flag build .")));
    assert_eq!(a, vec!["--some-future-flag", "."]);
  }

  // ── Schema consistency checks ──────────────────────────────────

  #[test]
  fn e2e_schema_knows_common_arity1_flags() {
    if !nix_command::schema::SCHEMA_AVAILABLE {
      return;
    }
    // These flags all take exactly one value
    for flag in [
      "max-jobs",
      "cores",
      "log-format",
      "builders",
      "store",
      "system",
      "access-tokens",
      "timeout",
    ] {
      assert_eq!(
        nix_command::schema::global_flag_arity(flag),
        Some(1),
        "expected arity 1 for --{flag}"
      );
    }
  }

  #[test]
  fn e2e_schema_knows_common_arity0_flags() {
    if !nix_command::schema::SCHEMA_AVAILABLE {
      return;
    }
    // Note: repair is a per-command flag, not a global flag
    for flag in [
      "offline",
      "refresh",
      "verbose",
      "debug",
      "keep-going",
      "keep-failed",
      "fallback",
      "show-trace",
      "print-build-logs",
      "quiet",
      "accept-flake-config",
      "no-accept-flake-config",
    ] {
      assert_eq!(
        nix_command::schema::global_flag_arity(flag),
        Some(0),
        "expected arity 0 for --{flag}"
      );
    }
  }

  #[test]
  fn e2e_schema_knows_option_is_arity2() {
    if !nix_command::schema::SCHEMA_AVAILABLE {
      return;
    }
    assert_eq!(nix_command::schema::global_flag_arity("option"), Some(2));
  }

  #[test]
  fn e2e_legacy_fallback_covers_critical_flags() {
    // Even without schema, these must work via the legacy fallback
    assert_eq!(resolve_global_flag_arity("option"), 2);
    assert_eq!(resolve_global_flag_arity("extra-experimental-features"), 1);
    assert_eq!(resolve_global_flag_arity("log-format"), 1);
    assert_eq!(resolve_global_flag_arity("builders"), 1);
    assert_eq!(resolve_global_flag_arity("max-jobs"), 1);
    assert_eq!(resolve_global_flag_arity("cores"), 1);
    assert_eq!(resolve_global_flag_arity("store"), 1);
    // Unknown flags default to 0
    assert_eq!(resolve_global_flag_arity("totally-unknown"), 0);
  }
}
