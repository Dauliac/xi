use std::{collections::BTreeSet, fs, path::PathBuf};

use tempfile::TempDir;

use super::*;

/// A directory that looks like a flake, so validation passes.
fn flake_dir(name: &str) -> (TempDir, PathBuf) {
  let tmp = TempDir::new().expect("tempdir");
  let dir = tmp.path().join(name);
  fs::create_dir_all(&dir).expect("create dir");
  fs::write(dir.join("flake.nix"), "{}").expect("write flake.nix");
  (tmp, dir)
}

fn inputs(names: &[&str]) -> BTreeSet<String> {
  names.iter().map(|s| (*s).to_string()).collect()
}

// ---------------------------------------------------------------- 4.1

#[test]
fn path_discriminator_accepts_path_shapes() {
  for value in [
    "path:/abs",
    "/abs",
    "./rel",
    "../rel",
    "~",
    "~/ghq/github.com/Dauliac/nix-lib",
    ".",
    "..",
  ] {
    assert!(is_path_value(value), "{value} should be a path");
  }
}

#[test]
fn path_discriminator_forwards_everything_else() {
  // A bare word is an indirect flakeref, never guessed to be a directory.
  for value in [
    "nixpkgs",
    "nix-lib",
    "github:NixOS/nixpkgs/nixos-unstable",
    "git+ssh://git@github.com/o/r",
    "https://example.com/x.tar.gz",
    "git+file:///abs",
  ] {
    assert!(
      !is_path_value(value),
      "{value} should be forwarded verbatim"
    );
  }
}

#[test]
fn url_value_is_not_touched() {
  let entry = Entry::new(
    "nixpkgs",
    "github:NixOS/nixpkgs/nixos-unstable",
    OverrideSource::GlobalConfig,
  );
  let r = normalize(&entry).expect("verbatim");
  assert_eq!(r.flakeref, "github:NixOS/nixpkgs/nixos-unstable");
  assert_eq!(r.path, None);
}

#[test]
// BDD: tests/features/21_override_input_config.feature#
// a-bare-word-is-not-mistaken-for-a-directory
fn bare_word_is_not_mistaken_for_a_directory() {
  let entry = Entry::new("nixpkgs", "nixpkgs", OverrideSource::GlobalConfig);
  let r = normalize(&entry).expect("verbatim");
  assert_eq!(r.flakeref, "nixpkgs");
}

// ---------------------------------------------------------------- 4.2

#[test]
fn absolute_path_becomes_path_scheme_flakeref() {
  let (_tmp, dir) = flake_dir("nix-lib");
  let entry = Entry::new(
    "nix-lib",
    dir.to_string_lossy().to_string(),
    OverrideSource::GlobalConfig,
  );
  let r = normalize(&entry).expect("normalizes");
  assert_eq!(r.flakeref, format!("path:{}", dir.display()));
  assert_eq!(r.path.as_deref(), Some(dir.as_path()));
}

#[test]
fn lexical_clean_does_not_resolve_symlinks() {
  assert_eq!(clean_lexical(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
  assert_eq!(clean_lexical(Path::new("/a/./b")), PathBuf::from("/a/b"));
  assert_eq!(
    clean_lexical(Path::new("/a/b/../../c")),
    PathBuf::from("/c")
  );
  // `..` cannot escape the root.
  assert_eq!(clean_lexical(Path::new("/../a")), PathBuf::from("/a"));
  // A relative path keeps leading `..` it cannot cancel.
  assert_eq!(clean_lexical(Path::new("../a")), PathBuf::from("../a"));
}

#[test]
fn tilde_is_expanded_because_toml_never_does_it() {
  let (tmp, dir) = flake_dir("nix-lib");
  // SAFETY: single-threaded test process scope.
  let prev = std::env::var("HOME").ok();
  unsafe { std::env::set_var("HOME", tmp.path()) };

  let entry = Entry::new("nix-lib", "~/nix-lib", OverrideSource::GlobalConfig);
  let r = normalize(&entry).expect("expands ~");
  assert_eq!(r.flakeref, format!("path:{}", dir.display()));

  match prev {
    Some(v) => unsafe { std::env::set_var("HOME", v) },
    None => unsafe { std::env::remove_var("HOME") },
  }
}

// ---------------------------------------------------------------- 4.3

#[test]
fn path_scheme_prefixed_relative_value_is_still_resolved() {
  let (tmp, dir) = flake_dir("nix-lib");
  let entry =
    Entry::new("nix-lib", "path:./nix-lib", OverrideSource::ProjectToml {
      dir: tmp.path().to_path_buf(),
    });
  let r = normalize(&entry).expect("strips and re-applies the scheme");
  assert_eq!(r.flakeref, format!("path:{}", dir.display()));
}

// ---------------------------------------------------------------- 4.4

#[test]
// BDD: tests/features/21_override_input_config.feature#
// an-unusable-path-fails-with-both-spellings
fn missing_directory_names_both_spellings() {
  let tmp = TempDir::new().expect("tempdir");
  let entry = Entry::new("nix-lib", "./nope", OverrideSource::ProjectToml {
    dir: tmp.path().to_path_buf(),
  });
  let err = normalize(&entry).expect_err("must fail");
  let msg = err.to_string();
  // The value as written, so the user recognises it.
  assert!(msg.contains("./nope"), "missing value-as-written: {msg}");
  // And the absolute path, so a wrong base directory is diagnosable.
  assert!(
    msg.contains(&tmp.path().join("nope").display().to_string()),
    "missing resolved path: {msg}"
  );
}

#[test]
fn directory_without_flake_nix_is_refused() {
  let tmp = TempDir::new().expect("tempdir");
  let dir = tmp.path().join("plain");
  fs::create_dir_all(&dir).expect("create dir");

  let entry = Entry::new("nix-lib", "./plain", OverrideSource::ProjectToml {
    dir: tmp.path().to_path_buf(),
  });
  let err = normalize(&entry).expect_err("must fail");
  assert!(
    err.to_string().contains("no flake.nix"),
    "unexpected: {err}"
  );
}

// ---------------------------------------------------------------- 4.5

#[test]
// BDD: tests/features/21_override_input_config.feature#
// a-path-containing-a-space-is-percent-encoded
fn space_in_path_is_percent_encoded() {
  // A raw space makes nix stop parsing `path:` as a URL and prepend the CWD
  // to the literal text, so this is a correctness case, not an edge case.
  let (_tmp, dir) = flake_dir("sp test");
  let entry = Entry::new(
    "nix-lib",
    dir.to_string_lossy().to_string(),
    OverrideSource::GlobalConfig,
  );
  let r = normalize(&entry).expect("normalizes");

  assert!(
    !r.flakeref.contains(' '),
    "a raw space survived: {}",
    r.flakeref
  );
  assert!(
    r.flakeref.contains("sp%20test"),
    "space not encoded: {}",
    r.flakeref
  );
}

#[test]
fn path_separators_survive_encoding() {
  assert_eq!(percent_encode_path("/a/b/c"), "/a/b/c");
}

#[test]
fn already_encoded_triplet_is_not_double_encoded() {
  assert_eq!(percent_encode_path("/a/sp%20test"), "/a/sp%20test");
}

#[test]
fn stray_percent_is_encoded() {
  assert_eq!(percent_encode_path("/a/100%"), "/a/100%25");
}

// ---------------------------------------------------------------- 3.1-3.3

#[test]
fn env_parses_whitespace_separated_pairs() {
  let pairs = parse_env_value("nix-lib=./a nix-vm-test=./b").expect("parses");
  assert_eq!(pairs, vec![
    ("nix-lib".to_string(), "./a".to_string()),
    ("nix-vm-test".to_string(), "./b".to_string()),
  ]);
}

#[test]
fn env_multiline_form_is_identical() {
  let one = parse_env_value("nix-lib=./a nix-vm-test=./b").expect("parses");
  let many =
    parse_env_value("\n  nix-lib=./a\n  nix-vm-test=./b\n").expect("parses");
  assert_eq!(one, many);
}

#[test]
// BDD: tests/features/21_override_input_config.feature#
// a-flakeref-containing-equals-survives-parsing
fn env_splits_on_first_equals_only() {
  // A flakeref carries its own `=` in its query string.
  let pairs =
    parse_env_value("nixpkgs=github:o/r?ref=main&rev=abc").expect("parses");
  assert_eq!(pairs, vec![(
    "nixpkgs".to_string(),
    "github:o/r?ref=main&rev=abc".to_string(),
  )]);
}

#[test]
fn env_rejects_pair_without_equals() {
  let err = parse_env_value("nix-lib ./a").expect_err("must fail");
  // Naming the token is the point: a dropped typo changes what gets built.
  assert!(err.to_string().contains("nix-lib"), "unexpected: {err}");
}

#[test]
fn env_rejects_empty_input_name() {
  let err = parse_env_value("=./a").expect_err("must fail");
  assert!(err.to_string().contains("empty input name"), "got: {err}");
}

#[test]
fn env_empty_value_yields_no_pairs() {
  assert!(parse_env_value("   \n ").expect("parses").is_empty());
}

// ---------------------------------------------------------------- base dirs

#[test]
fn project_file_value_resolves_against_the_files_own_directory() {
  let (tmp, dir) = flake_dir("nix-lib");
  // The command runs from a nested subdirectory; the value must not care.
  let entry = Entry::new("nix-lib", "./nix-lib", OverrideSource::ProjectToml {
    dir: tmp.path().to_path_buf(),
  });
  let r = normalize(&entry).expect("normalizes");
  assert_eq!(r.path.as_deref(), Some(dir.as_path()));
}

#[test]
fn env_value_resolves_against_the_flake_root() {
  let (tmp, dir) = flake_dir("nix-lib");
  let entry = Entry::new("nix-lib", "./nix-lib", OverrideSource::Env {
    flake_root: tmp.path().to_path_buf(),
  });
  let r = normalize(&entry).expect("normalizes");
  assert_eq!(r.path.as_deref(), Some(dir.as_path()));
}

#[test]
fn cli_value_resolves_against_the_process_cwd() {
  // Typed at a prompt, it must mean what the shell would mean.
  let (tmp, dir) = flake_dir("nix-lib");
  let entry = Entry::new("nix-lib", "./nix-lib", OverrideSource::Cli {
    cwd: tmp.path().to_path_buf(),
  });
  let r = normalize(&entry).expect("normalizes");
  assert_eq!(r.path.as_deref(), Some(dir.as_path()));
}

#[test]
// BDD: tests/features/21_override_input_config.feature#
// a-relative-value-in-the-global-config-is-refused
fn relative_value_in_global_config_is_refused() {
  let entry = Entry::new("nix-lib", "../nix-lib", OverrideSource::GlobalConfig);
  let err = normalize(&entry).expect_err("must fail");
  assert!(
    matches!(err, Error::RelativeInGlobalConfig { .. }),
    "got: {err}"
  );
  assert!(
    err.to_string().contains("machine-global"),
    "error should explain why: {err}"
  );
}

#[test]
fn absolute_value_in_global_config_is_fine() {
  let (_tmp, dir) = flake_dir("nix-lib");
  let entry = Entry::new(
    "nix-lib",
    dir.to_string_lossy().to_string(),
    OverrideSource::GlobalConfig,
  );
  assert!(normalize(&entry).is_ok());
}

// ---------------------------------------------------------------- 5.2 merge

#[test]
fn higher_rank_source_wins_per_name() {
  let (_tmp, dir) = flake_dir("nix-lib");
  let abs = dir.to_string_lossy().to_string();

  let entries = vec![
    Entry::new(
      "nix-lib",
      "github:Dauliac/nix-lib",
      OverrideSource::GlobalConfig,
    ),
    Entry::new("nix-lib", abs.clone(), OverrideSource::Cli {
      cwd: dir.clone(),
    }),
  ];
  let out = resolve(&entries, None).expect("resolves");

  // Exactly one override for that input, and only the winner is advertised.
  assert_eq!(out.len(), 1);
  assert_eq!(out[0].flakeref, format!("path:{}", dir.display()));
}

#[test]
// BDD: tests/features/21_override_input_config.feature#
// a-flag-replaces-only-the-input-it-names
fn a_flag_replaces_only_the_input_it_names() {
  let (_tmp, dir) = flake_dir("other");
  let entries = vec![
    Entry::new(
      "nix-lib",
      "github:Dauliac/nix-lib",
      OverrideSource::GlobalConfig,
    ),
    Entry::new(
      "nix-vm-test",
      "github:numtide/nix-vm-test",
      OverrideSource::GlobalConfig,
    ),
    Entry::new(
      "nix-lib",
      dir.to_string_lossy().to_string(),
      OverrideSource::Cli { cwd: dir.clone() },
    ),
  ];
  let out = resolve(&entries, None).expect("resolves");

  assert_eq!(out.len(), 2);
  let by_name: BTreeMap<_, _> = out
    .iter()
    .map(|r| (r.name.as_str(), r.flakeref.as_str()))
    .collect();
  assert_eq!(by_name["nix-lib"], format!("path:{}", dir.display()));
  assert_eq!(by_name["nix-vm-test"], "github:numtide/nix-vm-test");
}

#[test]
fn cascade_ranks_are_ordered_as_documented() {
  let d = PathBuf::from("/x");
  let ranks = [
    OverrideSource::GlobalConfig.rank(),
    OverrideSource::ProjectToml { dir: d.clone() }.rank(),
    OverrideSource::ProjectLocalToml { dir: d.clone() }.rank(),
    OverrideSource::Env {
      flake_root: d.clone(),
    }
    .rank(),
    OverrideSource::Cli { cwd: d }.rank(),
  ];
  assert!(ranks.windows(2).all(|w| w[0] < w[1]), "{ranks:?}");
}

// ---------------------------------------------------------------- 5.3 filter

#[test]
// BDD: tests/features/21_override_input_config.feature#
// a-global-entry-is-silent-in-unrelated-repos
fn override_naming_an_absent_input_is_dropped() {
  let entries = vec![Entry::new(
    "nix-lib",
    "github:Dauliac/nix-lib",
    OverrideSource::GlobalConfig,
  )];
  let known = inputs(&["nixpkgs", "flake-parts"]);
  let out = resolve(&entries, Some(&known)).expect("resolves");
  assert!(out.is_empty(), "a global entry must be silent elsewhere");
}

#[test]
fn override_naming_a_present_input_survives() {
  let entries = vec![Entry::new(
    "nix-lib",
    "github:Dauliac/nix-lib",
    OverrideSource::GlobalConfig,
  )];
  let known = inputs(&["nixpkgs", "nix-lib"]);
  let out = resolve(&entries, Some(&known)).expect("resolves");
  assert_eq!(out.len(), 1);
}

#[test]
// BDD: tests/features/21_override_input_config.feature#
// a-transitive-input-is-addressable
fn transitive_key_is_checked_against_the_parent_only() {
  let entries = vec![Entry::new(
    "nix-lib/flake-parts",
    "github:hercules-ci/flake-parts",
    OverrideSource::GlobalConfig,
  )];
  // `flake-parts` is not a root input; `nix-lib` is. The parent decides.
  let known = inputs(&["nix-lib"]);
  let out = resolve(&entries, Some(&known)).expect("resolves");
  assert_eq!(out.len(), 1);
  assert_eq!(out[0].name, "nix-lib/flake-parts");
}

#[test]
fn unknown_input_set_disables_filtering() {
  let entries = vec![Entry::new(
    "nix-lib",
    "github:Dauliac/nix-lib",
    OverrideSource::GlobalConfig,
  )];
  // `None` means "could not read flake.lock", which must not drop everything.
  assert_eq!(resolve(&entries, None).expect("resolves").len(), 1);
}

#[test]
fn flake_input_names_reads_root_inputs() {
  let tmp = TempDir::new().expect("tempdir");
  fs::write(
    tmp.path().join("flake.lock"),
    r#"{"nodes":{"root":{"inputs":{"nixpkgs":"nixpkgs","nix-lib":"nix-lib"}},
       "nixpkgs":{"locked":{}},"nix-lib":{"locked":{}}},"root":"root",
       "version":7}"#,
  )
  .expect("write lock");

  let got = flake_input_names(tmp.path()).expect("reads");
  assert_eq!(got, inputs(&["nixpkgs", "nix-lib"]));
}

#[test]
fn flake_input_names_is_none_without_a_lock() {
  let tmp = TempDir::new().expect("tempdir");
  assert!(flake_input_names(tmp.path()).is_none());
}

#[test]
fn flake_input_names_is_none_on_malformed_lock() {
  let tmp = TempDir::new().expect("tempdir");
  fs::write(tmp.path().join("flake.lock"), "not json").expect("write");
  assert!(flake_input_names(tmp.path()).is_none());
}

// ---------------------------------------------------------------- flattening

#[test]
fn flat_pairs_round_trip() {
  let resolved = vec![
    Resolved {
      name:     "a".into(),
      flakeref: "path:/x".into(),
      path:     Some(PathBuf::from("/x")),
      source:   OverrideSource::GlobalConfig,
    },
    Resolved {
      name:     "b".into(),
      flakeref: "github:o/r".into(),
      path:     None,
      source:   OverrideSource::GlobalConfig,
    },
  ];
  let flat = to_flat_pairs(&resolved);
  assert_eq!(flat, vec!["a", "path:/x", "b", "github:o/r"]);

  let back = entries_from_flat_pairs(&flat, Path::new("/cwd"));
  assert_eq!(back.len(), 2);
  assert_eq!(back[0].name, "a");
  assert_eq!(back[0].value, "path:/x");
  assert_eq!(back[1].name, "b");
}

#[test]
fn flat_pairs_ignore_a_trailing_odd_element() {
  let back = entries_from_flat_pairs(
    &[
      "a".to_string(),
      "path:/x".to_string(),
      "dangling".to_string(),
    ],
    Path::new("/cwd"),
  );
  assert_eq!(back.len(), 1);
}

// ------------------------------------------------- 5.4/5.5 gates (6.6)

fn ctx_with<'a>(
  global: &'a BTreeMap<String, String>,
  cwd: &'a Path,
  kind: nix_command::CommandKind,
  flake_subcommand: Option<&'a str>,
) -> ResolveContext<'a> {
  ResolveContext {
    global,
    project: &[],
    env_raw: None,
    cli_pairs: &[],
    flake_dir: None,
    cwd,
    kind,
    flake_subcommand,
  }
}

fn one_global() -> BTreeMap<String, String> {
  let mut m = BTreeMap::new();
  m.insert("nix-lib".to_string(), "github:Dauliac/nix-lib".to_string());
  m
}

#[test]
// BDD: tests/features/21_override_input_config.feature#
// flake-update-still-updates-the-lock
fn lock_writing_flake_subcommands_get_no_override() {
  // `--override-input` implies `--no-write-lock-file`, so injecting it here
  // would turn `nix flake update` into a silent no-op.
  for sub in ["update", "lock"] {
    let global = one_global();
    let ctx = ctx_with(
      &global,
      Path::new("/cwd"),
      nix_command::CommandKind::Flake,
      Some(sub),
    );
    let out = resolve_for_command(&ctx).expect("resolves");

    assert!(out.resolved.is_empty(), "{sub} must get no override");
    // And it must say so, otherwise the user cannot tell.
    let reason = out.suppressed.expect("suppression must be reported");
    assert!(reason.contains(sub), "reason should name {sub}: {reason}");
  }
}

#[test]
fn non_lock_writing_flake_subcommands_still_get_overrides() {
  for sub in ["check", "show", "metadata"] {
    let global = one_global();
    let ctx = ctx_with(
      &global,
      Path::new("/cwd"),
      nix_command::CommandKind::Flake,
      Some(sub),
    );
    let out = resolve_for_command(&ctx).expect("resolves");
    assert_eq!(out.resolved.len(), 1, "{sub} should keep its override");
    assert!(out.suppressed.is_none());
  }
}

#[test]
fn suppression_is_silent_when_nothing_was_configured() {
  // Nothing to suppress, so no noise on every `xi update`.
  let empty = BTreeMap::new();
  let ctx = ctx_with(
    &empty,
    Path::new("/cwd"),
    nix_command::CommandKind::Flake,
    Some("update"),
  );
  let out = resolve_for_command(&ctx).expect("resolves");
  assert!(out.suppressed.is_none());
}

#[test]
// BDD: tests/features/21_override_input_config.feature#
// store-commands-never-receive-flake-flags
fn store_level_kinds_never_receive_the_flag() {
  use nix_command::CommandKind as K;
  for kind in [K::Store, K::Copy, K::PathInfo, K::Config] {
    let global = one_global();
    let ctx = ctx_with(&global, Path::new("/cwd"), kind, None);
    let out = resolve_for_command(&ctx).expect("resolves");
    assert!(out.resolved.is_empty(), "{kind:?} must get no override");
  }
}

#[test]
fn flake_taking_kinds_do_accept_the_flag() {
  use nix_command::CommandKind as K;
  for kind in [K::Build, K::Develop, K::Run, K::Eval, K::Shell, K::Fmt] {
    assert!(
      kind.accepts_flake_overrides(),
      "{kind:?} should accept overrides"
    );
  }
}

#[test]
fn env_value_participates_in_resolution() {
  let (tmp, dir) = flake_dir("nix-lib");
  let empty = BTreeMap::new();
  let ctx = ResolveContext {
    global:           &empty,
    project:          &[],
    env_raw:          Some("nix-lib=./nix-lib"),
    cli_pairs:        &[],
    flake_dir:        Some(tmp.path()),
    cwd:              Path::new("/elsewhere"),
    kind:             nix_command::CommandKind::Build,
    flake_subcommand: None,
  };
  let out = resolve_for_command(&ctx).expect("resolves");

  assert_eq!(out.resolved.len(), 1);
  // Resolved against the flake root, not the unrelated CWD.
  assert_eq!(out.resolved[0].path.as_deref(), Some(dir.as_path()));
}

#[test]
fn a_bad_env_value_is_an_error_not_a_silent_drop() {
  let empty = BTreeMap::new();
  let ctx = ResolveContext {
    global:           &empty,
    project:          &[],
    env_raw:          Some("no-equals-sign"),
    cli_pairs:        &[],
    flake_dir:        None,
    cwd:              Path::new("/cwd"),
    kind:             nix_command::CommandKind::Build,
    flake_subcommand: None,
  };
  assert!(resolve_for_command(&ctx).is_err());
}

#[test]
fn nothing_configured_resolves_to_nothing() {
  let empty = BTreeMap::new();
  let ctx = ctx_with(
    &empty,
    Path::new("/cwd"),
    nix_command::CommandKind::Build,
    None,
  );
  let out = resolve_for_command(&ctx).expect("resolves");
  assert!(out.resolved.is_empty());
  assert!(out.suppressed.is_none());
}

// ------------------------------------------------- flake dir discovery

#[test]
fn discover_walks_up_to_the_nearest_flake() {
  let (tmp, dir) = flake_dir("proj");
  let nested = dir.join("crates").join("xi");
  fs::create_dir_all(&nested).expect("create nested");

  assert_eq!(discover_flake_dir(&nested).as_deref(), Some(dir.as_path()));
  // And nothing is found above a directory with no flake.nix at all.
  let bare = tmp.path().join("bare");
  fs::create_dir_all(&bare).expect("create bare");
  assert_ne!(discover_flake_dir(&bare).as_deref(), Some(bare.as_path()));
}

#[test]
fn resolution_is_deterministic_by_name() {
  // Sorted iteration keeps the report and the argv stable for snapshots.
  let entries = vec![
    Entry::new("z", "github:o/z", OverrideSource::GlobalConfig),
    Entry::new("a", "github:o/a", OverrideSource::GlobalConfig),
    Entry::new("m", "github:o/m", OverrideSource::GlobalConfig),
  ];
  let out = resolve(&entries, None).expect("resolves");
  let names: Vec<_> = out.iter().map(|r| r.name.as_str()).collect();
  assert_eq!(names, vec!["a", "m", "z"]);
}
