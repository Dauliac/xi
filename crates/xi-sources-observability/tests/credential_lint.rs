//! The secret-at-rest lint, run against the real workspace.
//!
//! `xi-sources-credentials` task 7.1 asks for "a static lint rule
//! [that] rejects any `tracing::` call that takes a credential value as
//! a parameter". [`redact::lint`] is that rule, and its own doc comment
//! makes the case for running it here: "a lint that only CI has is a
//! lint nobody runs locally". Its unit tests drive it over tempdir
//! fixtures, which proves the matcher works; this is the test that
//! proves the matcher is pointed at the code.
//!
//! It lives in this crate rather than in `xi-core`, where the
//! credential newtype is, because `xi-sources-observability` depends on
//! `xi-core` and not the other way round: a test here can scan the
//! whole tree, including the crate that holds the secrets.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use std::path::Path;

use xi_sources_observability::redact::lint;

/// Every `.rs` file in the workspace, scanned for a banned field name
/// on a log or metric call.
#[test]
fn no_observable_call_in_the_workspace_names_a_credential() {
  // `crates/` is this crate's manifest dir's parent.
  let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .expect("crates/ is the manifest dir's parent");

  let found = lint::scan_workspace(crates_dir).expect("scan the workspace");

  assert!(
    found.is_empty(),
    "a credential-shaped key reached an observable call:\n{}",
    found
      .iter()
      .map(ToString::to_string)
      .collect::<Vec<_>>()
      .join("\n")
  );
}

/// The scan really walked the tree it was pointed at.
///
/// Without this, a `scan_workspace` that silently returned nothing -
/// a wrong path, a changed skip rule, an extension typo - would make
/// the test above pass by doing nothing at all.
#[test]
fn the_scan_actually_reaches_the_workspace_sources() {
  let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .expect("crates/ is the manifest dir's parent");

  assert!(
    crates_dir
      .join("xi-core/src/sources/credentials.rs")
      .is_file(),
    "the tree this lint is supposed to cover is not where it expects"
  );

  // A planted violation in a copy of the tree layout must be caught, so
  // an empty result above means "clean" rather than "did not look".
  //
  // The banned key is assembled rather than written out: the scan walks
  // `tests/` too, so spelling it inline here would make this file its
  // own first finding and fail the scan above.
  let banned = concat!("to", "ken");
  let tmp = tempfile::TempDir::new().expect("tempdir");
  let src = tmp.path().join("some-crate/src");
  std::fs::create_dir_all(&src).expect("mkdir");
  std::fs::write(
    src.join("lib.rs"),
    format!(
      "fn leak(c: &Cfg) {{\n  info!({banned} = %c.expose(), \"x\");\n}}\n"
    ),
  )
  .expect("write");

  let found = lint::scan_workspace(tmp.path()).expect("scan the fixture");
  assert_eq!(found.len(), 1, "the matcher is not matching: {found:?}");
  assert_eq!(found[0].key, banned);
}
