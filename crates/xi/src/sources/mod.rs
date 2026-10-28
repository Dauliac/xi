//! The verb-less `xi switch` handler.
//!
//! The sources command group itself lives in
//! [`xi_core::sources::args`] and is mounted under each scope verb, so
//! the only thing left in the binary crate is the rejection path for a
//! bare `xi switch`.

use color_eyre::Result;
use xi_core::sources::verbless_switch_error;

/// Handler for the verb-less `xi switch`.
///
/// `switch` is accepted by the parser purely so this message can name
/// the four concrete verbs; clap's own "unrecognized subcommand" error
/// would not. No sources state is touched on this path.
///
/// # Errors
///
/// Always returns an error  -  that is the entire contract.
pub fn verbless_switch() -> Result<()> {
  Err(color_eyre::eyre::eyre!(verbless_switch_error()))
}
