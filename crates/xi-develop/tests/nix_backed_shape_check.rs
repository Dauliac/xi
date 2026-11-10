//! BDD: openspec/changes/devshell-e2e-harness tasks 3.8
//!
//! The fixture-drift tripwire, deliberately OUTSIDE the fast suite.
//!
//! Every other test in this change runs against the committed `fake-nix`
//! fixture. That buys a suite which runs in milliseconds with no nix, no
//! daemon and no network, and it accepts one risk in exchange: if real nix
//! ever changes the shape of `print-dev-env --json`, the fast suite keeps
//! passing against a contract nix no longer honours.
//!
//! This test is the only thing standing between that risk and a silent lie,
//! so it compares the fixture's assumptions against ONE real nix invocation.
//!
//! It is `#[ignore]`d because it needs a working nix, a flake, and network
//! access for substituters. Run it deliberately:
//!
//! ```text
//! cargo test -p xi-develop --test nix_backed_shape_check -- --ignored
//! ```
//!
//! If it fails, do not "fix" it by loosening the assertion. The fixture and
//! the harness's `NixResponse` builder are what need updating, and every
//! staged response in the suite should be re-read against the new shape.

use std::process::Command;

use xi_develop::env_file::DevEnv;

/// The repository root, which is itself a flake with a devshell.
fn repo_root() -> std::path::PathBuf {
  std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .expect("repo root must resolve")
}

fn real_nix() -> Option<std::path::PathBuf> {
  let path = std::env::var_os("PATH")?;
  std::env::split_paths(&path)
    .map(|d| d.join("nix"))
    .find(|c| c.is_file())
}

/// BDD: xi-devshell-test-harness#Only the consumed subset is relied upon
///
/// Asserts the four structural facts the whole fixture rests on:
///   1. the payload is an object with a `variables` member,
///   2. entries carry a `type` discriminator, and `exported` is a real value,
///   3. `PATH` arrives as a single colon-joined exported string,
///   4. store paths still look like `/nix/store/<32 chars>-<name>-<version>`.
#[test]
#[ignore = "needs real nix, a flake and network access; run with --ignored"]
fn real_nix_output_still_matches_the_fixture_shape() {
  let Some(nix) = real_nix() else {
    panic!(
      "no real nix on PATH. This check exists to validate the fixture against \
       real nix, so it cannot be satisfied by the fake."
    );
  };

  let out = Command::new(&nix)
    .args([
      "print-dev-env",
      ".#",
      "--json",
      "--option",
      "connect-timeout",
      "5",
    ])
    .current_dir(repo_root())
    .output()
    .expect("failed to invoke real nix");

  assert!(
    out.status.success(),
    "real nix failed, so the shape cannot be checked:\n{}",
    String::from_utf8_lossy(&out.stderr)
  );

  // 1 + 2: the payload must still parse through the production parser.
  let parsed = DevEnv::from_nix_json(&out.stdout).unwrap_or_else(|e| {
    panic!(
      "FIXTURE DRIFT: real nix output no longer parses through \
       DevEnv::from_nix_json: {e}\nUpdate the fake-nix fixture and the \
       harness NixResponse builder, then re-read every staged response."
    )
  });

  // 3: PATH must have arrived as an exported string that split into paths.
  assert!(
    !parsed.nix_paths.is_empty(),
    "FIXTURE DRIFT: real nix produced no nix_paths. Either PATH is no longer \
     an exported string, or it is no longer colon-joined."
  );

  // 4: the store-path shape the fixture fakes must still be what nix emits.
  assert!(
    !parsed.packages.is_empty(),
    "FIXTURE DRIFT: no store path parsed into a package. The \
     /nix/store/<32-char-hash>-<name>-<version> shape the fixture relies on \
     has changed."
  );

  let raw = String::from_utf8_lossy(&out.stdout);
  assert!(
    raw.contains("\"variables\""),
    "FIXTURE DRIFT: the top-level 'variables' member is gone"
  );
  assert!(
    raw.contains("\"exported\""),
    "FIXTURE DRIFT: the 'exported' type discriminator is gone"
  );
}
