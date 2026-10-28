//! BDD: contract for the `xi-sources-model` capability
//! (openspec/changes/xi-sources-model/specs/xi-sources-model/spec.md).
//!
//! Each test below is one scenario from that spec delta, named after it.
//! The assertions are the scenario's THEN clauses and nothing else, so a
//! failure here points at exactly one broken requirement.

#![expect(clippy::expect_used, clippy::panic, reason = "Fine in tests")]

use std::{fs, path::PathBuf};

use tempfile::TempDir;
use xi_core::sources::{
  GitopsHealth,
  SOURCES_SCHEMA_VERSION,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatus,
  SourcesStatusResponse,
  write_state_file,
};

const REMOTE: &str = "https://git.example/config";

/// A machine with a sources dir on disk and a state dir ready to populate.
struct Machine {
  _tmp:        TempDir,
  sources_dir: PathBuf,
  state_dir:   PathBuf,
}

impl Machine {
  /// `with_sources_dir = false` models a host that never ran
  /// `xi sources init`.
  fn new(with_sources_dir: bool) -> Self {
    let tmp = TempDir::new().expect("tempdir");
    let sources_dir = tmp.path().join("sources");
    let state_dir = tmp.path().join("state");
    if with_sources_dir {
      fs::create_dir_all(&sources_dir).expect("create sources dir");
    }
    fs::create_dir_all(&state_dir).expect("create state dir");
    Self {
      _tmp: tmp,
      sources_dir,
      state_dir,
    }
  }

  fn record(&self, name: &str, value: &str) {
    write_state_file(&self.state_dir, name, value).expect("write state");
  }

  /// The "run `xi sources status`" step, at library level.
  fn status(&self, gitops: &GitopsHealth) -> SourcesStatus {
    let id = SourcesId::new(Scope::System, REMOTE);
    SourcesContext::load(id, &self.sources_dir, &self.state_dir)
      .expect("load context")
      .status(gitops)
  }
}

/// Scenario: Fresh machine returns Missing.
#[test]
fn fresh_machine_returns_missing_and_names_the_configuration_surfaces() {
  let machine = Machine::new(false);

  match machine.status(&GitopsHealth::Disabled) {
    SourcesStatus::Missing { configure_with } => {
      let names: Vec<&str> = configure_with
        .iter()
        .map(|hint| hint.name.as_str())
        .collect();
      assert!(
        names.iter().any(|name| name.contains("XI_SOURCES_REMOTE")),
        "env var not named: {names:?}"
      );
      assert!(
        names.iter().any(|name| name.contains("[sources] remote")),
        "config key not named: {names:?}"
      );
      assert!(
        names.iter().any(|name| name.contains("xi sources init")),
        "command not named: {names:?}"
      );
    },
    other => panic!("expected Missing, got {other:?}"),
  }
}

/// Scenario: Switched-at-least-once machine returns Ready.
#[test]
fn configured_and_switched_machine_returns_ready_with_expected_refs() {
  let machine = Machine::new(true);
  machine.record("sources-head", "aaaa1111");
  machine.record("active-generation", "128");

  match machine.status(&GitopsHealth::Disabled) {
    SourcesStatus::Ready { snapshot } => {
      assert_eq!(snapshot.refs.sources_head.as_deref(), Some("aaaa1111"));
      assert_eq!(snapshot.refs.active_generation.as_deref(), Some("128"));
      assert_eq!(snapshot.sources_dir, machine.sources_dir);
      assert_eq!(snapshot.state_dir, machine.state_dir);
    },
    other => panic!("expected Ready, got {other:?}"),
  }
}

/// Scenario: All four refs observable.
///
/// Gitops disabled and no working dir, so two of the four are legitimately
/// unknown. The point is that the model reports all four either way.
#[test]
fn all_four_refs_are_observable_even_when_unknown() {
  let machine = Machine::new(true);
  machine.record("sources-head", "bbbb2222");
  machine.record("active-generation", "7");

  match machine.status(&GitopsHealth::Disabled) {
    SourcesStatus::Ready { snapshot } => {
      assert_eq!(snapshot.refs.remote_head, None);
      assert_eq!(snapshot.refs.working_head, None);
      assert_eq!(snapshot.refs.sources_head.as_deref(), Some("bbbb2222"));
      assert_eq!(snapshot.refs.active_generation.as_deref(), Some("7"));
    },
    other => panic!("expected Ready, got {other:?}"),
  }
}

/// A sources dir with no generation yet has not been switched to.
#[test]
fn initialised_but_never_switched_machine_returns_pending() {
  let machine = Machine::new(true);
  machine.record("sources-head", "cccc3333");

  assert!(matches!(
    machine.status(&GitopsHealth::Disabled),
    SourcesStatus::Pending { .. }
  ));
}

/// Scenario: Stale working-dir pointer surfaces as Degraded.
#[test]
fn stale_working_dir_pointer_is_degraded_naming_the_path() {
  let machine = Machine::new(true);
  machine.record("active-generation", "9");
  let stale = machine.state_dir.join("..").join("deleted-checkout");
  machine.record("working-dir", &stale.display().to_string());

  match machine.status(&GitopsHealth::Disabled) {
    SourcesStatus::Degraded { reason, .. } => {
      assert!(
        reason.contains("deleted-checkout"),
        "reason must name the stale path, got: {reason}"
      );
    },
    other => panic!("expected Degraded, got {other:?}"),
  }
}

/// Scenario: Unknown file in state dir is reported.
#[test]
fn unknown_state_dir_file_is_reported_not_silently_ignored() {
  let machine = Machine::new(true);
  machine.record("active-generation", "9");
  fs::write(machine.state_dir.join("random-file.txt"), "junk")
    .expect("write junk");

  match machine.status(&GitopsHealth::Disabled) {
    SourcesStatus::Degraded { reason, .. } => {
      assert!(
        reason.contains("random-file.txt"),
        "reason must name the unknown file, got: {reason}"
      );
    },
    other => panic!("expected Degraded, got {other:?}"),
  }
}

/// A failed gitops tick degrades an otherwise-Ready machine.
#[test]
fn failed_gitops_tick_degrades_an_otherwise_ready_machine() {
  let machine = Machine::new(true);
  machine.record("active-generation", "9");

  assert!(matches!(
    machine.status(&GitopsHealth::Ok),
    SourcesStatus::Ready { .. }
  ));

  let failed = GitopsHealth::Failed {
    reason: "could not reach origin".to_owned(),
  };
  match machine.status(&failed) {
    SourcesStatus::Degraded { reason, .. } => {
      assert!(reason.contains("could not reach origin"), "got: {reason}");
    },
    other => panic!("expected Degraded, got {other:?}"),
  }
}

/// Scenario: Scope separates identical URLs.
#[test]
fn same_url_at_two_scopes_is_two_identities() {
  let user = SourcesId::new(Scope::User, REMOTE);
  let system = SourcesId::new(Scope::System, REMOTE);

  assert_ne!(user, system);
  assert_ne!(
    user.slug(),
    system.slug(),
    "slugs must differ so the two scopes get independent dirs"
  );
}

/// Scenario: Trailing .git is irrelevant.
#[test]
fn same_repo_spelled_two_ways_is_one_identity() {
  let bare = SourcesId::new(Scope::User, "https://git.example/repo");
  let dotted = SourcesId::new(Scope::User, "https://git.example/repo.git");
  let with_user =
    SourcesId::new(Scope::User, "https://bob@GIT.example/repo.git/");

  assert_eq!(bare, dotted);
  assert_eq!(bare, with_user);
}

/// Scenario: Each switch verb maps to exactly one scope.
#[test]
fn switch_verb_scope_table_is_exact() {
  assert_eq!(Scope::for_switch_verb("nixos"), Some(Scope::System));
  assert_eq!(Scope::for_switch_verb("darwin"), Some(Scope::System));
  assert_eq!(Scope::for_switch_verb("system"), Some(Scope::System));
  assert_eq!(Scope::for_switch_verb("home"), Some(Scope::User));
}

/// Scenario: Verb-less switch is a parse error.
///
/// The resolver refuses to guess, and the message it hands the CLI names
/// all four concrete verbs.
#[test]
fn verbless_switch_resolves_to_nothing_and_lists_the_verbs() {
  assert_eq!(Scope::for_switch_verb("switch"), None);

  let message = xi_core::sources::verbless_switch_error();
  for verb in Scope::SWITCH_VERBS {
    assert!(message.contains(verb), "`{verb}` missing from: {message}");
  }
}

/// Scenario: No source code in state dir.
///
/// The writer is the enforcement point: unsanctioned names and oversized
/// payloads are both refused, so source code cannot land here by accident.
#[test]
fn state_dir_writer_refuses_source_code() {
  let machine = Machine::new(true);

  write_state_file(&machine.state_dir, "main.rs", "fn main() {}")
    .expect_err("unsanctioned filename must be refused");

  let oversized = "x".repeat(xi_core::sources::MAX_STATE_FILE_BYTES + 1);
  write_state_file(&machine.state_dir, "remote-head", &oversized)
    .expect_err("oversized payload must be refused");
}

/// The public surface is a versioned envelope, flat enough for `jq`.
#[test]
fn status_envelope_is_versioned_and_flat() {
  let machine = Machine::new(true);
  machine.record("sources-head", "dddd4444");
  machine.record("active-generation", "11");

  let response =
    SourcesStatusResponse::new(machine.status(&GitopsHealth::Disabled));
  let json = serde_json::to_value(&response).expect("serialise");

  assert_eq!(json["schema_version"], SOURCES_SCHEMA_VERSION);
  assert_eq!(json["status"], "ready");
  assert_eq!(json["sources_head"], "dddd4444");
  assert_eq!(json["active_generation"], "11");
}
