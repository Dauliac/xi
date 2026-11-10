//! BDD: openspec/changes/devshell-e2e-harness
//!   specs/xi-devshell-test-harness/spec.md#Fake nix output realism
//!
//! Pins the contract between the committed `fake-nix` fixture and
//! `DevEnv::from_nix_json`, which is the narrow subset of `nix print-dev-env
//! --json` that xi actually consumes.
//!
//! This is the fixture-drift tripwire for the fast suite. If these assertions
//! and the real nix output ever disagree, the whole end-to-end suite is
//! testing against a contract nix no longer honours, and would stay green
//! while production broke. The complementary check against *real* nix lives
//! outside the fast suite (see tasks 3.8).

use std::{fs, path::Path, process::Command};

use xi_develop::env_file::DevEnv;

/// Same 32-character hash the harness uses. `store_path::parse` skips exactly
/// 33 bytes (hash plus dash), so the length is load-bearing.
const HASH: &str = "abcdefghijklmnopqrstuvwxyz234567";

fn fake_nix() -> std::path::PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures/fake-nix")
    .canonicalize()
    .expect("fake-nix fixture must exist and be readable")
}

/// BDD: xi-devshell-test-harness#Only the consumed subset is relied upon
///
/// Exercises every filter in `from_nix_json` at once: a non-exported entry, a
/// non-string value, and a `SKIP_VARS` member alongside real exported
/// strings.
#[test]
fn only_exported_string_vars_outside_skip_vars_are_kept() {
  let json = format!(
    r#"{{"variables":{{
      "KEPT":{{"type":"exported","value":"yes"}},
      "NOT_EXPORTED":{{"type":"var","value":"no"}},
      "NOT_A_STRING":{{"type":"exported","value":["no"]}},
      "HOME":{{"type":"exported","value":"/should/be/skipped"}},
      "TERM":{{"type":"exported","value":"xterm"}},
      "PATH":{{"type":"exported","value":"/nix/store/{HASH}-cargo-1.97.1/bin"}},
      "shellHook":{{"type":"exported","value":"echo hi"}}
    }}}}"#
  );

  let env = DevEnv::from_nix_json(json.as_bytes()).expect("fixture must parse");

  assert_eq!(
    env.env_vars.get("KEPT").map(String::as_str),
    Some("yes"),
    "an exported string must be kept"
  );
  assert!(
    !env.env_vars.contains_key("NOT_EXPORTED"),
    "a non-exported entry must be dropped"
  );
  assert!(
    !env.env_vars.contains_key("NOT_A_STRING"),
    "a non-string value must be dropped"
  );
  assert!(
    !env.env_vars.contains_key("HOME"),
    "HOME is in SKIP_VARS and must be dropped, or sourcing the env file would \
     relocate the user's home"
  );
  assert!(
    !env.env_vars.contains_key("TERM"),
    "TERM is in SKIP_VARS and must be dropped"
  );
  assert!(
    !env.env_vars.contains_key("PATH"),
    "PATH is routed into nix_paths, not env_vars"
  );
  assert!(
    !env.env_vars.contains_key("shellHook"),
    "shellHook is routed into shell_hook, not env_vars"
  );
}

/// BDD: xi-devshell-test-harness#Only the consumed subset is relied upon
#[test]
fn path_is_colon_split_into_nix_paths() {
  let json = format!(
    r#"{{"variables":{{"PATH":{{"type":"exported","value":"/nix/store/{HASH}-cargo-1.97.1/bin:/nix/store/{HASH}-ripgrep-14.1.0/bin"}}}}}}"#
  );

  let env = DevEnv::from_nix_json(json.as_bytes()).unwrap();

  assert_eq!(env.nix_paths.len(), 2, "PATH must be split on ':'");
  assert!(env.nix_paths[0].ends_with("cargo-1.97.1/bin"));
  assert!(env.nix_paths[1].ends_with("ripgrep-14.1.0/bin"));
}

/// BDD: xi-devshell-test-harness#Only the consumed subset is relied upon
///
/// A blank `shellHook` must not become `Some("")`: downstream code treats
/// `Some` as "there is a hook to run".
#[test]
fn blank_shell_hook_is_dropped_but_real_one_is_kept() {
  let blank =
    r#"{"variables":{"shellHook":{"type":"exported","value":"   "}}}"#;
  assert!(
    DevEnv::from_nix_json(blank.as_bytes())
      .unwrap()
      .shell_hook
      .is_none(),
    "a whitespace-only shellHook must be treated as absent"
  );

  let real = r#"{"variables":{"shellHook":{"type":"exported","value":"echo one\necho two"}}}"#;
  assert_eq!(
    DevEnv::from_nix_json(real.as_bytes()).unwrap().shell_hook,
    Some("echo one\necho two".to_string()),
    "a multi-line shellHook must survive intact"
  );
}

/// BDD: xi-devshell-test-harness#Store paths parse into packages
///
/// The fixture's store-path shape must actually yield packages. If the hash
/// length drifts, parsing silently returns nothing and every package-reporting
/// code path goes untested while still passing.
#[test]
fn fixture_store_paths_parse_into_packages() {
  let json = format!(
    r#"{{"variables":{{"PATH":{{"type":"exported","value":"/nix/store/{HASH}-cargo-1.97.1/bin"}}}}}}"#
  );

  let env = DevEnv::from_nix_json(json.as_bytes()).unwrap();

  assert!(
    !env.packages.is_empty(),
    "the fixture store path must parse into at least one package; if this \
     fails the 32-character hash length has drifted"
  );
  let pkg = &env.packages[0];
  assert_eq!(pkg.name, "cargo", "package name must be parsed");
  assert_eq!(
    pkg.version.as_deref(),
    Some("1.97.1"),
    "package version must be parsed"
  );
}

/// BDD: xi-devshell-test-harness#Store paths parse into packages
#[test]
fn non_store_paths_are_preserved_but_yield_no_package() {
  let json =
    r#"{"variables":{"PATH":{"type":"exported","value":"/usr/local/bin"}}}"#;

  let env = DevEnv::from_nix_json(json.as_bytes()).unwrap();

  assert_eq!(
    env.nix_paths,
    vec!["/usr/local/bin".to_string()],
    "a non-store path must still be preserved on PATH"
  );
  assert!(
    env.packages.is_empty(),
    "a non-store path must not produce a package"
  );
}

// ───────────────────── the fixture script itself ─────────────────────

struct Fixture {
  _tmp: tempfile::TempDir,
  dir:  std::path::PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let tmp = tempfile::Builder::new().prefix("fx").tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    fs::create_dir_all(dir.join("responses")).unwrap();
    fs::create_dir_all(dir.join("store")).unwrap();
    Self { _tmp: tmp, dir }
  }

  fn stage(&self, index: usize, body: &str) {
    fs::write(self.dir.join(format!("responses/{index:03}.json")), body)
      .unwrap();
  }

  fn stage_side(&self, index: usize, ext: &str, body: &str) {
    fs::write(self.dir.join(format!("responses/{index:03}.{ext}")), body)
      .unwrap();
  }

  fn run(&self, args: &[&str]) -> std::process::Output {
    Command::new(fake_nix())
      .args(args)
      .env("XI_FAKE_NIX_DIR", &self.dir)
      .output()
      .expect("fake-nix must be executable; check its file mode in git")
  }
}

/// BDD: xi-devshell-test-harness#Suite runs with no nix present
///
/// Also guards the file mode: a fixture committed without the executable bit
/// fails here rather than mysteriously everywhere else.
#[test]
fn fixture_is_executable_and_answers_print_dev_env() {
  let fx = Fixture::new();
  fx.stage(1, r#"{"variables":{"A":{"type":"exported","value":"1"}}}"#);

  let out = fx.run(&["print-dev-env", ".#", "--json"]);

  assert!(
    out.status.success(),
    "stderr: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  let parsed = DevEnv::from_nix_json(&out.stdout).expect("output must parse");
  assert_eq!(parsed.env_vars.get("A").map(String::as_str), Some("1"));
}

/// BDD: xi-devshell-test-harness#Staged change drives exactly one re-source
///
/// Successive calls walk the staged list, and the last response repeats so a
/// one-response fixture serves unbounded evals.
#[test]
fn responses_advance_then_repeat() {
  let fx = Fixture::new();
  fx.stage(
    1,
    r#"{"variables":{"N":{"type":"exported","value":"one"}}}"#,
  );
  fx.stage(
    2,
    r#"{"variables":{"N":{"type":"exported","value":"two"}}}"#,
  );

  let values: Vec<String> = (0..4)
    .map(|_| {
      let out = fx.run(&["print-dev-env", ".#", "--json"]);
      DevEnv::from_nix_json(&out.stdout)
        .unwrap()
        .env_vars
        .get("N")
        .cloned()
        .unwrap()
    })
    .collect();

  assert_eq!(
    values,
    vec!["one", "two", "two", "two"],
    "calls must advance through staged responses, then repeat the last"
  );
}

/// BDD: xi-devshell-test-harness#Staged failure drives the degraded path
#[test]
fn staged_failure_exits_non_zero_with_stderr() {
  let fx = Fixture::new();
  fx.stage(1, "");
  fx.stage_side(1, "exit", "1");
  fx.stage_side(1, "stderr", "error: attribute 'devShells' missing\n");

  let out = fx.run(&["print-dev-env", ".#", "--json"]);

  assert!(!out.status.success(), "a staged failure must exit non-zero");
  assert!(
    String::from_utf8_lossy(&out.stderr).contains("error:"),
    "the staged stderr payload must reach the caller"
  );
}

/// BDD: xi-devshell-test-harness#Deliberate delay is staged, not slept
///
/// The delay must come from the fake under test instruction, so timeout and
/// backoff paths are reachable without the test itself sleeping and without
/// waiting on real nix. Here the delay IS the subject, not a synchronisation
/// primitive.
#[test]
fn staged_delay_actually_delays_that_call() {
  let fx = Fixture::new();
  fx.stage(1, r#"{"variables":{}}"#);
  fx.stage_side(1, "delay", "0.300");

  let start = std::time::Instant::now();
  let out = fx.run(&["print-dev-env", ".#", "--json"]);
  let elapsed = start.elapsed();

  assert!(out.status.success());
  assert!(
    elapsed >= std::time::Duration::from_millis(250),
    "a staged 300ms delay must actually delay the reply, took {elapsed:?}"
  );
}

/// BDD: xi-devshell-test-harness#Eval arguments are pinned
#[test]
fn invocations_are_recorded_with_cwd_and_args() {
  let fx = Fixture::new();
  fx.stage(1, r#"{"variables":{}}"#);

  fx.run(&[
    "print-dev-env",
    ".#",
    "--json",
    "--option",
    "connect-timeout",
    "5",
  ]);

  let log = fs::read_to_string(fx.dir.join("calls.log")).unwrap();
  assert!(log.contains("arg=print-dev-env"));
  assert!(log.contains("arg=--option"));
  assert!(log.contains("arg=connect-timeout"));
  assert!(
    log.contains("cwd="),
    "the working directory must be recorded"
  );
  assert!(log.contains("end"), "each record must be terminated");
}

/// BDD: xi-devshell-test-harness#Profile symlink is created
#[test]
fn profile_flag_creates_a_symlink() {
  let fx = Fixture::new();
  fx.stage(1, r#"{"variables":{}}"#);
  let profile = fx.dir.join("profile-default");

  fx.run(&[
    "print-dev-env",
    ".#",
    "--json",
    "--profile",
    profile.to_str().unwrap(),
  ]);

  let meta = profile
    .symlink_metadata()
    .expect("the profile path must exist after --profile");
  assert!(
    meta.file_type().is_symlink(),
    "a nix profile is a symlink; GC-root and dix paths depend on that shape"
  );
  assert!(
    profile.join("bin").is_dir(),
    "the symlink must resolve to a directory tree, not a dangling target"
  );
}

/// BDD: xi-devshell-test-harness#Suite runs with no nix present
///
/// The preflight probes xi makes before any devshell work must be answered,
/// and the version must satisfy the recommended minimum or every test's
/// stderr carries a version warning.
#[test]
fn preflight_probes_are_answered() {
  let fx = Fixture::new();
  fx.stage(1, r#"{"variables":{}}"#);

  let version = fx.run(&["--version"]);
  assert!(version.status.success());
  let text = String::from_utf8_lossy(&version.stdout);
  assert!(text.contains("2.31.2"), "got: {text}");

  let features = fx.run(&["config", "show", "experimental-features"]);
  assert!(features.status.success());
  let feats = String::from_utf8_lossy(&features.stdout);
  assert!(feats.contains("nix-command"), "got: {feats}");
  assert!(feats.contains("flakes"), "got: {feats}");
}

/// BDD: xi-devshell-test-harness#Suite runs with no nix present
///
/// An unmodelled subcommand must fail loudly. A plausible empty reply would
/// hide a newly introduced dependency on real nix behaviour.
#[test]
fn unmodelled_subcommand_fails_loudly() {
  let fx = Fixture::new();
  fx.stage(1, r#"{"variables":{}}"#);

  let out = fx.run(&["build", ".#foo"]);

  assert!(
    !out.status.success(),
    "an unmodelled subcommand must not succeed"
  );
  assert!(
    String::from_utf8_lossy(&out.stderr).contains("unsupported subcommand"),
    "the failure must name the cause"
  );
}
