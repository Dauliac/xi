//! BDD: `xi deploy`'s `--build-strategy`/`--build-only`/`--from-manifest`
//! flags refuse exactly like their `xi push` equivalents, from
//! `openspec/changes/xi-deploy-expose-push-flags/specs/
//! xi-deploy-expose-push-flags/spec.md`'s requirement that `xi deploy`
//! re-exposes these flags without changing their resolution semantics
//! (xi-6mf6.3.5).
//!
//! `push_args()` (`crates/xi/src/deploy.rs`, xi-6mf6.2.1) already passes
//! these flags through into `PushArgs` unchanged, and `PushArgs::validate()`
//! (`crates/xi-deploy/src/push/args.rs`) is the only place these
//! combinations are rejected. `push_args()` itself is private to
//! `crates/xi/src/deploy.rs`, so this mirrors its field-by-field mapping
//! by hand to build the exact `PushArgs` it would build from a given
//! `DeployArgs`, then calls `validate()` on it directly - proving `xi
//! deploy`'s flags reach the exact same validation `xi push` already
//! has, rather than a reimplementation that could drift in wording.

#![expect(clippy::expect_used, reason = "Fine in tests")]

use clap::Parser;
use xi_deploy::{
  push::args::{BuildStrategy, PushArgs, PushScope},
  unified_args::DeployArgs,
};

/// Flattens `DeployArgs` behind the real `clap` parser, the same shape
/// `xi deploy` itself parses through.
#[derive(Debug, Parser)]
struct DeployArgsHarness {
  #[command(flatten)]
  args: DeployArgs,
}

fn parse(args: &[&str]) -> DeployArgs {
  DeployArgsHarness::try_parse_from(
    std::iter::once("xi").chain(args.iter().copied()),
  )
  .expect("`xi deploy` flags parse")
  .args
}

/// Mirrors `push_args()` (`crates/xi/src/deploy.rs`) field for field:
/// the exact `PushArgs` a fleet rollout triggered by `args` would be
/// validated against.
fn push_args_from(args: &DeployArgs) -> PushArgs {
  PushArgs {
    targets:           Vec::new(),
    on:                args.on.clone(),
    backend:           args.backend,
    phase:             args.phase.clone(),
    mode:              args.mode,
    remote_build:      args.remote_build,
    sources_transport: args.sources_transport,
    retry:             args.retry,
    magic_rollback:    args.magic_rollback,
    parallel:          args.parallel,
    interactive_sudo:  args.interactive_sudo,
    copy_via:          args.copy_via,
    fail_fast:         args.fail_fast,
    force_dirty:       args.sources.force_dirty,
    build_strategy:    args.build_strategy,
    build_only:        args.build_only,
    from_manifest:     args.from_manifest.clone(),
    scope:             PushScope::System,
    reattach:          args.reattach.clone(),
    notify:            args.notify.clone(),
  }
}

#[test]
fn build_only_and_from_manifest_together_refuse_with_pushs_exact_text() {
  let args =
    parse(&["--build-only", "--from-manifest", "/tmp/push-manifest.json"]);

  let err = push_args_from(&args)
    .validate()
    .expect_err("--build-only and --from-manifest conflict");

  assert_eq!(
    err.to_string(),
    "--build-only and --from-manifest cannot both be given: one builds \
     without shipping, the other ships without building"
  );
}

#[test]
fn build_only_alone_with_build_strategy_omitted_refuses_with_pushs_exact_text()
{
  let args = parse(&["--build-only"]);
  assert!(matches!(args.build_strategy, BuildStrategy::Interleaved));

  let err = push_args_from(&args)
    .validate()
    .expect_err("--build-only needs --build-strategy batched");

  assert_eq!(
    err.to_string(),
    "--build-only only makes sense with --build-strategy batched"
  );
}

#[test]
fn build_only_alone_with_build_strategy_interleaved_refuses_with_pushs_exact_text()
 {
  let args = parse(&["--build-only", "--build-strategy", "interleaved"]);

  let err = push_args_from(&args)
    .validate()
    .expect_err("--build-only needs --build-strategy batched");

  assert_eq!(
    err.to_string(),
    "--build-only only makes sense with --build-strategy batched"
  );
}

#[test]
fn from_manifest_alone_with_build_strategy_omitted_refuses_with_pushs_exact_text()
 {
  let args = parse(&["--from-manifest", "/tmp/push-manifest.json"]);
  assert!(matches!(args.build_strategy, BuildStrategy::Interleaved));

  let err = push_args_from(&args)
    .validate()
    .expect_err("--from-manifest needs --build-strategy batched");

  assert_eq!(
    err.to_string(),
    "--from-manifest only makes sense with --build-strategy batched"
  );
}

#[test]
fn from_manifest_alone_with_build_strategy_interleaved_refuses_with_pushs_exact_text()
 {
  let args = parse(&[
    "--from-manifest",
    "/tmp/push-manifest.json",
    "--build-strategy",
    "interleaved",
  ]);

  let err = push_args_from(&args)
    .validate()
    .expect_err("--from-manifest needs --build-strategy batched");

  assert_eq!(
    err.to_string(),
    "--from-manifest only makes sense with --build-strategy batched"
  );
}

/// Negative control: the one combination of these three flags that is
/// valid - `--build-only` with `--build-strategy batched` - must not be
/// rejected, proving the test above refuses on the flag combination,
/// not merely because `--build-only`/`--from-manifest` were given at
/// all.
#[test]
fn build_only_with_build_strategy_batched_is_accepted() {
  let args = parse(&["--build-only", "--build-strategy", "batched"]);

  push_args_from(&args)
    .validate()
    .expect("--build-only with --build-strategy batched is valid");
}
