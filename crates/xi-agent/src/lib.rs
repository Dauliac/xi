//! xi-agent — structured project context for AI coding agents.
//!
//! Emits JSON envelopes on stdout, human progress on stderr. See the
//! design set at `specs/001-xi-agent/` for the full contract.

use color_eyre::eyre::Result;

pub mod args;
pub mod context;
pub mod devshell;
pub mod install;
pub mod manifest;
pub mod outputs;
pub mod schema;
pub mod stage;
pub mod validate;

/// Skills embedded from `crates/xi-agent/skills/` at compile time.
pub static SKILLS: include_dir::Dir<'_> =
  include_dir::include_dir!("$CARGO_MANIFEST_DIR/skills");

/// Entry point wired into `xi` main via the `Agent` subcommand variant.
///
/// # Errors
/// Returns an error only for argument-level failures. Every runtime
/// failure is reported inside the JSON envelope on stdout.
pub fn run(args: args::AgentArgs) -> Result<()> {
  match args.command {
    args::AgentCommand::Context(a) => context::run(a),
    args::AgentCommand::Outputs(a) => outputs::run(a),
    args::AgentCommand::Devshell(a) => devshell::run(a),
    args::AgentCommand::Stage(a) => stage::run(a),
    args::AgentCommand::Manifest(a) => manifest::run(a),
    args::AgentCommand::Validate(a) => validate::run(a),
    args::AgentCommand::Install(a) => install::run(a),
  }
}
