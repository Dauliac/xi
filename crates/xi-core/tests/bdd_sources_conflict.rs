//! BDD: contract for the `xi-sources-conflict` capability
//! (openspec/changes/xi-sources-conflict/specs/xi-sources-conflict/spec.md).
//!
//! One test per SC-X-* criterion in
//! `openspec/changes/xi-sources-conflict/proposal.md`, named after it, so
//! a failure here names exactly one broken requirement.
//!
//! These drive real git repositories in a temp dir rather than the pure
//! relation table the unit tests in `sources::conflict` cover: the
//! criteria are about what happens to files on disk, and a fake repo
//! cannot show that the sync left `.git` alone or that the state dir came
//! out byte-identical.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{
  collections::BTreeMap,
  fs,
  path::{Path, PathBuf},
  process::Command,
};

use tempfile::TempDir;
use xi_core::sources::{
  GitopsHealth,
  SOURCES_SCHEMA_VERSION,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatus,
  config::SyncMode,
  conflict::{
    AutoSync,
    ConflictError,
    EvaluationInput,
    EvaluationSource,
    FORCE_DIRTY_FLAG,
    ObservedRefs,
    PostAdvance,
    SourcesDiffResponse,
    SyncOutcome,
    SyncRequest,
    VerbStatus,
    apply_auto_sync,
    check_working_dir_git,
    check_write_target,
    post_advance,
    resolve_evaluation_source,
    sync,
    three_way_diff,
  },
  write_state_file,
};

const REMOTE: &str = "https://git.example/config";

/// What `xi <scope> switch` would name in a refusal.
const WHAT: &str = "xi home switch";

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn git(at: &Path, args: &[&str]) -> String {
  let out = Command::new("git")
    .arg("-C")
    .arg(at)
    .args([
      "-c",
      "user.email=t@e",
      "-c",
      "user.name=t",
      "-c",
      "commit.gpgsign=false",
    ])
    .args(args)
    .output()
    .expect("run git");
  assert!(
    out.status.success(),
    "git {args:?} failed in {}: {}",
    at.display(),
    String::from_utf8_lossy(&out.stderr)
  );
  String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn head(at: &Path) -> String {
  git(at, &["rev-parse", "HEAD"])
}

/// Every file under `root`, `.git` excluded, as path to bytes.
///
/// The SC-X-012 comparison: a snapshot cheap enough to take before and
/// after a sync, and detailed enough that a single changed byte shows up.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
  fn walk(dir: &Path, base: &Path, into: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(dir).expect("read dir") {
      let path = entry.expect("dir entry").path();
      if path.file_name().is_some_and(|name| name == ".git") {
        continue;
      }
      if path.is_dir() {
        walk(&path, base, into);
      } else {
        let rel = path.strip_prefix(base).expect("under base").to_path_buf();
        into.insert(rel, fs::read(&path).expect("read file"));
      }
    }
  }

  let mut out = BTreeMap::new();
  if root.is_dir() {
    walk(root, root, &mut out);
  }
  out
}

/// The three-clone world the capability reasons about: a remote, the
/// sources dir xi owns, and the operator's working dir.
struct World {
  _tmp:        TempDir,
  remote:      PathBuf,
  sources_dir: PathBuf,
  state_dir:   PathBuf,
  working_dir: PathBuf,
  /// A file outside every xi-managed tree, for SC-X-012.
  bystander:   PathBuf,
}

impl World {
  /// A remote with one commit, cloned twice, with the state dir
  /// recording all three heads as the daemon would have left them.
  fn new() -> Self {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();

    let remote = root.join("remote");
    fs::create_dir_all(&remote).expect("mkdir remote");
    fs::write(remote.join("flake.nix"), "{ outputs = _: { }; }\n")
      .expect("write flake");
    fs::create_dir_all(remote.join("hosts")).expect("mkdir hosts");
    fs::write(remote.join("hosts/laptop.nix"), "{ }\n").expect("write host");
    git(&remote, &["init", "-q", "-b", "main"]);
    git(&remote, &["add", "-A"]);
    git(&remote, &["commit", "-q", "-m", "base"]);

    let sources_dir = root.join("sources");
    let working_dir = root.join("working");
    for dest in [&sources_dir, &working_dir] {
      let ok = Command::new("git")
        .args(["clone", "-q"])
        .arg(&remote)
        .arg(dest)
        .status()
        .expect("run git clone")
        .success();
      assert!(ok, "clone into {} failed", dest.display());
    }

    let state_dir = root.join("state");
    fs::create_dir_all(&state_dir).expect("mkdir state");

    let bystander = root.join("bystander.txt");
    fs::write(&bystander, b"untouched\n").expect("write bystander");

    let world = Self {
      _tmp: tmp,
      remote,
      sources_dir,
      state_dir,
      working_dir,
      bystander,
    };
    world.record_heads();
    world
  }

  /// What the daemon writes after a tick: the three refs as observed.
  fn record_heads(&self) {
    for (name, value) in [
      ("remote-head", head(&self.remote)),
      ("sources-head", head(&self.sources_dir)),
      ("working-head", head(&self.working_dir)),
    ] {
      write_state_file(&self.state_dir, name, &value).expect("write state");
    }
    write_state_file(
      &self.state_dir,
      "working-dir",
      &self.working_dir.display().to_string(),
    )
    .expect("write working-dir");
  }

  /// A commit on top of `repo`'s HEAD, touching `path`.
  fn commit(&self, repo: &Path, path: &str, body: &str, message: &str) {
    let full = repo.join(path);
    if let Some(parent) = full.parent() {
      fs::create_dir_all(parent).expect("mkdir commit parent");
    }
    fs::write(&full, body).expect("write commit file");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
  }

  /// The gitops daemon's tick: fetch the remote into the sources dir and
  /// fast-forward it. Writes nothing outside the sources dir.
  fn advance_sources_dir(&self) {
    git(&self.sources_dir, &["fetch", "-q", "origin", "main"]);
    git(&self.sources_dir, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    write_state_file(&self.state_dir, "sources-head", &head(&self.sources_dir))
      .expect("write sources-head");
    write_state_file(&self.state_dir, "remote-head", &head(&self.remote))
      .expect("write remote-head");
  }

  /// The operator fetching, which is theirs to run and is not on xi's
  /// code path (see `FORBIDDEN_WORKING_DIR_GIT`).
  ///
  /// This is the precondition for a sync to be *available* at all: the
  /// matrix asks `merge-base --is-ancestor` in the working dir, so a
  /// working dir that has never seen the sources dir's objects lands in
  /// `ConflictState::Unknown` rather than row 3, and the diff legs that
  /// span the two trees cannot be computed either. The fetch moves no
  /// ref the operator owns -- HEAD and the tree stay stale.
  fn operator_fetches(&self) {
    git(&self.working_dir, &["fetch", "-q", "origin"]);
  }

  fn context(&self) -> SourcesContext {
    SourcesContext::load(
      SourcesId::new(Scope::User, REMOTE),
      &self.sources_dir,
      &self.state_dir,
    )
    .expect("load context")
  }

  fn refs(&self) -> ObservedRefs {
    ObservedRefs::observe(
      &self.sources_dir,
      Some(&self.working_dir),
      &self.context().refs,
    )
  }

  /// `resolve_evaluation_source` for this world.
  ///
  /// `working` is `None` for the no-working-dir machine of SC-X-005.
  fn resolve(
    &self,
    working: Option<&Path>,
    force_dirty: bool,
  ) -> EvaluationSource {
    resolve_evaluation_source(&EvaluationInput {
      sources_dir: &self.sources_dir,
      working_dir: working,
      recorded: &self.context().refs,
      force_dirty,
      what: WHAT,
    })
  }

  fn sync(&self, force_dirty: bool) -> Result<SyncOutcome, ConflictError> {
    sync(&SyncRequest {
      sources_dir: &self.sources_dir,
      working_dir: &self.working_dir,
      state_dir: &self.state_dir,
      force_dirty,
      what: "xi home sources sync",
    })
  }
}

// ---------------------------------------------------------------------------
// SC-X-001 .. SC-X-005: the resolver
// ---------------------------------------------------------------------------

/// SC-X-001: a clean working dir ahead of the remote is evaluated, and
/// the resolver reaches no further than the three trees -- the recorded
/// `remote_head` is the only thing it knows about the remote, so there is
/// no in-line daemon tick to make.
#[test]
fn sc_x_001_a_clean_working_dir_ahead_of_remote_is_what_switch_evaluates() {
  let world = World::new();
  world.commit(
    &world.working_dir,
    "hosts/laptop.nix",
    "{ x = 1; }\n",
    "local",
  );

  // The remote moves on without the working dir being told, which is the
  // state a resolver that fetched in-line would report differently.
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote");

  match world.resolve(Some(&world.working_dir), false) {
    EvaluationSource::WorkingDir { path, dirty_source } => {
      assert_eq!(path, world.working_dir);
      assert!(!dirty_source, "a committed tree is not a dirty source");
    },
    other => panic!("expected the working dir, got {other:?}"),
  }

  // Nothing fetched: the sources dir is still on the base commit.
  assert_eq!(
    head(&world.sources_dir),
    world
      .context()
      .refs
      .sources_head
      .expect("recorded sources head"),
    "the resolver must not advance the sources dir in-line"
  );
}

/// SC-X-002: unstaged changes refuse, naming the files and the escape.
#[test]
fn sc_x_002_a_dirty_working_dir_refuses_and_names_the_files_and_the_escape() {
  let world = World::new();
  fs::write(world.working_dir.join("hosts/laptop.nix"), "{ y = 2; }\n")
    .expect("dirty the tree");

  let EvaluationSource::Error { reason } =
    world.resolve(Some(&world.working_dir), false)
  else {
    panic!("a dirty working dir must refuse");
  };
  assert!(
    reason.contains("hosts/laptop.nix"),
    "the refusal must name the dirty file: {reason}"
  );
  assert!(
    reason.contains(FORCE_DIRTY_FLAG),
    "the refusal must name the escape hatch: {reason}"
  );

  // 2.3: the escape hatch works, and says the generation is not
  // reproducible from any sha.
  match world.resolve(Some(&world.working_dir), true) {
    EvaluationSource::WorkingDir { dirty_source, .. } => {
      assert!(
        dirty_source,
        "{FORCE_DIRTY_FLAG} must mark the source dirty"
      );
    },
    other => panic!("expected a forced working dir, got {other:?}"),
  }
}

/// SC-X-003: the daemon's fetch never mutates the working dir, even when
/// the working dir lags behind.
#[test]
fn sc_x_003_a_daemon_tick_never_mutates_the_working_dir() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");

  let before = snapshot(&world.working_dir);
  let working_head_before = head(&world.working_dir);

  world.advance_sources_dir();

  assert_eq!(
    snapshot(&world.working_dir),
    before,
    "a tick that advanced the sources dir rewrote the working dir"
  );
  assert_eq!(
    head(&world.working_dir),
    working_head_before,
    "a tick moved the working dir's HEAD"
  );

  // The invariant behind it: the daemon's write guard is rooted at the
  // sources dir, so a path in the working dir is not writable at all.
  let escape = world.working_dir.join("hosts/laptop.nix");
  assert!(
    check_write_target(&world.sources_dir, &escape).is_err(),
    "the daemon's write guard must reject a working-dir path"
  );
}

/// SC-X-004: a force-pushed remote is rejected, the reason is
/// machine-readable, and the sources dir stays on the sha it had.
#[test]
fn sc_x_004_a_force_pushed_remote_is_rejected_and_sources_stays_put() {
  let world = World::new();
  let sources_head_before = head(&world.sources_dir);

  // Rewrite the remote's history: a new root commit, so the recorded
  // sources head is not an ancestor of it.
  git(&world.remote, &["checkout", "-q", "--orphan", "rewritten"]);
  world.commit(
    &world.remote,
    "flake.nix",
    "{ outputs = _: { }; }\n",
    "rewrite",
  );
  let rewritten = head(&world.remote);

  // The daemon fetched the objects but has not moved the sources dir.
  git(&world.sources_dir, &["fetch", "-q", "origin", "rewritten"]);
  write_state_file(&world.state_dir, "remote-head", &rewritten)
    .expect("record the rewritten remote head");

  let EvaluationSource::Error { reason } =
    world.resolve(Some(&world.working_dir), false)
  else {
    panic!("a force-pushed remote must refuse");
  };
  assert!(
    reason.contains("remote-force-push"),
    "the refusal must carry the machine-readable reason: {reason}"
  );
  assert!(
    reason.contains("Degraded"),
    "the refusal must report the Degraded status: {reason}"
  );
  assert_eq!(
    head(&world.sources_dir),
    sources_head_before,
    "the sources dir must be left on the previous sha"
  );
}

/// SC-X-005: with no working dir the sources dir is the only authority.
#[test]
fn sc_x_005_with_no_working_dir_the_sources_dir_is_the_authority() {
  let world = World::new();
  // Even a state that would be a refusal with a working dir: the
  // working dir is what diverges, and there isn't one.
  world.commit(
    &world.working_dir,
    "hosts/laptop.nix",
    "{ x = 1; }\n",
    "local",
  );

  assert_eq!(world.resolve(None, false), EvaluationSource::SourcesDir {
    path: world.sources_dir.clone(),
  });
}

// ---------------------------------------------------------------------------
// SC-X-006: `xi <scope> sources diff`
// ---------------------------------------------------------------------------

/// SC-X-006: three legs, in the spec's order, inside the house envelope.
#[test]
fn sc_x_006_diff_reports_three_legs_in_the_house_envelope() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");
  world.advance_sources_dir();
  world.operator_fetches();
  world.commit(
    &world.working_dir,
    "hosts/laptop.nix",
    "{ x = 1; }\n",
    "local",
  );

  let report =
    three_way_diff(&world.sources_dir, Some(&world.working_dir), &world.refs());
  let labels: Vec<&str> =
    report.sections.iter().map(|s| s.label.as_str()).collect();
  assert_eq!(
    labels,
    ["remote → sources", "sources → working", "remote → working"],
    "the legs must be the spec's three, in its order"
  );

  let response = SourcesDiffResponse::new(report);
  assert_eq!(response.schema_version, SOURCES_SCHEMA_VERSION);
  assert_eq!(
    response.status,
    VerbStatus::Ready,
    "every leg was computable, so the verb is Ready"
  );

  // `sources → working` is the leg the operator reads before a sync, so
  // it has to carry the actual change rather than an empty body.
  let working_leg = response
    .sections
    .iter()
    .find(|s| s.label == "sources → working")
    .expect("the sources → working leg");
  assert!(
    working_leg.body.contains("hosts/laptop.nix"),
    "the leg must show the file that differs: {}",
    working_leg.body
  );
}

/// SC-X-006, two-way half (task 5.2): no working dir, one meaningful leg.
#[test]
fn sc_x_006_with_no_working_dir_the_diff_reduces_to_the_remote_leg() {
  let world = World::new();
  let refs =
    ObservedRefs::observe(&world.sources_dir, None, &world.context().refs);

  let report = three_way_diff(&world.sources_dir, None, &refs);
  let labels: Vec<&str> =
    report.sections.iter().map(|s| s.label.as_str()).collect();
  assert_eq!(labels, ["remote → sources"]);
}

// ---------------------------------------------------------------------------
// SC-X-007 / SC-X-010 / SC-X-011: `sync_mode`
// ---------------------------------------------------------------------------

/// SC-X-007: `prompt` announces the sync through status and writes
/// nothing.
#[test]
fn sc_x_007_prompt_announces_the_sync_and_leaves_the_working_dir_alone() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");

  let stale_working = head(&world.working_dir);
  let before = snapshot(&world.working_dir);

  world.advance_sources_dir();
  assert_eq!(post_advance(SyncMode::Prompt), PostAdvance::Announce);

  let status = world.context().status(&GitopsHealth::Disabled);
  let snap = match &status {
    SourcesStatus::Ready { snapshot } | SourcesStatus::Pending { snapshot } => {
      snapshot
    },
    other => panic!("expected a described status, got {other:?}"),
  };
  assert!(snap.sync_available, "status must report the available sync");
  assert_eq!(
    snap.pending_sources_sha.as_deref(),
    Some(head(&world.sources_dir).as_str()),
    "status must name the new sources sha"
  );
  assert_eq!(
    snap.refs.working_head.as_deref(),
    Some(stale_working.as_str()),
    "status must name the stale working sha"
  );
  assert_eq!(
    snapshot(&world.working_dir),
    before,
    "`prompt` must not write to the working dir"
  );

  // The announcement is a statement about refs, not an action: nothing
  // ran, so the mode's hook is all that changed.
  assert_eq!(
    apply_auto_sync(
      SyncMode::Prompt,
      &world.sources_dir,
      &world.state_dir,
      Some(&world.working_dir),
      WHAT,
    )
    .expect("auto sync"),
    AutoSync::NotRequested
  );
}

/// SC-X-010: `auto-copy` syncs a clean tree on the next command, and
/// declines a dirty one without failing that command.
#[test]
fn sc_x_010_auto_copy_syncs_a_clean_tree_and_defers_a_dirty_one() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");
  world.advance_sources_dir();
  world.operator_fetches();
  assert_eq!(post_advance(SyncMode::AutoCopy), PostAdvance::QueueSync);

  // Dirty first: the deferral must not consume the pending sync.
  fs::write(world.working_dir.join("hosts/laptop.nix"), "{ y = 2; }\n")
    .expect("dirty the tree");
  assert_eq!(
    apply_auto_sync(
      SyncMode::AutoCopy,
      &world.sources_dir,
      &world.state_dir,
      Some(&world.working_dir),
      WHAT,
    )
    .expect("auto sync"),
    AutoSync::DeferredDirty
  );
  assert!(
    !world.working_dir.join("hosts/server.nix").exists(),
    "a deferred sync must not have copied anything"
  );

  // Clean the tree; the same call now applies.
  git(&world.working_dir, &["checkout", "-q", "--", "."]);
  let applied = apply_auto_sync(
    SyncMode::AutoCopy,
    &world.sources_dir,
    &world.state_dir,
    Some(&world.working_dir),
    WHAT,
  )
  .expect("auto sync");
  match applied {
    AutoSync::Applied(outcome) => {
      assert_eq!(outcome.sources_head, head(&world.sources_dir));
      assert!(!outcome.forced, "a clean tree is not a forced sync");
    },
    other => panic!("expected the sync to run, got {other:?}"),
  }
  assert!(
    world.working_dir.join("hosts/server.nix").exists(),
    "auto-copy must have written the new file"
  );

  // Idempotent: a second pass has nothing to do.
  assert_eq!(
    apply_auto_sync(
      SyncMode::AutoCopy,
      &world.sources_dir,
      &world.state_dir,
      Some(&world.working_dir),
      WHAT,
    )
    .expect("auto sync"),
    AutoSync::UpToDate
  );
}

/// SC-X-011: `never` still reports the sync, and never writes.
#[test]
fn sc_x_011_never_reports_the_sync_but_never_writes() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");
  world.advance_sources_dir();
  assert_eq!(post_advance(SyncMode::Never), PostAdvance::LogOnly);

  for dirty in [false, true] {
    if dirty {
      fs::write(world.working_dir.join("hosts/laptop.nix"), "{ y = 2; }\n")
        .expect("dirty the tree");
    }
    let before = snapshot(&world.working_dir);

    let status = world.context().status(&GitopsHealth::Disabled);
    let snap = match &status {
      SourcesStatus::Ready { snapshot }
      | SourcesStatus::Pending { snapshot } => snapshot,
      other => panic!("expected a described status, got {other:?}"),
    };
    assert!(
      snap.sync_available,
      "`never` governs who acts, not what status reports (dirty={dirty})"
    );

    assert_eq!(
      apply_auto_sync(
        SyncMode::Never,
        &world.sources_dir,
        &world.state_dir,
        Some(&world.working_dir),
        WHAT,
      )
      .expect("auto sync"),
      AutoSync::NotRequested
    );
    assert_eq!(
      snapshot(&world.working_dir),
      before,
      "`never` must not write to the working dir (dirty={dirty})"
    );
  }
}

// ---------------------------------------------------------------------------
// SC-X-008 / SC-X-009 / SC-X-012 / SC-X-013: `xi <scope> sources sync`
// ---------------------------------------------------------------------------

/// SC-X-008: copy-with-erase, `.git` untouched, HEAD advanced.
#[test]
fn sc_x_008_sync_copies_with_erase_and_advances_head() {
  let world = World::new();

  // The remote adds one file, rewrites another, and deletes a third.
  world.commit(
    &world.remote,
    "hosts/server.nix",
    "{ new = true; }\n",
    "add",
  );
  world.commit(
    &world.remote,
    "flake.nix",
    "{ outputs = _: { v = 2; }; }\n",
    "edit",
  );
  git(&world.remote, &["rm", "-q", "hosts/laptop.nix"]);
  git(&world.remote, &["commit", "-q", "-m", "delete"]);
  world.advance_sources_dir();
  world.operator_fetches();

  let git_config_before =
    fs::read(world.working_dir.join(".git/config")).expect("read git config");
  let outcome = world.sync(false).expect("sync a clean tree");

  assert!(
    world.working_dir.join("hosts/server.nix").exists(),
    "the added file must be written"
  );
  assert_eq!(
    fs::read_to_string(world.working_dir.join("flake.nix"))
      .expect("read flake"),
    "{ outputs = _: { v = 2; }; }\n",
    "the edited file must be replaced, not merged"
  );
  assert!(
    !world.working_dir.join("hosts/laptop.nix").exists(),
    "a file absent from the sources tree must be erased"
  );
  assert_eq!(
    head(&world.working_dir),
    outcome.sources_head,
    "HEAD must be advanced to the sources head"
  );
  assert_eq!(head(&world.working_dir), head(&world.sources_dir));
  assert!(!outcome.forced, "a clean tree is not a forced sync");
  assert!(
    outcome.written.iter().any(|p| p == "hosts/server.nix"),
    "the outcome must list what it wrote: {:?}",
    outcome.written
  );
  assert_eq!(
    outcome.deleted,
    ["hosts/laptop.nix"],
    "the outcome must list what it erased"
  );

  // `.git` is not part of the tracked set, so the repository's own
  // configuration -- its remote above all -- comes out untouched.
  assert_eq!(
    fs::read(world.working_dir.join(".git/config")).expect("read git config"),
    git_config_before,
    "the sync must not rewrite the working dir's .git"
  );
  assert_eq!(
    git(&world.working_dir, &["remote", "get-url", "origin"]),
    world.remote.display().to_string(),
    "the working dir must still point at its own remote"
  );

  // `--soft`, by task 5a.2: HEAD moved and the index did not, which is
  // what leaves the operator a reviewable diff instead of a silent
  // overwrite. The consequence is that the tree reads *dirty* right
  // after a sync -- see xi-5ytq for what that costs the next `switch`.
  let porcelain = git(&world.working_dir, &["status", "--porcelain"]);
  assert!(
    porcelain.contains("hosts/laptop.nix"),
    "the erase must be reviewable in the index: {porcelain}"
  );
  assert!(
    porcelain.contains("hosts/server.nix"),
    "the write must be reviewable in the index: {porcelain}"
  );
}

/// SC-X-009: a dirty tree refuses, naming the files.
#[test]
fn sc_x_009_sync_refuses_a_dirty_tree_without_the_escape() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");
  world.advance_sources_dir();
  world.operator_fetches();
  fs::write(world.working_dir.join("hosts/laptop.nix"), "{ y = 2; }\n")
    .expect("dirty the tree");

  let err = world.sync(false).expect_err("a dirty tree must refuse");
  let ConflictError::Dirty(reason) = &err else {
    panic!("expected a Dirty refusal, got {err:?}");
  };
  assert!(
    reason.contains("hosts/laptop.nix"),
    "the refusal must name the dirty file: {reason}"
  );
  assert!(
    reason.contains(FORCE_DIRTY_FLAG),
    "the refusal must name the escape hatch: {reason}"
  );
  assert!(
    !world.working_dir.join("hosts/server.nix").exists(),
    "a refused sync must not have written anything"
  );

  // The escape hatch runs, and reports that it destroyed work.
  let outcome = world.sync(true).expect("a forced sync runs");
  assert!(outcome.forced, "a forced sync must say so");
}

/// xi-5ytq: a sync's own `reset --soft` residue must not read as dirty
/// to the next `switch`.
///
/// `sc_x_008` above asserts the raw git index still shows the inverse
/// delta after a sync - that is `--soft`'s whole point, a reviewable
/// diff. This test is the other half of the bead: despite that index
/// state, the *resolver* must see a clean tree and evaluate the working
/// dir, not refuse it the way `SC-X-002` refuses a genuinely dirty one.
#[test]
fn xi_5ytq_a_synced_tree_is_not_dirty_to_the_next_switch() {
  let world = World::new();
  world.commit(
    &world.remote,
    "hosts/server.nix",
    "{ new = true; }\n",
    "add",
  );
  world.advance_sources_dir();
  world.operator_fetches();

  world.sync(false).expect("sync a clean tree");

  // The raw git state is exactly what sc_x_008 documents: not clean.
  let porcelain = git(&world.working_dir, &["status", "--porcelain"]);
  assert!(
    !porcelain.trim().is_empty(),
    "the sync's index-only delta must still be there for review"
  );

  // The resolver must disagree with raw `git status` here and evaluate
  // the working dir.
  match world.resolve(Some(&world.working_dir), false) {
    EvaluationSource::WorkingDir { path, dirty_source } => {
      assert_eq!(path, world.working_dir);
      assert!(
        !dirty_source,
        "a sync's own residue is not operator dirt, so this switch is \
         reproducible from sources_head and must not be marked dirty-source"
      );
    },
    other => {
      panic!(
        "a synced working dir must evaluate clean, got {other:?} (raw git \
         status: {porcelain:?})"
      )
    },
  }
}

/// xi-5ytq, the `auto-copy` path: the sync that runs unprompted at the
/// top of an interactive command must not leave the command it ran
/// ahead of refusing itself.
#[test]
fn xi_5ytq_auto_copy_sync_does_not_make_the_next_switch_refuse_itself() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");
  world.advance_sources_dir();
  world.operator_fetches();

  match apply_auto_sync(
    SyncMode::AutoCopy,
    &world.sources_dir,
    &world.state_dir,
    Some(&world.working_dir),
    WHAT,
  )
  .expect("auto sync")
  {
    AutoSync::Applied(_) => {},
    other => panic!("expected the sync to run, got {other:?}"),
  }

  match world.resolve(Some(&world.working_dir), false) {
    EvaluationSource::WorkingDir { dirty_source, .. } => {
      assert!(
        !dirty_source,
        "sync_mode = auto-copy must not refuse the very command that \
         triggered it"
      );
    },
    other => {
      panic!(
        "the switch right after an auto-copy sync must not refuse, got \
         {other:?}"
      )
    },
  }
}

/// SC-X-012: nothing outside the working dir is touched.
#[test]
fn sc_x_012_sync_touches_nothing_outside_the_working_dir() {
  let world = World::new();
  world.commit(&world.remote, "hosts/server.nix", "{ }\n", "remote moved");
  git(&world.remote, &["rm", "-q", "hosts/laptop.nix"]);
  git(&world.remote, &["commit", "-q", "-m", "delete"]);
  world.advance_sources_dir();
  world.operator_fetches();

  let sources_before = snapshot(&world.sources_dir);
  let sources_head_before = head(&world.sources_dir);
  let state_before = snapshot(&world.state_dir);
  let bystander_before = fs::read(&world.bystander).expect("read bystander");

  world.sync(false).expect("sync a clean tree");

  assert_eq!(
    snapshot(&world.sources_dir),
    sources_before,
    "the sources dir must be read-only for the whole operation"
  );
  assert_eq!(head(&world.sources_dir), sources_head_before);
  assert_eq!(
    snapshot(&world.state_dir),
    state_before,
    "the sync must not write the state dir"
  );
  assert_eq!(
    fs::read(&world.bystander).expect("read bystander"),
    bystander_before,
    "a file outside every managed tree must be byte-identical"
  );
}

/// SC-X-013: there is no reverse direction. The sync only ever writes
/// the working dir, and the daemon's own code path may not reach for a
/// merging git subcommand to fake one.
#[test]
fn sc_x_013_there_is_no_working_dir_to_sources_dir_direction() {
  let world = World::new();

  // A write guard rooted at the working dir rejects the sources dir, so
  // a reverse copy cannot be expressed with the same primitive.
  assert!(
    check_write_target(
      &world.working_dir,
      &world.sources_dir.join("flake.nix")
    )
    .is_err(),
    "the sync's guard must reject a sources-dir path"
  );

  // And the sanctioned reverse path is git push plus the daemon's next
  // fetch, so the merging subcommands stay refused in the working dir.
  for subcommand in ["pull", "merge", "rebase", "fetch"] {
    assert!(
      check_working_dir_git(&world.working_dir, &[subcommand]).is_err(),
      "`git {subcommand}` must stay refused in the working dir"
    );
  }
  assert!(
    check_working_dir_git(&world.working_dir, &["push"]).is_ok(),
    "`git push` is the sanctioned reverse path"
  );
}
