//! The one seam every Nix-touching command goes through.
//!
//! Registering a profile generation and activating a system are the two
//! operations a test must never actually perform: one writes under
//! `/nix/var/nix/profiles`, the other replaces the running system. Both
//! are reached through [`Exec`] so a test can substitute a recorder and
//! still assert the exact argv the daemon would have run.
//!
//! Git is deliberately *not* behind this seam. A git fixture in a
//! tempdir is cheap, honest, and the thing the fast-forward and
//! force-push scenarios actually need to exercise; mocking it would make
//! those tests assert our own mock's behaviour instead of git's.

use std::{ffi::OsString, path::PathBuf, process::Command};

/// One command the daemon wants to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
  /// Program name or absolute path.
  pub program: OsString,
  /// Arguments, excluding `argv[0]`.
  pub args:    Vec<OsString>,
  /// Working directory, when it matters.
  pub cwd:     Option<PathBuf>,
}

impl Invocation {
  /// Build an invocation from string-ish parts.
  pub fn new<P, A, S>(program: P, args: A) -> Self
  where
    P: Into<OsString>,
    A: IntoIterator<Item = S>,
    S: Into<OsString>,
  {
    Self {
      program: program.into(),
      args:    args.into_iter().map(Into::into).collect(),
      cwd:     None,
    }
  }

  /// The argv as a lossy string, for logs and for test assertions.
  #[must_use]
  pub fn display(&self) -> String {
    let mut parts = vec![self.program.to_string_lossy().into_owned()];
    parts.extend(self.args.iter().map(|a| a.to_string_lossy().into_owned()));
    parts.join(" ")
  }
}

/// What a command did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
  /// Exit code, or `None` when the process was signalled.
  pub code:   Option<i32>,
  /// Captured stdout, lossily decoded.
  pub stdout: String,
  /// Captured stderr, lossily decoded.
  pub stderr: String,
}

impl Outcome {
  /// Whether the command exited zero.
  #[must_use]
  pub const fn ok(&self) -> bool {
    matches!(self.code, Some(0))
  }

  /// The failure text to put in a log or a `Degraded` reason.
  ///
  /// Prefers stderr, which is where every tool in this pipeline writes
  /// its diagnostics, and falls back to stdout so a tool that got the
  /// convention wrong still produces a usable message.
  #[must_use]
  pub fn failure_text(&self) -> String {
    let stderr = self.stderr.trim();
    if stderr.is_empty() {
      self.stdout.trim().to_owned()
    } else {
      stderr.to_owned()
    }
  }
}

/// How the daemon runs an external command.
pub trait Exec: Send + Sync {
  /// Run `invocation` to completion, capturing both streams.
  ///
  /// # Errors
  ///
  /// Returns the spawn error. A non-zero exit is *not* an error: it is a
  /// successful observation, reported through [`Outcome::code`].
  fn run(&self, invocation: &Invocation) -> std::io::Result<Outcome>;
}

/// The real thing: `std::process::Command`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemExec;

impl Exec for SystemExec {
  fn run(&self, invocation: &Invocation) -> std::io::Result<Outcome> {
    let mut command = Command::new(&invocation.program);
    command.args(&invocation.args);
    if let Some(cwd) = invocation.cwd.as_ref() {
      command.current_dir(cwd);
    }
    let output = command.output()?;
    Ok(Outcome {
      code:   output.status.code(),
      stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
      stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
  }
}

// ---------------------------------------------------------------------------
// Test double
// ---------------------------------------------------------------------------

/// An [`Exec`] that records what it was asked to run and replies from a
/// script.
///
/// Compiled unconditionally rather than under `cfg(test)`: the scenarios
/// that must never touch a real profile or a real activation live in
/// `tests/`, which links the library as an ordinary dependency and so
/// cannot see anything `cfg(test)` hides.
pub mod recorder {
  use std::sync::Mutex;

  use super::{Exec, Invocation, Outcome};

  /// A scripted, recording [`Exec`].
  #[derive(Debug, Default)]
  pub struct RecordingExec {
    calls:   Mutex<Vec<Invocation>>,
    replies: Mutex<Vec<Outcome>>,
  }

  impl RecordingExec {
    /// An exec that replies with a clean exit to everything.
    #[must_use]
    pub fn always_ok() -> Self {
      Self::default()
    }

    /// Queue one reply. Replies are consumed in order; once the queue is
    /// empty every further call succeeds with empty output.
    #[must_use]
    pub fn with_reply(self, code: i32, stdout: &str, stderr: &str) -> Self {
      if let Ok(mut replies) = self.replies.lock() {
        replies.push(Outcome {
          code:   Some(code),
          stdout: stdout.to_owned(),
          stderr: stderr.to_owned(),
        });
      }
      self
    }

    /// Every invocation seen so far, as `program arg arg` strings.
    #[must_use]
    pub fn argv_log(&self) -> Vec<String> {
      self.calls.lock().map_or_else(
        |poisoned| {
          poisoned
            .into_inner()
            .iter()
            .map(Invocation::display)
            .collect()
        },
        |calls| calls.iter().map(Invocation::display).collect(),
      )
    }
  }

  impl Exec for RecordingExec {
    fn run(&self, invocation: &Invocation) -> std::io::Result<Outcome> {
      if let Ok(mut calls) = self.calls.lock() {
        calls.push(invocation.clone());
      }
      let queued = self.replies.lock().ok().and_then(|mut replies| {
        if replies.is_empty() {
          None
        } else {
          Some(replies.remove(0))
        }
      });
      Ok(queued.unwrap_or(Outcome {
        code:   Some(0),
        stdout: String::new(),
        stderr: String::new(),
      }))
    }
  }
}
