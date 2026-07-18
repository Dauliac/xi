//! Unified user-facing CLI output.
//!
//! Direct CLI output uses bare icons (no `[xi]` label) since the user
//! already knows they're running xi. The `[xi]` label is reserved for
//! daemon notifications that appear asynchronously in the shell prompt,
//! where context is needed to distinguish from other output.
//!
//!   ⟳ loading...     (blue)
//!   ✓ success         (green)
//!   ● info            (white)
//!   ▲ warning         (yellow)
//!   ✗ error           (red)

use xi_core::style::{self, Icon};

/// Print an info message to stderr.
pub fn info(msg: impl Into<String>) {
  eprintln!("{}", style::status_line(Icon::Info, &msg.into()));
}

/// Print a success message to stderr.
pub fn success(msg: impl Into<String>) {
  eprintln!("{}", style::status_line(Icon::Success, &msg.into()));
}

/// Print a loading/progress message to stderr.
pub fn loading(msg: impl Into<String>) {
  eprintln!("{}", style::status_line(Icon::Loading, &msg.into()));
}

/// Print a warning to stderr.
pub fn warn(msg: impl Into<String>) {
  eprintln!("{}", style::status_line(Icon::Warn, &msg.into()));
}

/// Print an error to stderr.
pub fn error(msg: impl Into<String>) {
  eprintln!("{}", style::status_line(Icon::Error, &msg.into()));
}
