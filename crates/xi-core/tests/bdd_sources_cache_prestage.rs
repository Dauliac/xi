//! BDD: contract for the `xi-sources-cache-prestage` capability
//! (openspec/changes/xi-sources-cache-prestage/specs/…/spec.md).
//!
//! One test per SC-K-* criterion in
//! `openspec/changes/xi-sources-cache-prestage/proposal.md`, named after
//! it, so a failure here names exactly one broken requirement.
//!
//! The capability's four outside edges are all traits:
//! [`AttrReader`] (`nix eval`), [`KeyAdvertiser`] (HTTPS),
//! [`DaemonReload`] (systemd), and [`ConfPaths::under`] (the
//! filesystem). Every one is injected here, so no test needs `nix`
//! installed, a network, or write access to `/etc/nix` - and the
//! assertions are about xi's decisions rather than about whether the
//! machine running them happens to be a NixOS box.
//!
//! The reversal criterion (SC-K-006) is the one worth reading twice:
//! "byte-identical to pre-prestage state" includes the case where there
//! was no `nix.conf` at all, which an empty file would not satisfy.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{cell::RefCell, collections::BTreeMap, fs};

use serial_test::serial;
use tempfile::TempDir;
use xi_core::sources::{
  Scope,
  cache_prestage::{
    AmendOutcome,
    AttrReader,
    CacheSettings,
    ConfPaths,
    DaemonReload,
    INCLUDE_LINE,
    KeyAdvertiser,
    Prestage,
    PrestageError,
    PrestageOptions,
    PrestageStatus,
    XI_CONF_NAME,
    candidate_attr_paths,
    parse_xi_conf,
    scope_needs_reload,
  },
  lock::{ActorName, LOCK_FILE, LockOptions, SourcesLock},
};

/// Sets `XI_SOURCES_LOCK_WAIT` for the duration of a test and restores
/// whatever was there before on drop.
///
/// Mutating process env is only safe here because every test that uses
/// this is `#[serial]`: `LockOptions::from_env` reads `std::env::var`
/// directly (it has no closure-injection escape hatch the way
/// `xi_core::sources::config` does), so there is no way to exercise
/// xi-wtrq's lock-contention path without it.
struct LockWaitEnv {
  previous: Option<String>,
}

impl LockWaitEnv {
  fn set(value: &str) -> Self {
    let previous = std::env::var("XI_SOURCES_LOCK_WAIT").ok();
    // SAFETY: serialised against every other mutator of this process's
    // environment by `#[serial]` on every test that constructs this.
    unsafe {
      std::env::set_var("XI_SOURCES_LOCK_WAIT", value);
    }
    Self { previous }
  }
}

impl Drop for LockWaitEnv {
  fn drop(&mut self) {
    // SAFETY: see `LockWaitEnv::set`.
    unsafe {
      match &self.previous {
        Some(value) => std::env::set_var("XI_SOURCES_LOCK_WAIT", value),
        None => std::env::remove_var("XI_SOURCES_LOCK_WAIT"),
      }
    }
  }
}

const CACHE: &str = "https://cache.example.org";
const KEY: &str =
  "cache.example.org-1:QmFzZTY0S2V5VmFsdWVIZXJlRm9yVGVzdGluZz0=";
const FLAKE: &str = "/srv/nix-config";

// ---------------------------------------------------------------------------
// Doubles for the four edges
// ---------------------------------------------------------------------------

/// An `AttrReader` answering from a fixture table.
///
/// Anything not in the table reads as "no such attribute", which is how
/// the real reader reports a path a flake does not define: probing six
/// candidates means five are *expected* to miss.
struct TableReader {
  answers: BTreeMap<String, String>,
  /// Paths asked for, in order, so a test can assert the probe order.
  asked:   RefCell<Vec<String>>,
  /// When set, every read fails with this message instead.
  fails:   Option<String>,
}

impl TableReader {
  fn new(pairs: &[(&str, &str)]) -> Self {
    Self {
      answers: pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect(),
      asked:   RefCell::new(Vec::new()),
      fails:   None,
    }
  }

  fn failing(message: &str) -> Self {
    Self {
      answers: BTreeMap::new(),
      asked:   RefCell::new(Vec::new()),
      fails:   Some(message.to_owned()),
    }
  }

  /// A flake declaring one cache and one key at `attr`.
  fn declaring(attr: &str) -> Self {
    Self::new(&[
      (&format!("{attr}.substituters"), CACHE),
      (&format!("{attr}.trusted-public-keys"), KEY),
    ])
  }
}

impl AttrReader for TableReader {
  fn read(
    &self,
    _flake_ref: &str,
    attr_path: &str,
  ) -> Result<Option<String>, String> {
    if let Some(message) = self.fails.as_ref() {
      return Err(message.clone());
    }
    self.asked.borrow_mut().push(attr_path.to_owned());
    Ok(self.answers.get(attr_path).cloned())
  }
}

/// A `KeyAdvertiser` that vouches for whatever it is told to.
struct Advertiser {
  keys:  Vec<String>,
  fails: Option<String>,
}

impl Advertiser {
  fn vouching_for(key: &str) -> Self {
    Self {
      keys:  vec![key.to_owned()],
      fails: None,
    }
  }

  fn unreachable() -> Self {
    Self {
      keys:  Vec::new(),
      fails: Some("host unreachable".to_owned()),
    }
  }
}

impl KeyAdvertiser for Advertiser {
  fn advertised_keys(&self, _substituter: &str) -> Result<Vec<String>, String> {
    match self.fails.as_ref() {
      Some(message) => Err(message.clone()),
      None => Ok(self.keys.clone()),
    }
  }
}

/// A `DaemonReload` that records whether it was asked.
#[derive(Default)]
struct Reloader {
  called: RefCell<usize>,
}

impl DaemonReload for Reloader {
  fn reload(&self) -> Result<(), String> {
    *self.called.borrow_mut() += 1;
    Ok(())
  }
}

/// One machine: a `nix` directory, a state directory, and the doubles.
struct Machine {
  _tmp:      TempDir,
  paths:     ConfPaths,
  /// Also where the sources lock lives for this fixture: a test's
  /// `Scope::System` must never resolve to the real `/var/lib/xi/state`.
  state_dir: std::path::PathBuf,
  reload:    Reloader,
}

impl Machine {
  fn new() -> Self {
    let tmp = TempDir::new().expect("tempdir");
    let nix_dir = tmp.path().join("nix");
    let state_dir = tmp.path().join("state");
    for dir in [&nix_dir, &state_dir] {
      fs::create_dir_all(dir).expect("mkdir");
    }
    Self {
      paths: ConfPaths::under(&nix_dir, &state_dir),
      state_dir,
      _tmp: tmp,
      reload: Reloader::default(),
    }
  }

  fn prestage<'a>(
    &'a self,
    reader: &'a dyn AttrReader,
    advertiser: &'a dyn KeyAdvertiser,
    scope: Scope,
  ) -> Prestage<'a> {
    Prestage {
      scope,
      paths: self.paths.clone(),
      lock_state_dir: self.state_dir.clone(),
      reader,
      advertiser,
      reload: &self.reload,
    }
  }

  fn nix_conf(&self) -> Option<String> {
    fs::read_to_string(&self.paths.nix_conf).ok()
  }

  fn xi_conf(&self) -> Option<String> {
    fs::read_to_string(&self.paths.xi_conf).ok()
  }

  fn reloads(&self) -> usize {
    *self.reload.called.borrow()
  }
}

/// The candidate list a NixOS host would probe.
fn candidates() -> Vec<String> {
  candidate_attr_paths("box", "alice")
}

/// The attribute path a NixOS host's settings live at.
fn nixos_attr() -> String {
  r#"nixosConfigurations."box".config.nix.settings"#.to_owned()
}

// ---------------------------------------------------------------------------
// SC-K-001 / SC-K-002: the write, and its idempotence
// ---------------------------------------------------------------------------

/// SC-K-001: prestage writes the fragment with what the flake declared,
/// and nothing is built to find it out.
#[test]
fn sc_k_001_prestage_writes_the_declared_caches_without_building() {
  let m = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);

  let report = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  assert_eq!(report.status, PrestageStatus::Applied);
  assert_eq!(report.attr_path.as_deref(), Some(nixos_attr().as_str()));
  assert_eq!(report.settings.substituters, [CACHE]);
  assert_eq!(report.settings.trusted_public_keys, [KEY]);

  let fragment = m.xi_conf().expect("the fragment must be written");
  let parsed = parse_xi_conf(&fragment);
  assert_eq!(
    parsed, report.settings,
    "the fragment must round-trip to exactly what was reported"
  );

  // The reader was asked only for attributes, never for a derivation:
  // "without building anything" is the criterion, and a build would
  // have to go through a different seam entirely.
  let asked = reader.asked.borrow();
  assert!(
    asked.iter().all(|a| {
      a.ends_with(".substituters") || a.ends_with(".trusted-public-keys")
    }),
    "discovery must read only the two settings: {asked:?}"
  );
}

/// SC-K-002: the include line is added once, and a second run is a
/// no-op that writes nothing and reloads nothing.
#[test]
fn sc_k_002_the_include_is_added_once_and_prestage_is_idempotent() {
  let m = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);
  let opts = PrestageOptions::default();

  let first = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &opts)
    .expect("first prestage");
  assert_eq!(first.status, PrestageStatus::Applied);
  assert_eq!(first.amend, Some(AmendOutcome::Added));

  let conf = m.nix_conf().expect("nix.conf must exist after the amend");
  assert_eq!(conf.lines().filter(|l| l.trim() == INCLUDE_LINE).count(), 1);
  let after_first = (conf.clone(), m.xi_conf(), m.reloads());

  let second = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &opts)
    .expect("second prestage");

  assert_eq!(
    second.status,
    PrestageStatus::Unchanged,
    "a second run with the same settings has nothing to do"
  );
  assert_eq!(
    (m.nix_conf().expect("still there"), m.xi_conf(), m.reloads()),
    after_first,
    "an unchanged run must not rewrite the files or reload the daemon"
  );
  assert_eq!(
    m.nix_conf()
      .expect("still there")
      .lines()
      .filter(|l| l.trim() == INCLUDE_LINE)
      .count(),
    1,
    "the include line must not be duplicated"
  );
}

/// SC-K-002, pre-existing-file half: an operator's own `nix.conf` keeps
/// its content, and gains exactly one line.
#[test]
fn sc_k_002_an_existing_nix_conf_keeps_its_content() {
  let m = Machine::new();
  // Deliberately without a trailing newline: appending to that must not
  // join xi's line onto the operator's last one.
  let original = "experimental-features = nix-command flakes";
  fs::write(m.paths.nix_conf.clone(), original).expect("write nix.conf");

  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);
  m.prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  let conf = m.nix_conf().expect("nix.conf");
  assert!(
    conf.starts_with(original),
    "the operator's content must be preserved verbatim: {conf:?}"
  );
  assert!(
    conf.contains(&format!("\n{INCLUDE_LINE}")),
    "the include must be on its own line: {conf:?}"
  );
}

// ---------------------------------------------------------------------------
// SC-K-003: what Nix will actually read
// ---------------------------------------------------------------------------

/// SC-K-003: the values Nix will read back are the ones that were
/// declared.
///
/// Asserted on the fragment plus the include, which is what `nix
/// show-config` reads, rather than by running `nix`: the criterion is
/// about what xi put in Nix's path, and spawning Nix here would test
/// the machine's Nix instead.
#[test]
fn sc_k_003_the_effective_config_reflects_the_declared_values() {
  let m = Machine::new();
  // The key's name has to be claimed by one of the declared
  // substituters, or the guard refuses it before anything is written -
  // which is its job, and is asserted in the SC-K-005 scenarios.
  const ONE_KEY: &str = "one.example.org-1:QmFzZTY0S2V5VmFsdWVIZXJlRm9yVGVzdA=";
  let reader = TableReader::new(&[
    (
      &format!("{}.substituters", nixos_attr()),
      "https://one.example.org https://two.example.org",
    ),
    (&format!("{}.trusted-public-keys", nixos_attr()), ONE_KEY),
  ]);
  let advertiser = Advertiser::vouching_for(ONE_KEY);

  m.prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  let settings = parse_xi_conf(&m.xi_conf().expect("fragment"));
  assert_eq!(settings.substituters, [
    "https://one.example.org",
    "https://two.example.org"
  ]);

  // The include is what makes the fragment effective; the status verb
  // reports exactly that pairing rather than the fragment alone.
  let status = m
    .prestage(&reader, &advertiser, Scope::System)
    .status()
    .expect("status");
  assert!(status.active, "fragment plus include means in effect");
  assert_eq!(status.xi_conf.file_name().expect("name"), XI_CONF_NAME);
}

// ---------------------------------------------------------------------------
// SC-K-004: the fallbacks, and the clean exit
// ---------------------------------------------------------------------------

/// SC-K-004: a home-manager-only flake is found through the
/// home-manager candidates rather than missed.
#[test]
fn sc_k_004_a_home_manager_only_flake_is_found_by_fallback() {
  let m = Machine::new();
  let attr = r#"homeConfigurations."alice".config.nix.settings"#;
  let reader = TableReader::declaring(attr);
  let advertiser = Advertiser::vouching_for(KEY);

  let report = m
    .prestage(&reader, &advertiser, Scope::User)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  assert_eq!(report.status, PrestageStatus::Applied);
  assert_eq!(report.attr_path.as_deref(), Some(attr));

  // The NixOS path is probed first and misses, which is the normal
  // case this capability is built around rather than a failure.
  let asked = reader.asked.borrow();
  assert!(
    asked[0].starts_with("nixosConfigurations"),
    "the probe order must start at the system scope: {asked:?}"
  );
}

/// SC-K-004, flake-level half: the `nixCache.settings` convention is
/// the last resort for a repo with no NixOS and no home-manager module.
#[test]
fn sc_k_004_the_flake_level_convention_is_the_last_resort() {
  let m = Machine::new();
  let reader = TableReader::declaring("nixCache.settings");
  let advertiser = Advertiser::vouching_for(KEY);

  let report = m
    .prestage(&reader, &advertiser, Scope::User)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  assert_eq!(report.attr_path.as_deref(), Some("nixCache.settings"));
  assert_eq!(
    candidates().last().map(String::as_str),
    Some("nixCache.settings"),
    "it is the last resort, so it must be probed last"
  );
}

/// SC-K-004, nothing-declared half: a flake with no caches exits
/// cleanly as `Missing` and writes no fragment.
#[test]
fn sc_k_004_a_flake_declaring_nothing_is_missing_and_writes_no_file() {
  let m = Machine::new();
  let reader = TableReader::new(&[]);
  let advertiser = Advertiser::vouching_for(KEY);

  let report = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("a flake with no caches is not an error");

  assert_eq!(report.status, PrestageStatus::Missing);
  assert_eq!(report.attr_path, None);
  assert!(report.settings.is_empty());
  assert_eq!(report.amend, None, "nothing to include, nothing amended");
  assert!(!report.reloaded);
  assert!(m.xi_conf().is_none(), "no fragment may be written");
  assert!(m.nix_conf().is_none(), "nix.conf must not be created");
  assert_eq!(m.reloads(), 0);

  // Every candidate was tried before giving up.
  assert_eq!(
    reader.asked.borrow().len(),
    candidates().len(),
    "`Missing` means every well-known path was probed"
  );
}

/// A reader that fails for a reason other than "no such attribute" is
/// an error, not `Missing`: "xi could not ask" and "the flake declares
/// nothing" are different problems for the operator.
#[test]
fn a_reader_failure_is_an_error_rather_than_a_missing_status() {
  let m = Machine::new();
  let reader = TableReader::failing("error: access to absolute path forbidden");
  let advertiser = Advertiser::vouching_for(KEY);

  let err = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect_err("an unreadable flake is an error");

  assert!(matches!(err, PrestageError::Reader(_)), "got {err:?}");
  assert!(m.xi_conf().is_none(), "a failed read must write nothing");
}

// ---------------------------------------------------------------------------
// SC-K-005: the declared-keys guard
// ---------------------------------------------------------------------------

/// SC-K-005: a key no host vouches for is refused, and nothing is
/// written.
#[test]
fn sc_k_005_an_unverifiable_key_is_refused_and_writes_nothing() {
  let m = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::unreachable();

  let err = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect_err("an unvouched key must be refused");

  let PrestageError::UnverifiedKeys { keys } = &err else {
    panic!("expected UnverifiedKeys, got {err:?}");
  };
  assert_eq!(keys.len(), 1);
  assert_eq!(keys[0].key, KEY);

  let message = err.to_string();
  assert!(
    message.contains("code-signing authority"),
    "the refusal must say what accepting the key would mean: {message}"
  );
  assert!(
    message.contains("--trust-declared-keys"),
    "the refusal must name the escape hatch: {message}"
  );

  assert!(
    m.xi_conf().is_none() && m.nix_conf().is_none(),
    "a refused prestage must leave the machine untouched"
  );
  assert_eq!(m.reloads(), 0);
}

/// SC-K-005, bypass half: `--trust-declared-keys` writes, and reports
/// every key it waved through so the decision is on the record.
#[test]
fn sc_k_005_trust_declared_keys_bypasses_and_records_what_it_waved_through() {
  let m = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::unreachable();

  let report = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions {
      trust_declared_keys: true,
    })
    .expect("the bypass must let the write through");

  assert_eq!(report.status, PrestageStatus::Applied);
  assert_eq!(
    report.trusted_as_declared,
    [KEY],
    "the envelope must name every key accepted on the operator's word"
  );
  assert_eq!(
    parse_xi_conf(&m.xi_conf().expect("fragment")).trusted_public_keys,
    [KEY]
  );
}

/// SC-K-005, the normal path: the guard reports nothing when every key
/// was actually vouched for, so a non-empty list always means a bypass.
#[test]
fn sc_k_005_a_verified_key_is_not_reported_as_waved_through() {
  let m = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);

  let report = m
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  assert!(
    report.trusted_as_declared.is_empty(),
    "a verified key must not look like a bypassed one"
  );
}

// ---------------------------------------------------------------------------
// SC-K-006: reversal
// ---------------------------------------------------------------------------

/// SC-K-006: unstage removes the include and the fragment, and leaves
/// `nix.conf` byte-identical to what it was.
#[test]
fn sc_k_006_unstage_restores_nix_conf_byte_for_byte() {
  let m = Machine::new();
  let original = "experimental-features = nix-command flakes\nmax-jobs = 4\n";
  fs::write(m.paths.nix_conf.clone(), original).expect("write nix.conf");
  let before = fs::read(&m.paths.nix_conf).expect("read before");

  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);

  m.prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");
  assert_ne!(
    fs::read(&m.paths.nix_conf).expect("read amended"),
    before,
    "the prestage really did change the file, so the restore is not vacuous"
  );

  let report = m
    .prestage(&reader, &advertiser, Scope::System)
    .unstage()
    .expect("unstage");

  assert!(report.include_removed);
  assert!(report.xi_conf_removed);
  assert_eq!(
    fs::read(&m.paths.nix_conf).expect("read restored"),
    before,
    "nix.conf must be byte-identical to its pre-prestage state"
  );
  assert!(m.xi_conf().is_none(), "the fragment must be gone");
  assert!(
    !m.paths.receipt.exists(),
    "the receipt is consumed by the reversal"
  );
}

/// SC-K-006, no-file half: a machine that had no `nix.conf` must not be
/// left with an empty one, which is not byte-identical to absent.
#[test]
fn sc_k_006_a_machine_with_no_nix_conf_gets_none_back() {
  let m = Machine::new();
  assert!(m.nix_conf().is_none(), "precondition: no nix.conf");

  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);
  m.prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");
  assert!(m.nix_conf().is_some(), "the prestage created it");

  m.prestage(&reader, &advertiser, Scope::System)
    .unstage()
    .expect("unstage");

  assert!(
    m.nix_conf().is_none(),
    "an empty file is not the same as no file; the path must be removed"
  );
}

/// SC-K-006, refusal half: a `nix.conf` the operator has edited since
/// the prestage is not restored, because removing the line would not
/// reproduce what the receipt recorded.
#[test]
fn sc_k_006_an_edited_nix_conf_is_refused_rather_than_clobbered() {
  let m = Machine::new();
  fs::write(m.paths.nix_conf.clone(), "max-jobs = 4\n").expect("write");

  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);
  m.prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  // The operator edits the file afterwards.
  let edited = format!(
    "{}cores = 0\n",
    m.nix_conf().expect("nix.conf after prestage")
  );
  fs::write(m.paths.nix_conf.clone(), &edited).expect("operator edit");

  let err = m
    .prestage(&reader, &advertiser, Scope::System)
    .unstage()
    .expect_err("an edited nix.conf must not be clobbered");

  assert!(
    matches!(err, PrestageError::NixConfModified { .. }),
    "got {err:?}"
  );
  assert_eq!(
    m.nix_conf().as_deref(),
    Some(edited.as_str()),
    "the operator's edit must survive the refusal"
  );
  assert!(
    m.xi_conf().is_some(),
    "a refused unstage must not delete the fragment either"
  );
}

/// Unstage with nothing to reverse is a named error, not a silent
/// success that would make a scripted rollback look like it worked.
#[test]
fn unstaging_without_a_prestage_is_a_named_error() {
  let m = Machine::new();
  let reader = TableReader::new(&[]);
  let advertiser = Advertiser::vouching_for(KEY);

  let err = m
    .prestage(&reader, &advertiser, Scope::System)
    .unstage()
    .expect_err("there is nothing to reverse");
  assert!(
    matches!(err, PrestageError::NotPrestaged { .. }),
    "got {err:?}"
  );
}

// ---------------------------------------------------------------------------
// SC-K-007: the daemon reload
// ---------------------------------------------------------------------------

/// SC-K-007: a system-scope prestage reloads the daemon, because the
/// next build reads the config through it; a user-scope one does not,
/// because there is no daemon in that path to reload.
#[test]
fn sc_k_007_only_a_system_scope_prestage_reloads_the_daemon() {
  assert!(scope_needs_reload(Scope::System));
  assert!(!scope_needs_reload(Scope::User));

  let system = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);

  let report = system
    .prestage(&reader, &advertiser, Scope::System)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");
  assert!(
    report.reloaded,
    "the envelope must say the daemon was reloaded"
  );
  assert_eq!(system.reloads(), 1);

  let user = Machine::new();
  let user_reader =
    TableReader::declaring(r#"homeConfigurations."alice".config.nix.settings"#);
  let report = user
    .prestage(&user_reader, &advertiser, Scope::User)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");
  assert!(!report.reloaded);
  assert_eq!(
    user.reloads(),
    0,
    "a user-scope prestage has no daemon to reload"
  );
}

/// The fragment round-trips through its own renderer, so a value an
/// operator reads out of `xi.conf` is the value xi put in.
#[test]
fn the_fragment_round_trips_through_its_renderer() {
  let settings = CacheSettings {
    substituters:        vec![
      "https://one.example.org".to_owned(),
      "https://two.example.org".to_owned(),
    ],
    trusted_public_keys: vec![KEY.to_owned()],
  };
  let rendered = xi_core::sources::cache_prestage::render_xi_conf(&settings);
  assert_eq!(parse_xi_conf(&rendered), settings);

  // An empty fragment parses to empty rather than to a one-element list
  // of the empty string, which is what a naive split would give.
  assert!(parse_xi_conf("").is_empty());
}

// ---------------------------------------------------------------------------
// xi-wtrq: cache prestage takes the sources lock
// ---------------------------------------------------------------------------

/// xi-wtrq: `Prestage::run` takes the sources lock (`ActorName::CachePrestage`)
/// before reading the flake and releases it once it returns.
#[test]
fn prestage_takes_the_sources_lock_and_releases_it() {
  let machine = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);

  machine
    .prestage(&reader, &advertiser, Scope::User)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect("prestage");

  let lock_path = machine.state_dir.join(LOCK_FILE);
  assert!(
    lock_path.is_file(),
    "run must have taken the lock, which creates {}",
    lock_path.display()
  );
  assert_eq!(
    fs::read_to_string(&lock_path).expect("read lock").trim(),
    "",
    "a released lock leaves an empty record"
  );
}

/// A prestage cannot run while another actor holds the sources lock: the
/// error names the holder rather than racing it.
#[test]
#[serial]
fn prestage_refuses_a_held_lock_and_names_the_holder() {
  let _env = LockWaitEnv::set("50ms");
  let machine = Machine::new();
  let reader = TableReader::declaring(&nixos_attr());
  let advertiser = Advertiser::vouching_for(KEY);

  let held = SourcesLock::acquire(
    &machine.state_dir,
    &LockOptions::new(ActorName::Daemon, "tick").without_refresh(),
  )
  .expect("the test takes the free lock first");

  let err = machine
    .prestage(&reader, &advertiser, Scope::User)
    .run(FLAKE, &candidates(), &PrestageOptions::default())
    .expect_err("a held lock must refuse the run");
  let PrestageError::Lock(message) = &err else {
    panic!("expected PrestageError::Lock, got {err:?}");
  };
  assert!(message.contains("daemon"), "got: {message}");
  assert!(
    machine.xi_conf().is_none(),
    "a run refused on the lock must not have written anything"
  );

  held.release().expect("release");
}
