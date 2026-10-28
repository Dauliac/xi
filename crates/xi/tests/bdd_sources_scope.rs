//! BDD: the xi-sources-model scope table, asserted against the real
//! command types rather than a copy of the table.
//!
//! `xi-core` owns the `verb -> Scope` mapping but cannot depend on the
//! per-platform arg crates, so the two halves are pinned together here:
//! each command's `scope()` must agree with what the resolver returns for
//! the verb that reaches it.

use xi_core::sources::Scope;

/// Scenario: Each switch verb maps to exactly one scope.
#[test]
fn each_switch_command_declares_the_scope_the_resolver_expects() {
  let cases = [
    ("nixos", xi_nixos::args::OsArgs::scope()),
    ("darwin", xi_darwin::args::DarwinArgs::scope()),
    ("system", xi::system::args::SystemArgs::scope()),
    ("home", xi_home::args::HomeArgs::scope()),
  ];

  for (verb, declared) in cases {
    assert_eq!(
      Scope::for_switch_verb(verb),
      Some(declared),
      "`xi {verb} switch` declares a scope the resolver disagrees with"
    );
  }
}

/// The three system-scope verbs really do collapse to one scope, and
/// `home` is the only user-scope verb.
#[test]
fn system_verbs_share_a_scope_and_home_stands_alone() {
  assert_eq!(xi_nixos::args::OsArgs::scope(), Scope::System);
  assert_eq!(xi_darwin::args::DarwinArgs::scope(), Scope::System);
  assert_eq!(xi::system::args::SystemArgs::scope(), Scope::System);
  assert_eq!(xi_home::args::HomeArgs::scope(), Scope::User);
}

/// Scenario: Verb-less switch is a parse error.
///
/// Nothing in the table answers for a bare `switch`.
#[test]
fn bare_switch_matches_no_command() {
  assert_eq!(Scope::for_switch_verb("switch"), None);
}
