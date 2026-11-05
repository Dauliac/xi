//! BDD: contract for the `xi-sources-config` capability
//! (openspec/changes/xi-sources-config/specs/xi-sources-config/spec.md).
//!
//! Each test below is one scenario from that spec delta, named after it.
//! The assertions are the scenario's THEN clauses and nothing else, so a
//! failure here points at exactly one broken requirement.
//!
//! Two groups of scenarios are deliberately not here:
//!
//! - "System paths require root at activation" is a Nix-module property
//!   (`systemd.tmpfiles.rules`), not a Rust one. It is asserted by evaluating a
//!   real `nixosSystem`; see the note on bead xi-pko.
//! - The adoption scenarios under "Switch is the sources entrypoint" need a git
//!   remote and the `xi` binary, so they live with the CLI in
//!   `crates/xi/tests/bdd_sources_entrypoint.rs`. The halves that are pure
//!   library  -  the `[sources]` block writer, and which status a clone reports
//!   before it has been activated  -  are covered below.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]
#![expect(
  clippy::duration_suboptimal_units,
  reason = "These assert the spec's own spelling (60s, 5m); from_mins would \
            hide what is being pinned"
)]

use std::{collections::HashMap, path::PathBuf, time::Duration};

use serial_test::serial;
use tempfile::TempDir;
use xi_core::sources::{
  GitopsHealth,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatus,
  SourcesStatusResponse,
  config::{
    BlockWrite,
    ConfigError,
    DEFAULT_BRANCH,
    DEFAULT_POLL_INTERVAL,
    Installer,
    Layers,
    Provenance,
    RebootWindow,
    RemoteSpec,
    SOURCES_KEYS,
    ScopePaths,
    SourcesConfig,
    SyncMode,
    WriteError,
    env_var_for,
  },
  switch::record_activation_at,
  write_state_file,
};

/// A fake environment, so no test has to mutate the real one.
///
/// `std::env::set_var` is process-global and these tests run in
/// parallel, so reading the environment through a closure is the only
/// way the XDG and env-var scenarios can be asserted deterministically.
fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
  let map: HashMap<String, String> = pairs
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
  move |key: &str| map.get(key).cloned()
}

fn toml_layer(raw: &str) -> SourcesConfig {
  SourcesConfig::from_toml_str(&PathBuf::from("test.toml"), raw)
    .expect("layer should parse")
}

// -- Requirement: Precedence ladder ---------------------------------------

/// Scenario: Flag beats everything.
///
/// GIVEN the same key is set via all five surfaces with distinct values
/// WHEN the user runs the command with the CLI flag
/// THEN the flag's value is used
/// AND `xi sources status --explain` reports provenance = `cli-flag`
#[test]
fn flag_beats_everything() {
  let layers = Layers {
    cli:        SourcesConfig {
      branch: Some("from-flag".to_owned()),
      ..SourcesConfig::default()
    },
    env:        SourcesConfig::from_env_with(&env_of(&[(
      "XI_SOURCES_BRANCH",
      "from-env",
    )]))
    .expect("env layer"),
    toml:       toml_layer("[sources]\nbranch = \"from-toml\"\n"),
    nix_module: toml_layer("[sources]\nbranch = \"from-nix-module\"\n"),
  };

  let resolved = layers.resolve();

  assert_eq!(resolved.branch, "from-flag");
  assert_eq!(
    resolved.provenance.get("branch"),
    Some(Provenance::CliFlag),
    "provenance must name the rung that actually answered"
  );
}

/// Every rung answers once the rungs above it go quiet, and the
/// built-in default is what answers when all four are empty.
#[test]
fn each_rung_answers_in_turn() {
  let env =
    SourcesConfig::from_env_with(&env_of(&[("XI_SOURCES_BRANCH", "from-env")]))
      .expect("env layer");
  let toml = toml_layer("[sources]\nbranch = \"from-toml\"\n");
  let nix_module = toml_layer("[sources]\nbranch = \"from-nix-module\"\n");

  let cases = [
    (
      Layers {
        env,
        toml: toml.clone(),
        nix_module: nix_module.clone(),
        ..Layers::default()
      },
      "from-env",
      Provenance::EnvVar,
    ),
    (
      Layers {
        toml,
        nix_module: nix_module.clone(),
        ..Layers::default()
      },
      "from-toml",
      Provenance::TomlFile,
    ),
    (
      Layers {
        nix_module,
        ..Layers::default()
      },
      "from-nix-module",
      Provenance::NixModule,
    ),
    (Layers::default(), DEFAULT_BRANCH, Provenance::Default),
  ];

  for (layers, expected, provenance) in cases {
    let resolved = layers.resolve();
    assert_eq!(resolved.branch, expected);
    assert_eq!(resolved.provenance.get("branch"), Some(provenance));
  }
}

/// Scenario: Env variable beats toml.
///
/// GIVEN `.xi.toml` sets `branch = "main"` and `XI_SOURCES_BRANCH=dev`
/// WHEN `xi sources status` runs
/// THEN the resolved branch is `dev`
#[test]
fn env_variable_beats_toml() {
  let layers = Layers {
    env: SourcesConfig::from_env_with(&env_of(&[("XI_SOURCES_BRANCH", "dev")]))
      .expect("env layer"),
    toml: toml_layer("[sources]\nbranch = \"main\"\n"),
    ..Layers::default()
  };

  assert_eq!(layers.resolve().branch, "dev");
}

/// Each key walks the ladder on its own: a lower rung still answers for
/// the keys the higher rungs are silent about.
#[test]
fn rungs_combine_per_key_rather_than_shadowing_wholesale() {
  let layers = Layers {
    toml: toml_layer("[sources]\nbranch = \"from-toml\"\n"),
    nix_module: toml_layer("[sources]\nsync-mode = \"never\"\n"),
    ..Layers::default()
  };

  let resolved = layers.resolve();

  assert_eq!(resolved.branch, "from-toml");
  assert_eq!(resolved.sync_mode, SyncMode::Never);
  assert_eq!(
    resolved.provenance.get("branch"),
    Some(Provenance::TomlFile)
  );
  assert_eq!(
    resolved.provenance.get("sync-mode"),
    Some(Provenance::NixModule)
  );
}

/// The two built-in defaults the spec names explicitly.
#[test]
fn built_in_defaults_match_the_spec() {
  let resolved = Layers::default().resolve();

  assert_eq!(resolved.branch, "main");
  assert_eq!(resolved.poll_interval, Duration::from_secs(60));
  assert_eq!(resolved.poll_interval, DEFAULT_POLL_INTERVAL);
  assert_eq!(resolved.sync_mode, SyncMode::Prompt);
  assert!(!resolved.require_signed);
}

// -- Requirement: Filesystem path table -----------------------------------

/// Scenario: XDG unset falls back to spec defaults.
///
/// GIVEN `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME` are unset
/// WHEN `xi home sources status` runs
/// THEN paths resolve to `~/.local/share/xi/sources`, `~/.local/state/xi`,
/// `~/.config/xi/sources.toml`
#[test]
fn xdg_unset_falls_back_to_spec_defaults() {
  let paths =
    ScopePaths::resolve_with(Scope::User, &env_of(&[("HOME", "/home/you")]));

  assert_eq!(
    paths.sources_dir,
    PathBuf::from("/home/you/.local/share/xi/sources")
  );
  assert_eq!(paths.state_dir, PathBuf::from("/home/you/.local/state/xi"));
  assert_eq!(
    paths.config_file,
    PathBuf::from("/home/you/.config/xi/sources.toml")
  );
}

/// An XDG variable set to the empty string is "not set", per the XDG
/// spec's own "either not set or empty" wording.
#[test]
fn empty_xdg_variables_count_as_unset() {
  let paths = ScopePaths::resolve_with(
    Scope::User,
    &env_of(&[
      ("HOME", "/home/you"),
      ("XDG_DATA_HOME", ""),
      ("XDG_STATE_HOME", ""),
      ("XDG_CONFIG_HOME", ""),
    ]),
  );

  assert_eq!(
    paths.sources_dir,
    PathBuf::from("/home/you/.local/share/xi/sources")
  );
  assert_eq!(paths.state_dir, PathBuf::from("/home/you/.local/state/xi"));
}

/// When the XDG variables *are* set, they win over the `$HOME` fallback.
#[test]
fn xdg_variables_are_honoured_when_set() {
  let paths = ScopePaths::resolve_with(
    Scope::User,
    &env_of(&[
      ("HOME", "/home/you"),
      ("XDG_DATA_HOME", "/data"),
      ("XDG_STATE_HOME", "/state"),
      ("XDG_CONFIG_HOME", "/conf"),
    ]),
  );

  assert_eq!(paths.sources_dir, PathBuf::from("/data/xi/sources"));
  assert_eq!(paths.state_dir, PathBuf::from("/state/xi"));
  assert_eq!(paths.config_file, PathBuf::from("/conf/xi/sources.toml"));
}

/// System scope is a fixed table, not an XDG-derived one.
#[test]
fn system_scope_paths_are_fixed() {
  let paths = ScopePaths::resolve_with(
    Scope::System,
    &env_of(&[("HOME", "/root"), ("XDG_DATA_HOME", "/data")]),
  );

  assert_eq!(paths.sources_dir, PathBuf::from("/var/lib/xi/sources"));
  assert_eq!(paths.state_dir, PathBuf::from("/var/lib/xi/state"));
  assert_eq!(paths.config_file, PathBuf::from("/etc/xi/sources.toml"));
}

/// `XI_SOURCES_STATE_DIR` overrides the state dir and nothing else.
#[test]
fn state_dir_env_override_applies_at_both_scopes() {
  for scope in [Scope::System, Scope::User] {
    let paths = ScopePaths::resolve_with(
      scope,
      &env_of(&[
        ("HOME", "/home/you"),
        ("XI_SOURCES_STATE_DIR", "/elsewhere/state"),
      ]),
    );
    assert_eq!(paths.state_dir, PathBuf::from("/elsewhere/state"));
  }

  // A relative override is ignored rather than honoured: the state dir
  // is read by daemons that have no meaningful CWD.
  let paths = ScopePaths::resolve_with(
    Scope::User,
    &env_of(&[("HOME", "/home/you"), ("XI_SOURCES_STATE_DIR", "relative")]),
  );
  assert_eq!(paths.state_dir, PathBuf::from("/home/you/.local/state/xi"));
}

// -- Requirement: `.xi.toml [sources]` schema -----------------------------

/// Scenario: Unknown key errors with location.
///
/// GIVEN `.xi.toml` contains `[sources] frobnicate = true`
/// WHEN any xi command loads the config
/// THEN the command exits non-zero with a message
/// `<file>:<line>: unknown key 'frobnicate' in [sources]`
#[test]
fn unknown_key_errors_with_location() {
  let raw =
    "# a comment\n[sources]\nremote = \"https://git.example/x\"\nfrobnicate = \
     true\n";
  let err = SourcesConfig::from_toml_str(&PathBuf::from("/tmp/.xi.toml"), raw)
    .expect_err("an unknown key must be rejected");

  match &err {
    ConfigError::File {
      path,
      line,
      message,
    } => {
      assert_eq!(path, &PathBuf::from("/tmp/.xi.toml"));
      assert_eq!(*line, 4, "must point at the offending key, not the file");
      assert_eq!(message, "unknown key 'frobnicate' in [sources]");
    },
    ConfigError::Env { .. } => panic!("expected a file-located error"),
  }

  assert_eq!(
    err.to_string(),
    "/tmp/.xi.toml:4: unknown key 'frobnicate' in [sources]"
  );
}

/// A `.xi.toml` with no `[sources]` table keeps working, per the
/// proposal's "Breaking?: No".
#[test]
fn absent_sources_table_is_an_empty_layer_not_an_error() {
  let config = toml_layer("[ci]\nbackend = \"nix-fast-build\"\n");
  assert!(config.is_empty());
}

/// Every value the schema accepts round-trips into the typed layer.
#[test]
fn full_block_parses_into_every_field() {
  let config = toml_layer(
    "[sources]\nremote = \"git@github.com:you/cfg.git\"\nbranch = \
     \"trunk\"\nworking-dir = \"/home/you/src/cfg\"\npoll-interval = \
     \"5m\"\nrequire-signed = true\nallowed-signers = \
     \"/etc/ssh/allowed_signers\"\nsync-mode = \"auto-copy\"\ninstaller = \
     \"lix\"\n",
  );

  assert_eq!(config.remote.as_deref(), Some("git@github.com:you/cfg.git"));
  assert_eq!(config.branch.as_deref(), Some("trunk"));
  assert_eq!(config.working_dir, Some(PathBuf::from("/home/you/src/cfg")));
  assert_eq!(config.poll_interval, Some(Duration::from_secs(300)));
  assert_eq!(config.require_signed, Some(true));
  assert_eq!(config.sync_mode, Some(SyncMode::AutoCopy));
  assert_eq!(config.installer, Some(Installer::Lix));
}

/// Validation is a load-time concern, not a first-fetch one (SC-C-006),
/// and each failure names the key that caused it.
#[test]
fn invalid_values_are_rejected_at_load_time() {
  let cases = [
    ("remote = \"not a url\"", "invalid remote URL"),
    (
      "working-dir = \"relative/path\"",
      "must be an absolute path",
    ),
    ("allowed-signers = \"rel\"", "must be an absolute path"),
    ("poll-interval = \"banana\"", "invalid 'poll-interval'"),
    ("poll-interval = \"0s\"", "greater than zero"),
    ("branch = \"has space\"", "must not contain whitespace"),
    ("branch = \"-dash\"", "must not start with '-'"),
    ("sync-mode = \"sometimes\"", "expected one of"),
    ("installer = \"brew\"", "expected one of"),
    ("scope = \"user\"", "unknown key 'scope'"),
    ("remote = 42", "must be a string"),
    ("require-signed = \"yes\"", "must be a boolean"),
  ];

  for (line, needle) in cases {
    let raw = format!("[sources]\n{line}\n");
    let err = SourcesConfig::from_toml_str(&PathBuf::from("x.toml"), &raw)
      .expect_err(&format!("`{line}` must be rejected"));
    assert!(
      err.to_string().contains(needle),
      "`{line}` should report {needle:?}, got: {err}"
    );
  }
}

/// `require-signed` without `allowed-signers` is the one cross-key rule
/// the schema cannot express on its own.
#[test]
fn require_signed_without_allowed_signers_is_rejected() {
  let layers = Layers {
    toml: toml_layer("[sources]\nrequire-signed = true\n"),
    ..Layers::default()
  };

  let err = layers
    .resolve()
    .check_consistency()
    .expect_err("an empty trust root must not pass as verification");
  assert!(err.contains("allowed-signers"), "got: {err}");
}

// -- Requirement: Env variable surface ------------------------------------

/// The spec's rule is mechanical: uppercase the key, hyphens to
/// underscores. Pinning it stops the two sides drifting.
#[test]
fn env_var_names_follow_the_mechanical_rule() {
  assert_eq!(env_var_for("remote"), "XI_SOURCES_REMOTE");
  assert_eq!(env_var_for("poll-interval"), "XI_SOURCES_POLL_INTERVAL");
  assert_eq!(env_var_for("allowed-signers"), "XI_SOURCES_ALLOWED_SIGNERS");
  // `xi-sources-conflict` task 5b.1 names this one in the spec text, so
  // it is pinned rather than left to the rule above.
  assert_eq!(env_var_for("sync-mode"), "XI_SOURCES_SYNC_MODE");
}

/// Every key the `[sources]` table accepts has an env spelling, because
/// the precedence ladder promises the env rung can answer for any of
/// them. A sub-table is the documented exception and lives in
/// `SOURCES_TABLE_KEYS` instead.
#[test]
fn every_scalar_key_has_an_env_spelling() {
  for key in SOURCES_KEYS {
    let var = env_var_for(key);
    assert!(
      var.starts_with("XI_SOURCES_"),
      "`{key}` produced `{var}`, which is not an XI_SOURCES_* name"
    );
    assert!(
      !var.contains('-') && !var.contains('.'),
      "`{key}` produced `{var}`, which is not a legal variable name"
    );
  }
}

/// The env layer applies exactly the validation the TOML layer does,
/// and reports the variable instead of a file+line.
#[test]
fn env_layer_validates_and_names_the_variable() {
  let err = SourcesConfig::from_env_with(&env_of(&[(
    "XI_SOURCES_POLL_INTERVAL",
    "banana",
  )]))
  .expect_err("a bad duration must be rejected");

  match &err {
    ConfigError::Env { var, .. } => {
      assert_eq!(var, "XI_SOURCES_POLL_INTERVAL");
    },
    ConfigError::File { .. } => panic!("expected an env-located error"),
  }
  assert!(err.to_string().starts_with("XI_SOURCES_POLL_INTERVAL: "));
}

/// An empty variable is "not set", so it does not occupy its rung.
#[test]
fn empty_env_variable_does_not_occupy_its_rung() {
  let layers = Layers {
    env: SourcesConfig::from_env_with(&env_of(&[("XI_SOURCES_BRANCH", "")]))
      .expect("env layer"),
    toml: toml_layer("[sources]\nbranch = \"from-toml\"\n"),
    ..Layers::default()
  };

  let resolved = layers.resolve();
  assert_eq!(resolved.branch, "from-toml");
  assert_eq!(
    resolved.provenance.get("branch"),
    Some(Provenance::TomlFile)
  );
}

// -- The `[sources]` block writer -----------------------------------------
//
// `SourcesConfig::write_block` is the one surface that *writes* the
// schema the rest of this file reads. Its only caller, `xi sources
// init`, was removed when switch became the single entrypoint, so these
// tests are what keeps the writer honest until a capability picks it up
// again. See bead xi-4p3h.

/// A second write at a different URL must refuse rather than clobber.
#[test]
fn init_refuses_to_overwrite_without_force() {
  let tmp = TempDir::new().expect("tempdir");
  let path = tmp.path().join(".xi.toml");
  let original = "# keep me\n[sources]\nremote = \"https://git.example/a\"\n";
  std::fs::write(&path, original).expect("seed config");

  let incoming = SourcesConfig {
    remote: Some("https://git.example/b".to_owned()),
    ..SourcesConfig::default()
  };

  let err = incoming
    .write_block(&path, false)
    .expect_err("a second init must refuse");
  assert!(matches!(err, WriteError::AlreadyConfigured { .. }));
  assert!(err.to_string().contains("pass --force"));

  assert_eq!(
    std::fs::read_to_string(&path).expect("reread"),
    original,
    "the refusal must not be half-applied"
  );
}

/// With `--force` the block is replaced, and only the block.
#[test]
fn init_with_force_replaces_only_the_sources_block() {
  let tmp = TempDir::new().expect("tempdir");
  let path = tmp.path().join(".xi.toml");
  std::fs::write(
    &path,
    "# keep me\n[ci]\nbackend = \"nix-fast-build\"\n\n[sources]\n\
     remote = \"https://git.example/a\"\n",
  )
  .expect("seed config");

  let incoming = SourcesConfig {
    remote: Some("https://git.example/b".to_owned()),
    ..SourcesConfig::default()
  };

  assert_eq!(
    incoming.write_block(&path, true).expect("forced write"),
    BlockWrite::Replaced
  );

  let after = std::fs::read_to_string(&path).expect("reread");
  assert!(
    after.contains("# keep me"),
    "comments must survive: {after}"
  );
  assert!(
    after.contains("nix-fast-build"),
    "[ci] must survive: {after}"
  );
  assert!(after.contains("https://git.example/b"));
  assert!(!after.contains("https://git.example/a"));
}

/// A missing file is created; a file without the block gets one
/// appended.
#[test]
fn init_creates_or_appends_depending_on_the_file() {
  let tmp = TempDir::new().expect("tempdir");
  let config = SourcesConfig {
    remote: Some("https://git.example/a".to_owned()),
    ..SourcesConfig::default()
  };

  let fresh = tmp.path().join("fresh/.xi.toml");
  assert_eq!(
    config.write_block(&fresh, false).expect("create"),
    BlockWrite::Created
  );

  let existing = tmp.path().join("existing.toml");
  std::fs::write(&existing, "[ci]\nbackend = \"auto\"\n").expect("seed");
  assert_eq!(
    config.write_block(&existing, false).expect("append"),
    BlockWrite::Appended
  );
  let after = std::fs::read_to_string(&existing).expect("reread");
  assert!(after.contains("[ci]") && after.contains("[sources]"));
}

/// `init` writes only what the operator supplied. Writing today's
/// defaults would promote rung 5 to rung 3 and make the ladder lie.
#[test]
fn init_writes_a_minimal_block() {
  let tmp = TempDir::new().expect("tempdir");
  let path = tmp.path().join(".xi.toml");
  SourcesConfig {
    remote: Some("https://git.example/a".to_owned()),
    ..SourcesConfig::default()
  }
  .write_block(&path, false)
  .expect("write");

  let after = std::fs::read_to_string(&path).expect("reread");
  assert!(!after.contains("branch"), "default must not be frozen in");
  assert!(!after.contains("poll-interval"));
  assert!(!after.contains("sync-mode"));

  let reparsed = toml_layer(&after);
  assert_eq!(reparsed.branch, None);
  assert_eq!(
    Layers {
      toml: reparsed,
      ..Layers::default()
    }
    .resolve()
    .provenance
    .get("branch"),
    Some(Provenance::Default)
  );
}

/// Anything `init` writes must be readable by the loader: these two
/// sides are the same schema and must not drift.
#[test]
fn written_blocks_round_trip_through_the_loader() {
  let tmp = TempDir::new().expect("tempdir");
  let path = tmp.path().join(".xi.toml");
  let original = SourcesConfig {
    remote:          Some("ssh://git@example.org/cfg".to_owned()),
    remotes:         Some(vec![RemoteSpec::plain(
      "ssh://git@example.org/cfg".to_owned(),
    )]),
    branch:          Some("trunk".to_owned()),
    working_dir:     Some(PathBuf::from("/home/you/src/cfg")),
    poll_interval:   Some(Duration::from_secs(300)),
    require_signed:  Some(true),
    allowed_signers: Some(PathBuf::from("/etc/ssh/allowed_signers")),
    gpg_public_keys: Some(PathBuf::from("/etc/xi/gpg.asc")),
    sync_mode:       Some(SyncMode::Never),
    installer:       Some(Installer::Detsys),
    reboot_window:   Some(RebootWindow::Daily {
      start: 2 * 60,
      end:   4 * 60,
    }),
    on_advance:      Some("echo advanced".to_owned()),
    on_build:        Some("echo built".to_owned()),
    on_switch:       Some("systemctl restart my-service".to_owned()),
    // The two sub-trees have their own round-trip coverage in the
    // credentials and push suites; left empty here so this case keeps
    // testing exactly the flat keys it was written for.
    credentials:     Default::default(),
    secrets:         Default::default(),
  };

  original.write_block(&path, false).expect("write");
  let reparsed =
    SourcesConfig::from_toml_file(&path).expect("written block must parse");

  assert_eq!(reparsed, original);
}

// -- Cross-surface: the Nix modules write the same schema -----------------

/// The exact bytes `modules/nixos/sources.nix` produced when evaluated
/// against a real `nixosSystem`.
///
/// Pinned as a fixture so a rename on the Nix side (`workingDir` ->
/// `working-dir` and friends) fails here instead of at a user's first
/// activation, where the only symptom would be a silently ignored key.
#[test]
fn nix_module_output_parses_as_a_valid_layer() {
  let rendered = "[sources]\npoll-interval = \"5m\"\nremote = \
                  \"git@github.com:you/nix-config.git\"\nworking-dir = \
                  \"/home/you/src/nix-config\"\n";

  let layer = SourcesConfig::from_toml_str(
    &PathBuf::from("/etc/xi/sources.toml"),
    rendered,
  )
  .expect("the Nix module must emit a schema this crate accepts");

  assert_eq!(
    layer.remote.as_deref(),
    Some("git@github.com:you/nix-config.git")
  );
  assert_eq!(layer.poll_interval, Some(Duration::from_secs(300)));
  assert_eq!(
    layer.working_dir,
    Some(PathBuf::from("/home/you/src/nix-config"))
  );
}

// -- Requirement: Remote and installable are distinguished without a flag --

/// Scenario: A flake reference is not adopted.
/// Scenario: An https remote is adopted.
///
/// One case per row of the table in
/// `openspec/changes/xi-sources-config/design.md`, plus the shapes most
/// likely to be misread. Adopting something the user meant as a
/// one-off flake ref would silently repoint what the machine deploys
/// from, so this table is the safety property of the whole entrypoint.
#[test]
fn positional_shape_decides_remote_versus_installable() {
  use xi_core::sources::config::{PositionalKind, classify_positional};

  let remotes = [
    "git@github.com:you/cfg.git",
    "git@git.example.org:cfg",
    "https://git.example/owner/repo.git",
    "https://git.example/dotfiles",
    "http://git.example/dotfiles",
    "ssh://git@example.org/cfg",
  ];
  for arg in remotes {
    assert_eq!(
      classify_positional(arg),
      PositionalKind::ConfigRemote,
      "`{arg}` should be read as a config-repo remote"
    );
  }

  let installables = [
    ".",
    "./cfg",
    "/abs/path",
    "/nix/store/abc-foo",
    "github:owner/repo",
    "gitlab:owner/repo",
    "sourcehut:~owner/repo",
    "git+https://git.example/repo",
    "git+ssh://git@example.org/cfg",
    "path:/abs/path",
    "flake:nixpkgs",
    "nixpkgs",
    "",
  ];
  for arg in installables {
    assert_eq!(
      classify_positional(arg),
      PositionalKind::Installable,
      "`{arg}` should keep its pre-existing installable meaning"
    );
  }
}

/// The `git+` prefix is the user explicitly asking for the flake
/// reading of a URL that would otherwise be adopted.
#[test]
fn git_plus_prefix_opts_out_of_adoption() {
  use xi_core::sources::config::{PositionalKind, classify_positional};

  assert_eq!(
    classify_positional("https://git.example/cfg"),
    PositionalKind::ConfigRemote
  );
  assert_eq!(
    classify_positional("git+https://git.example/cfg"),
    PositionalKind::Installable
  );
}

/// Classification is shape-only: no network, no filesystem, so it
/// cannot differ between machines or hang on a slow DNS lookup.
#[test]
fn classification_does_not_depend_on_what_exists_on_disk() {
  use xi_core::sources::config::{PositionalKind, classify_positional};

  // Neither path exists in the test environment.
  assert_eq!(
    classify_positional("/definitely/not/here"),
    PositionalKind::Installable
  );
  assert_eq!(
    classify_positional("https://definitely.invalid/nope.git"),
    PositionalKind::ConfigRemote
  );
}

// -- Requirement: a positional is disambiguated without breaking existing
// meanings (xi-deploy-unified-verb) --------------------------------------

/// Every shape `classify_positional` already resolves keeps that exact
/// reading from `classify_deploy_positional` when no target matches -
/// confirming the new function is layered on top rather than a
/// modification of the existing one.
#[test]
fn deploy_positional_falls_through_to_classify_positional_when_no_target_matches()
 {
  use std::collections::BTreeSet;

  use xi_core::sources::config::{
    DeployPositionalKind,
    PositionalKind,
    classify_deploy_positional,
  };

  let no_targets = BTreeSet::new();

  let remotes = [
    "git@github.com:you/cfg.git",
    "git@git.example.org:cfg",
    "https://git.example/owner/repo.git",
    "https://git.example/dotfiles",
    "http://git.example/dotfiles",
    "ssh://git@example.org/cfg",
  ];
  for arg in remotes {
    assert_eq!(
      classify_deploy_positional(arg, &no_targets),
      DeployPositionalKind::Positional(PositionalKind::ConfigRemote),
      "`{arg}` should keep its config-remote reading"
    );
  }

  let installables = [
    ".",
    "./cfg",
    "/abs/path",
    "/nix/store/abc-foo",
    "github:owner/repo",
    "gitlab:owner/repo",
    "sourcehut:~owner/repo",
    "git+https://git.example/repo",
    "git+ssh://git@example.org/cfg",
    "path:/abs/path",
    "flake:nixpkgs",
    "nixpkgs",
    "",
  ];
  for arg in installables {
    assert_eq!(
      classify_deploy_positional(arg, &no_targets),
      DeployPositionalKind::Positional(PositionalKind::Installable),
      "`{arg}` should keep its installable reading"
    );
  }
}

/// A bare word with none of `classify_positional`'s recognised shapes
/// resolves as a fleet target when it is in the discovered/inventory set.
#[test]
fn deploy_positional_resolves_a_bare_word_target_first() {
  use std::collections::BTreeSet;

  use xi_core::sources::config::{
    DeployPositionalKind,
    classify_deploy_positional,
  };

  let targets: BTreeSet<String> = ["web1".to_owned(), "db1".to_owned()].into();

  assert_eq!(
    classify_deploy_positional("web1", &targets),
    DeployPositionalKind::Target
  );
}

/// A bare word matching a target resolves as a target even when a
/// same-named directory exists in `cwd` - `classify_deploy_positional`
/// never stats the filesystem, so a colliding local directory must not
/// change the reading.
#[test]
#[serial]
fn deploy_positional_resolves_as_target_even_with_a_same_named_local_dir() {
  use std::collections::BTreeSet;

  use xi_core::sources::config::{
    DeployPositionalKind,
    classify_deploy_positional,
  };

  let tmp = TempDir::new().expect("tempdir");
  std::fs::create_dir(tmp.path().join("web1")).expect("create colliding dir");

  let original_dir = std::env::current_dir().expect("current dir");
  std::env::set_current_dir(tmp.path()).expect("chdir into tempdir");
  let result = std::panic::catch_unwind(|| {
    let targets: BTreeSet<String> = ["web1".to_owned()].into();
    classify_deploy_positional("web1", &targets)
  });
  std::env::set_current_dir(original_dir).expect("restore cwd");

  assert_eq!(
    result.expect("classify_deploy_positional must not panic"),
    DeployPositionalKind::Target,
    "a bare word matching a target must resolve as a target even when a \
     same-named directory exists in cwd"
  );
}

/// A bare word matching no target still falls through to
/// `classify_positional`'s default installable reading.
#[test]
fn deploy_positional_falls_through_when_bare_word_matches_no_target() {
  use std::collections::BTreeSet;

  use xi_core::sources::config::{
    DeployPositionalKind,
    PositionalKind,
    classify_deploy_positional,
  };

  let targets: BTreeSet<String> = ["web1".to_owned()].into();

  assert_eq!(
    classify_deploy_positional("nope", &targets),
    DeployPositionalKind::Positional(PositionalKind::Installable)
  );
}

/// Anything already shaped like a path, URL, scp spec, or flake
/// reference keeps that reading even when its text happens to equal a
/// target name - the target check only ever applies to bare words.
#[test]
fn deploy_positional_shaped_args_never_read_as_a_target() {
  use std::collections::BTreeSet;

  use xi_core::sources::config::{
    DeployPositionalKind,
    PositionalKind,
    classify_deploy_positional,
  };

  let targets: BTreeSet<String> = ["web1".to_owned()].into();

  assert_eq!(
    classify_deploy_positional("./web1", &targets),
    DeployPositionalKind::Positional(PositionalKind::Installable),
    "a path-shaped positional is never read as a target"
  );
  assert_eq!(
    classify_deploy_positional("github:web1", &targets),
    DeployPositionalKind::Positional(PositionalKind::Installable),
    "a flake-ref-shaped positional is never read as a target"
  );
}

// -- Requirement: Switch is the sources entrypoint, status half -----------

/// The status a fresh adopt reports, decided on bead xi-4p3h.
///
/// Adoption is steps 1-5 of the entrypoint requirement and "proceed
/// with the normal build and activation" is step 6, so the window
/// between a successful clone and a successful activation is reachable
/// whenever step 6 fails. [[xi-sources-model]] maps exactly that window
/// - "sources dir exists but `active_generation` is unknown (never
/// switched)" - to `Pending`, so a clone alone must never report
/// `Ready`, and recording an activation is the only thing that promotes
/// it.
#[test]
fn a_clone_is_pending_until_an_activation_is_recorded() {
  let tmp = TempDir::new().expect("tempdir");
  let paths = ScopePaths {
    sources_dir: tmp.path().join("sources"),
    state_dir:   tmp.path().join("state"),
    config_file: tmp.path().join("sources.toml"),
  };

  // Exactly what adoption leaves behind: a clone, the remote it came
  // from, and that clone's HEAD. No generation.
  std::fs::create_dir_all(&paths.sources_dir).expect("mkdir sources");
  for (name, value) in [
    ("remote-url", "https://git.example/dotfiles"),
    ("sources-head", "aaaa1111"),
  ] {
    write_state_file(&paths.state_dir, name, value).expect("seed state");
  }

  match status_of(&paths) {
    SourcesStatus::Pending { snapshot } => {
      assert_eq!(snapshot.refs.sources_head.as_deref(), Some("aaaa1111"));
      assert_eq!(snapshot.refs.active_generation, None);
    },
    other => {
      panic!("a clone with no generation must be Pending, got {other:?}")
    },
  }

  // Now what a successful activation leaves behind.
  let profile = tmp.path().join("generation-42");
  std::fs::create_dir_all(&profile).expect("mkdir profile");
  record_activation_at(&paths, &profile, false).expect("record activation");

  let resolved = profile
    .canonicalize()
    .expect("canonicalize profile")
    .display()
    .to_string();
  match status_of(&paths) {
    SourcesStatus::Ready { snapshot } => {
      assert_eq!(
        snapshot.refs.active_generation.as_deref(),
        Some(resolved.as_str()),
        "the recorded generation must name the activated store path"
      );
      assert_eq!(
        snapshot.refs.sources_head.as_deref(),
        Some("aaaa1111"),
        "recording a generation must not disturb the adopted HEAD"
      );
    },
    other => panic!("expected Ready after an activation, got {other:?}"),
  }
}

/// `xi <scope> sources status` reads the model, not the writer, so the
/// promotion has to be visible in the serialised envelope too.
#[test]
fn the_envelope_reports_pending_before_an_activation_and_ready_after() {
  let tmp = TempDir::new().expect("tempdir");
  let paths = ScopePaths {
    sources_dir: tmp.path().join("sources"),
    state_dir:   tmp.path().join("state"),
    config_file: tmp.path().join("sources.toml"),
  };
  std::fs::create_dir_all(&paths.sources_dir).expect("mkdir sources");
  write_state_file(&paths.state_dir, "remote-url", "https://git.example/d")
    .expect("seed state");

  let envelope = |paths: &ScopePaths| {
    serde_json::to_value(SourcesStatusResponse::new(status_of(paths)))
      .expect("serialise envelope")
  };

  let before = envelope(&paths);
  assert_eq!(before["status"], "pending");
  assert!(before["active_generation"].is_null());

  record_activation_at(&paths, &tmp.path().join("sources"), false)
    .expect("record activation");

  let after = envelope(&paths);
  assert_eq!(after["status"], "ready");
  assert!(after["active_generation"].is_string());
}

/// The model's four statuses, driven from a path table.
fn status_of(paths: &ScopePaths) -> SourcesStatus {
  let id = SourcesId::new(Scope::User, "https://git.example/dotfiles");
  SourcesContext::load(id, &paths.sources_dir, &paths.state_dir)
    .expect("load context")
    .status(&GitopsHealth::Disabled)
}
