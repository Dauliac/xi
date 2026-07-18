use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub type Spinner = ProgressBar;

const DEFAULT_SPINNER_TICK: Duration = Duration::from_millis(80);
const SPINNER_FRAMES: &[&str] = &["⢹", "⢺", "⢼", "⣸", "⣇", "⡧", "⡗", "⡏"];

fn spinner_style() -> ProgressStyle {
  #[expect(clippy::expect_used)]
  ProgressStyle::with_template("{spinner:.blue} {msg}")
    .expect("Static spinner template is valid")
    .tick_strings(SPINNER_FRAMES)
}

/// Returns a spinner with a blue animation and message.
///
/// # Panics
///
/// Panics if the hardcoded spinner template is invalid.
#[must_use]
pub fn spinner(message: impl Into<String>) -> Spinner {
  let spinner = ProgressBar::new_spinner().with_style(spinner_style());
  spinner.set_message(message.into());
  spinner.enable_steady_tick(DEFAULT_SPINNER_TICK);

  spinner
}

/// Returns a spinner managed by a [`MultiProgress`] group so that
/// multiple concurrent spinners share the same terminal region and
/// don't duplicate lines.
#[must_use]
pub fn multi_spinner(
  mp: &MultiProgress,
  message: impl Into<String>,
) -> Spinner {
  let spinner = mp.add(ProgressBar::new_spinner().with_style(spinner_style()));
  spinner.set_message(message.into());
  spinner.enable_steady_tick(DEFAULT_SPINNER_TICK);

  spinner
}
