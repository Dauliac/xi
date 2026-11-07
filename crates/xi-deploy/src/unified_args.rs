//! The flag surface for the unified `xi deploy` verb.
//!
//! Per `xi-deploy-unified-verb/spec.md`'s "Every auto-detected decision
//! has a forcing flag" requirement, [`DeployArgs`] is the union of every
//! flag `switch` (via [`SwitchSourcesArgs`] and the loose, non-installable
//! pieces of [`CommonRebuildArgs`]) and `push` (via [`PushArgs`]) already
//! have, plus the flags only `xi deploy` needs: `--scope`, `--mechanism`,
//! `--local`/`--here`, `--target`, `--fleet`, and `--dry`/`-n`.
//!
//! [`CommonRebuildArgs`] is not flattened wholesale: it carries
//! [`InstallableArgs`](xi_core::installable::InstallableArgs)'s own
//! single-value positional, which would collide with the trailing, multi-value
//! positional this verb needs for its target-or-installable
//! disambiguation (`xi-deploy-unified-verb/spec.md`'s positional
//! requirement - a bare word is checked against the target set before
//! falling back to `classify_positional`, and two or more positionals
//! are always a target list). `--dry`/`-n` and `--force-dirty` are
//! likewise defined once each, even though both appear in more than one
//! of the unioned structs, so clap never sees the same flag declared
//! twice.
//!
//! This module only defines the flag surface and proves it parses; the
//! positional-disambiguation rule, the decision tree, and dispatch into
//! `switch`/`push` are owned by this change's other task groups.

use std::path::PathBuf;

use clap::Args;
use xi_core::{
  args::{CacheArgs, DiffType, NixBuildPassthroughArgs},
  sources::switch::SwitchSourcesArgs,
};

use crate::push::args::{CopyVia, PushBackend, PushMode};

/// Which half of the machine - or both - `xi deploy` governs.
///
/// A superset of `push`'s own `PushScope`: `All` has no equivalent there,
/// since `push` is always scoped to exactly one half.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DeployScope {
  /// Host-wide configuration.
  #[default]
  System,
  /// Per-user configuration.
  User,
  /// Both, in sequence.
  All,
}

/// Which activation mechanism `xi deploy` should target, overriding
/// `SystemDeploymentKind::detect()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DeployMechanism {
  /// `nixos-rebuild`, via `os switch`'s code path.
  Nixos,
  /// `darwin-rebuild`, via `darwin switch`'s code path.
  Darwin,
  /// `system-manager`, via `system switch`'s code path.
  SystemManager,
}

/// The full flag surface for `xi deploy`.
///
/// See the module docs for why this is a hand-assembled union rather
/// than a flatten of `SwitchSourcesArgs`, `CommonRebuildArgs`, and
/// `PushArgs` in one shot.
///
/// `--help` groups the surface into three headings: "Where"
/// (target/scope/mechanism resolution, including `sources`'s
/// `--branch`/`--adopt`/`--no-adopt` and their `--force`/`--force-dirty`
/// overrides - all five answer "which source, at which location"), "How
/// it builds" (backend, build strategy, and the Nix-level build knobs),
/// and "Safety" (confirmation, preview, rollback, and failure handling).
/// Every plain flag carries an explicit `help_heading` rather than
/// leaning on clap's `next_help_heading` stickiness: `clap_derive`
/// rejects `#[command(next_help_heading = ...)]` on a plain `#[arg]`
/// field, accepting it only on `flatten`/`subcommand` fields, so a
/// flattened sub-struct's own un-annotated fields (`sources`,
/// `passthrough`, `cache`) pick up their heading from
/// `next_help_heading` set on the flatten field itself instead.
#[derive(Debug, Args)]
pub struct DeployArgs {
  /// A fleet target name, a path/URL/scp-spec installable to adopt or
  /// build, or several target names (two or more positionals are
  /// always read as a target list).
  #[arg(trailing_var_arg = true, help_heading = "Where")]
  pub targets: Vec<String>,

  /// Force every positional to be read as a fleet target name.
  #[arg(long, help_heading = "Where")]
  pub target: Vec<String>,

  /// Which half of the machine to deploy.
  #[arg(long, value_enum, help_heading = "Where")]
  pub scope: Option<DeployScope>,

  /// Force a specific activation mechanism, bypassing host detection.
  #[arg(long, value_enum, help_heading = "Where")]
  pub mechanism: Option<DeployMechanism>,

  /// Activate locally, bypassing the fleet-rollout branch of the
  /// decision tree.
  #[arg(long, alias = "here", help_heading = "Where")]
  pub local: bool,

  /// Force a whole-fleet rollout, bypassing the local-activation
  /// branches of the decision tree.
  #[arg(long, help_heading = "Where")]
  pub fleet: bool,

  /// Filter discovered targets by tag (`@tag`) or by regex.
  #[arg(long, help_heading = "Where")]
  pub on: Option<String>,

  /// Flags shared with every scope's `switch`: `--branch`, `--adopt`,
  /// `--no-adopt`, `--force`, `--force-dirty`.
  #[command(flatten, next_help_heading = "Where")]
  pub sources: SwitchSourcesArgs,

  /// Whether to display a package diff.
  #[arg(long, short, value_enum, default_value_t = DiffType::Auto, env = "XI_DIFF", help_heading = "How it builds")]
  pub diff: DiffType,

  /// Don't use nix-output-monitor for the build process.
  #[arg(long, env = "XI_NO_NOM", value_parser = clap::builder::BoolishValueParser::new(), help_heading = "How it builds")]
  pub no_nom: bool,

  /// Path to save the result link, defaults to using a temporary directory.
  #[arg(long, short, help_heading = "How it builds")]
  pub out_link: Option<PathBuf>,

  #[command(flatten, next_help_heading = "How it builds")]
  pub passthrough: NixBuildPassthroughArgs,

  #[command(flatten, next_help_heading = "How it builds")]
  pub cache: CacheArgs,

  /// Force a specific deployment backend.
  #[arg(long, value_enum, default_value_t = PushBackend::Auto, help_heading = "How it builds")]
  pub backend: PushBackend,

  /// Bring-up phases to run, comma separated.
  #[arg(
    long,
    default_value = "activate",
    value_name = "CSV",
    help_heading = "How it builds"
  )]
  pub phase: String,

  /// Evaluate locally and copy a closure, or sync the sources dir.
  #[arg(long, value_enum, help_heading = "How it builds")]
  pub mode: Option<PushMode>,

  /// Build on the target instead of copying a closure in.
  #[arg(long, help_heading = "How it builds")]
  pub remote_build: Option<bool>,

  /// Number of targets to activate concurrently.
  #[arg(
    long,
    default_value_t = 1,
    value_name = "N",
    help_heading = "How it builds"
  )]
  pub parallel: usize,

  /// Upload the closure from here, or let the target substitute it.
  #[arg(long, value_enum, default_value_t = CopyVia::Local, help_heading = "How it builds")]
  pub copy_via: CopyVia,

  /// Allocate a PTY so a remote sudo password prompt reaches the terminal.
  #[arg(long, help_heading = "How it builds")]
  pub interactive_sudo: bool,

  /// Only print the resolved plan, without building, copying, or
  /// activating anything.
  #[arg(long, short = 'n', alias = "dry-run", help_heading = "Safety")]
  pub dry: bool,

  /// Ask for confirmation.
  #[arg(long, short, help_heading = "Safety")]
  pub ask: bool,

  /// Seconds to confirm the new generation in before rolling back (0 = off).
  #[arg(
    long,
    default_value_t = 0,
    value_name = "SECONDS",
    help_heading = "Safety"
  )]
  pub magic_rollback: u64,

  /// Stop at the first failing target instead of finishing the rest.
  #[arg(long, help_heading = "Safety")]
  pub fail_fast: bool,
}

#[cfg(test)]
mod tests {
  use clap::Parser;

  use super::*;
  use crate::push::args::{CopyVia, PushBackend, PushMode};

  #[derive(Debug, Parser)]
  struct TestCli {
    #[command(flatten)]
    args: DeployArgs,
  }

  fn parse(args: &[&str]) -> clap::error::Result<DeployArgs> {
    TestCli::try_parse_from(std::iter::once("xi").chain(args.iter().copied()))
      .map(|cli| cli.args)
  }

  #[test]
  fn no_flags_at_all_parses_with_every_default() {
    let args = parse(&[]).expect("parses");
    assert!(args.targets.is_empty());
    assert!(args.target.is_empty());
    assert!(args.scope.is_none());
    assert!(args.mechanism.is_none());
    assert!(!args.local);
    assert!(!args.fleet);
    assert!(!args.dry);
    assert!(!args.sources.adopt);
    assert!(!args.sources.no_adopt);
    assert!(!args.sources.force);
    assert!(!args.sources.force_dirty);
    assert_eq!(args.sources.branch, None);
    assert!(!args.ask);
    assert!(matches!(args.diff, DiffType::Auto));
    assert!(!args.no_nom);
    assert_eq!(args.out_link, None);
    assert_eq!(args.on, None);
    assert!(matches!(args.backend, PushBackend::Auto));
    assert_eq!(args.phase, "activate");
    assert!(args.mode.is_none());
    assert_eq!(args.remote_build, None);
    assert_eq!(args.magic_rollback, 0);
    assert_eq!(args.parallel, 1);
    assert!(!args.interactive_sudo);
    assert!(matches!(args.copy_via, CopyVia::Local));
    assert!(!args.fail_fast);
  }

  // SwitchSourcesArgs flags, unchanged.

  #[test]
  fn switch_sources_flags_parse_identically() {
    let args =
      parse(&["--branch", "main", "--adopt", "--force", "--force-dirty"])
        .expect("parses");
    assert_eq!(args.sources.branch, Some("main".to_owned()));
    assert!(args.sources.adopt);
    assert!(args.sources.force);
    assert!(args.sources.force_dirty);
  }

  #[test]
  fn adopt_and_no_adopt_still_conflict() {
    let err = parse(&["--adopt", "--no-adopt"]).expect_err("conflict");
    assert_eq!(
      err.kind(),
      clap::error::ErrorKind::ArgumentConflict,
      "got: {err}"
    );
  }

  // CommonRebuildArgs's loose flags (minus the installable positional).

  #[test]
  fn dry_short_flag_and_long_alias_both_parse() {
    assert!(parse(&["-n"]).expect("parses").dry);
    assert!(parse(&["--dry-run"]).expect("parses").dry);
    assert!(parse(&["--dry"]).expect("parses").dry);
  }

  #[test]
  fn ask_diff_no_nom_and_out_link_parse_identically() {
    let args = parse(&[
      "-a",
      "--diff",
      "always",
      "--no-nom",
      "--out-link",
      "/tmp/result",
    ])
    .expect("parses");
    assert!(args.ask);
    assert!(matches!(args.diff, DiffType::Always));
    assert!(args.no_nom);
    assert_eq!(args.out_link, Some(PathBuf::from("/tmp/result")));
  }

  #[test]
  fn nix_build_passthrough_flags_still_parse() {
    let args = parse(&["-j", "4", "--keep-going", "-t"]).expect("parses");
    assert_eq!(args.passthrough.max_jobs, Some(4));
    assert!(args.passthrough.keep_going);
    assert!(args.passthrough.show_trace);
  }

  #[test]
  fn cache_flags_still_parse() {
    let args =
      parse(&["--push-to", "s3://cache", "--no-push"]).expect("parses");
    assert_eq!(args.cache.push_to, Some("s3://cache".to_owned()));
    assert!(args.cache.no_push);
  }

  // PushArgs flags, unchanged.

  #[test]
  fn push_flags_parse_identically() {
    let args = parse(&[
      "--on",
      "@web",
      "--backend",
      "colmena",
      "--phase",
      "activate",
      "--mode",
      "sources",
      "--remote-build",
      "true",
      "--magic-rollback",
      "30",
      "--parallel",
      "4",
      "--interactive-sudo",
      "--copy-via",
      "target",
      "--fail-fast",
    ])
    .expect("parses");
    assert_eq!(args.on, Some("@web".to_owned()));
    assert!(matches!(args.backend, PushBackend::Colmena));
    assert_eq!(args.phase, "activate");
    assert!(matches!(args.mode, Some(PushMode::Sources)));
    assert_eq!(args.remote_build, Some(true));
    assert_eq!(args.magic_rollback, 30);
    assert_eq!(args.parallel, 4);
    assert!(args.interactive_sudo);
    assert!(matches!(args.copy_via, CopyVia::Target));
    assert!(args.fail_fast);
  }

  // New flags this verb adds on top of the union.

  #[test]
  fn scope_accepts_all_three_values_including_the_new_all_variant() {
    assert!(matches!(
      parse(&["--scope", "system"]).expect("parses").scope,
      Some(DeployScope::System)
    ));
    assert!(matches!(
      parse(&["--scope", "user"]).expect("parses").scope,
      Some(DeployScope::User)
    ));
    assert!(matches!(
      parse(&["--scope", "all"]).expect("parses").scope,
      Some(DeployScope::All)
    ));
  }

  #[test]
  fn mechanism_accepts_the_three_backends() {
    assert!(matches!(
      parse(&["--mechanism", "nixos"]).expect("parses").mechanism,
      Some(DeployMechanism::Nixos)
    ));
    assert!(matches!(
      parse(&["--mechanism", "darwin"]).expect("parses").mechanism,
      Some(DeployMechanism::Darwin)
    ));
    assert!(matches!(
      parse(&["--mechanism", "system-manager"])
        .expect("parses")
        .mechanism,
      Some(DeployMechanism::SystemManager)
    ));
  }

  #[test]
  fn local_and_its_here_alias_both_parse() {
    assert!(parse(&["--local"]).expect("parses").local);
    assert!(parse(&["--here"]).expect("parses").local);
  }

  #[test]
  fn fleet_forces_a_whole_fleet_rollout_flag() {
    assert!(parse(&["--fleet"]).expect("parses").fleet);
  }

  #[test]
  fn explicit_target_collects_every_occurrence() {
    let args =
      parse(&["--target", "web1", "--target", "web2"]).expect("parses");
    assert_eq!(args.target, vec!["web1".to_owned(), "web2".to_owned()]);
  }

  // Positional handling: a single trailing bucket, not split between an
  // installable slot and a target-list slot.

  #[test]
  fn a_single_bare_positional_lands_in_targets() {
    let args = parse(&["web1"]).expect("parses");
    assert_eq!(args.targets, vec!["web1".to_owned()]);
  }

  #[test]
  fn two_or_more_positionals_all_land_in_targets() {
    let args = parse(&["web1", "web2", "web3"]).expect("parses");
    assert_eq!(
      args.targets,
      vec!["web1".to_owned(), "web2".to_owned(), "web3".to_owned()]
    );
  }

  #[test]
  fn flags_and_positionals_parse_together() {
    let args =
      parse(&["--scope", "user", "--dry", "./dotfiles"]).expect("parses");
    assert!(matches!(args.scope, Some(DeployScope::User)));
    assert!(args.dry);
    assert_eq!(args.targets, vec!["./dotfiles".to_owned()]);
  }

  // Task 3.2: the flag surface renders under three distinct `--help`
  // headings instead of one undifferentiated `Options:` block.

  #[test]
  fn help_renders_where_how_it_builds_and_safety_as_distinct_sections() {
    use clap::CommandFactory;

    let help = TestCli::command().render_long_help().to_string();

    let where_pos = help.find("Where:").expect("Where heading present");
    let how_pos = help
      .find("How it builds:")
      .expect("How it builds heading present");
    let safety_pos = help.find("Safety:").expect("Safety heading present");
    assert!(
      where_pos < how_pos && how_pos < safety_pos,
      "expected headings in order Where, How it builds, Safety; got:\n{help}"
    );

    // Spot-check representative flags land under the heading they were
    // grouped into, rather than bleeding into a neighboring section.
    let where_section = &help[where_pos..how_pos];
    let how_section = &help[how_pos..safety_pos];
    let safety_section = &help[safety_pos..];

    for flag in ["--scope", "--mechanism", "--fleet", "--on", "--adopt"] {
      assert!(
        where_section.contains(flag),
        "expected {flag} under Where; got:\n{where_section}"
      );
    }
    for flag in ["--backend", "--mode", "--copy-via", "--parallel"] {
      assert!(
        how_section.contains(flag),
        "expected {flag} under How it builds; got:\n{how_section}"
      );
    }
    for flag in ["--dry", "--ask", "--magic-rollback", "--fail-fast"] {
      assert!(
        safety_section.contains(flag),
        "expected {flag} under Safety; got:\n{safety_section}"
      );
    }
  }
}
