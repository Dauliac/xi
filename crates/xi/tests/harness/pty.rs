//! A real interactive shell on a real pseudo-terminal.
//!
//! Spec: `specs/xi-devshell-test-harness/spec.md#Interactive end-to-end layer`
//!
//! Needed because the devshell's most important behaviour only exists with a
//! tty and a real parent/child shell pair:
//!
//! * non-interactive `bash` does not run `PROMPT_COMMAND` the way interactive
//!   bash does, and zsh `precmd` requires `-i`,
//! * the `exit 200` cd-out sentinel only means anything when a real parent
//!   shell observes the child's status,
//! * environment isolation between nesting levels is a property of two live
//!   processes, not of a string.
//!
//! Built on `nix::pty`, which the workspace already depends on, rather than
//! adding a PTY crate.
//!
//! **Synchronisation is a prompt sentinel, never elapsed time.** We set a
//! unique `PS1`, write a command, and read until the sentinel comes back.
//! Because the activation hook runs *in* the prompt hook, seeing the sentinel
//! is positive proof the hook has finished for that command. Polling the read
//! buffer for the sentinel is polling an observable; it is not a sleep
//! standing in for a signal.

use std::{
  io::{Read, Write},
  os::{
    fd::{AsRawFd, OwnedFd},
    unix::process::CommandExt as _,
  },
  process::{Child, Command},
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};

use super::{Shell, TestWorld};

/// Unique prompt marker. The surrounding punctuation makes an accidental
/// match on ordinary command output effectively impossible.
pub const SENTINEL: &str = "@@XI-PROMPT-READY@@";

/// How long to wait for a sentinel before declaring the shell wedged.
pub const PROMPT_DEADLINE: Duration = Duration::from_secs(20);

const POLL: Duration = Duration::from_millis(10);

nix::ioctl_write_int_bad!(set_controlling_tty, nix::libc::TIOCSCTTY);

pub struct PtyShell {
  child:  Child,
  master: Arc<OwnedFd>,
  buffer: Arc<Mutex<String>>,
  cursor: usize,
  shell:  Shell,
  /// Monotonic command counter, so each end-marker is unique and a stale
  /// marker can never satisfy a later wait.
  seq:    u64,
}

impl PtyShell {
  /// Spawn an interactive shell on a fresh PTY inside `world`.
  ///
  /// Returns `None` when the shell is not installed, so a host without `fish`
  /// skips rather than fails.
  pub fn spawn(world: &TestWorld, shell: Shell, rc_body: &str) -> Option<Self> {
    let bin = shell.skip_if_missing()?;

    // An rc file is how a real user installs the hook, so the test installs
    // it the same way instead of hand-rolling the activation.
    let rc_path = write_rc(world, shell, rc_body);

    let pty = nix::pty::openpty(None, None).expect("openpty failed");
    let master = Arc::new(pty.master);
    let slave = pty.slave;

    // Turn OFF terminal echo. Otherwise the tty echoes every command we type
    // back into the transcript, and a marker-extracting read finds the marker
    // in the echoed *command* instead of in the shell's *reply*. That produced
    // values like "$XI_WORLD" (the unexpanded text) rather than "a".
    {
      use nix::sys::termios;
      if let Ok(mut attrs) = termios::tcgetattr(&slave) {
        attrs.local_flags.remove(termios::LocalFlags::ECHO);
        attrs.local_flags.remove(termios::LocalFlags::ECHOE);
        attrs.local_flags.remove(termios::LocalFlags::ECHOK);
        attrs.local_flags.remove(termios::LocalFlags::ECHONL);
        let _ = termios::tcsetattr(&slave, termios::SetArg::TCSANOW, &attrs);
      }
    }

    let mut cmd = Command::new(&bin);
    match shell {
      Shell::Bash => {
        cmd.arg("--rcfile").arg(&rc_path).arg("-i");
      },
      Shell::Zsh => {
        // zsh reads .zshrc from ZDOTDIR; there is no --rcfile.
        cmd.env("ZDOTDIR", rc_path.parent().unwrap());
        cmd.arg("-i");
      },
      Shell::Fish => {
        cmd.arg("-i");
      },
    }

    cmd.env_clear();
    for (k, v) in world.env() {
      cmd.env(k, v);
    }
    // A dumb terminal keeps escape sequences out of the transcript.
    cmd.env("TERM", "dumb");
    cmd.current_dir(world.flake_root());

    let stdin = slave.try_clone().unwrap();
    let stdout = slave.try_clone().unwrap();
    let stderr = slave.try_clone().unwrap();
    cmd.stdin(stdin).stdout(stdout).stderr(stderr);

    unsafe {
      cmd.pre_exec(move || {
        // New session, then claim the slave (already fd 0) as the
        // controlling terminal. Without this the shell has no tty and will
        // not behave interactively, which is the entire point.
        nix::unistd::setsid().map_err(std::io::Error::from)?;
        set_controlling_tty(0, 0).map_err(std::io::Error::from)?;
        Ok(())
      });
    }

    let child = cmd.spawn().expect("failed to spawn shell on pty");
    drop(slave); // the child owns it now; holding it would block EOF

    let buffer = Arc::new(Mutex::new(String::new()));
    spawn_reader(Arc::clone(&master), Arc::clone(&buffer));

    let mut this = Self {
      child,
      master,
      buffer,
      cursor: 0,
      shell,
      seq: 0,
    };

    // Wait for the rc-driven prompt, then re-assert the sentinel in case the
    // activation hook rewrote PS1, and wait again. After this the shell is
    // known-idle and the cursor is in phase with the prompt stream.
    this.read_until(SENTINEL);
    this.drain();
    this.write_line(&prompt_assignment(shell));
    this.read_until(SENTINEL);

    Some(this)
  }

  /// Run one command and return everything the shell emitted for it.
  ///
  /// Two-phase and deliberately so. Counting prompts alone does not work:
  /// the shell emits prompts on its own schedule (rc processing, entering and
  /// leaving subshells), so a cursor that advances one prompt per command
  /// drifts out of phase and starts reporting the *previous* command's
  /// output. Observed symptom was three sentinels arriving back to back and
  /// every query returning empty.
  ///
  /// So: drain whatever is pending, write the command followed by a unique
  /// end-marker, wait for that marker (the command line has finished), then
  /// wait for the next prompt sentinel (the prompt hook has finished and the
  /// shell is idle again). The marker is unique per command, so it can never
  /// match a stale one.
  pub fn run(&mut self, line: &str) -> String {
    self.seq += 1;
    let mark = format!("@@XI-DONE-{}@@", self.seq);

    self.drain();
    self.write_line(&format!("{line}\nprintf '%s\\n' '{mark}'"));

    let body = self.read_until(&mark);
    // The prompt hook runs after the command line, so the shell is only
    // genuinely idle once the next sentinel lands.
    self.read_until(SENTINEL);
    body
  }

  /// Write a line and wait for nothing at all.
  ///
  /// For commands after which no marker and no prompt can arrive, because the
  /// shell itself is going away (a cascading `exit`).
  pub fn send_raw(&mut self, line: &str) {
    self.drain();
    self.write_line(line);
  }

  /// Run a command that changes shell level, so no end-marker can be
  /// expected.
  ///
  /// `exit` terminates the shell before any trailing `printf` could run, so
  /// waiting for a marker would hang forever. Here the only available signal
  /// is the next prompt, printed by whichever shell is left in charge.
  pub fn run_changing_level(&mut self, line: &str) -> String {
    self.drain();
    self.write_line(line);
    self.read_until(SENTINEL)
  }

  /// Read the value of an environment variable as the live shell sees it.
  ///
  /// Wrapped in markers because the transcript also contains the echoed
  /// command itself, which would otherwise match.
  pub fn env_var(&mut self, name: &str) -> Option<String> {
    let out = self.run(&format!("echo \"<[${name}]>\""));
    extract_between(&out, "<[", "]>").filter(|v| !v.is_empty())
  }

  /// Current working directory as the live shell sees it.
  pub fn pwd(&mut self) -> String {
    let out = self.run("echo \"<[$PWD]>\"");
    extract_between(&out, "<[", "]>").unwrap_or_default()
  }

  /// True when a devshell subshell is currently active.
  pub fn in_devshell(&mut self) -> bool {
    self.env_var("__XI_IN_DEVSHELL").as_deref() == Some("1")
  }

  pub fn depth(&mut self) -> u32 {
    self
      .env_var("__XI_DEPTH")
      .and_then(|d| d.parse().ok())
      .unwrap_or(0)
  }

  fn write_line(&mut self, line: &str) {
    let mut f = std::fs::File::from(self.master.try_clone().unwrap());
    write!(f, "{line}\n").expect("failed to write to pty");
    f.flush().ok();
    // Do not close the fd: File::drop would, and the shell would see EOF.
    std::mem::forget(f);
  }

  /// Discard anything buffered so far, so the next wait cannot match output
  /// that belongs to an earlier command.
  fn drain(&mut self) {
    self.cursor = self.buffer.lock().unwrap().len();
  }

  /// Block until `needle` appears after the cursor, returning the text that
  /// preceded it and leaving the cursor just past it.
  ///
  /// This polls a buffer that a reader thread fills. Polling for bytes to
  /// arrive is waiting on an observable; it is not a fixed sleep standing in
  /// for a signal.
  fn read_until(&mut self, needle: &str) -> String {
    let start = Instant::now();
    loop {
      {
        let buf = self.buffer.lock().unwrap();
        let from = self.cursor.min(buf.len());
        if let Some(found) = buf[from..].find(needle) {
          let end = from + found;
          let chunk = buf[from..end].to_string();
          self.cursor = end + needle.len();
          return chunk;
        }
      }

      if start.elapsed() > PROMPT_DEADLINE {
        let buf = self.buffer.lock().unwrap();
        panic!(
          "timed out after {PROMPT_DEADLINE:?} waiting for {needle:?} from \
           {}.\n--- transcript from cursor ---\n{}\n--- full transcript \
           ---\n{buf}",
          self.shell.name(),
          &buf[self.cursor.min(buf.len())..]
        );
      }
      std::thread::sleep(POLL);
    }
  }

  /// Full transcript so far, for diagnostics.
  pub fn transcript(&self) -> String {
    self.buffer.lock().unwrap().clone()
  }

  /// Exit status of the top-level shell, or `None` while it is still running.
  pub fn exit_status(&mut self) -> Option<i32> {
    match self.child.try_wait() {
      Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
      _ => None,
    }
  }

  /// Wait for the top-level shell to terminate, which is what a cascading
  /// exit must produce.
  ///
  /// Polls the child rather than sleeping a fixed interval.
  pub fn wait_for_exit(&mut self, deadline: Duration) -> Option<i32> {
    let start = Instant::now();
    loop {
      if let Some(code) = self.exit_status() {
        return Some(code);
      }
      if start.elapsed() > deadline {
        return None;
      }
      std::thread::sleep(POLL);
    }
  }
}

impl Drop for PtyShell {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

fn spawn_reader(master: Arc<OwnedFd>, buffer: Arc<Mutex<String>>) {
  std::thread::spawn(move || {
    let fd = master.as_raw_fd();
    let mut file = unsafe {
      use std::os::fd::FromRawFd;
      // Borrowed: the Arc keeps the real fd alive, so do not let this File
      // close it on drop.
      std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(fd))
    };
    let mut chunk = [0u8; 4096];
    loop {
      match file.read(&mut chunk) {
        Ok(0) => break,
        Ok(n) => {
          let text = String::from_utf8_lossy(&chunk[..n]).into_owned();
          buffer.lock().unwrap().push_str(&text);
        },
        // EIO is how Linux reports "the child closed the slave".
        Err(_) => break,
      }
    }
  });
}

fn prompt_assignment(shell: Shell) -> String {
  match shell {
    Shell::Fish => {
      format!("function fish_prompt; printf '%s' '{SENTINEL}'; end")
    },
    _ => format!("PS1='{SENTINEL}'"),
  }
}

/// Write an rc file containing the real activation hook plus `extra`.
fn write_rc(
  world: &TestWorld,
  shell: Shell,
  extra: &str,
) -> std::path::PathBuf {
  let dir = world.home().to_path_buf();
  std::fs::create_dir_all(&dir).unwrap();

  let name = match shell {
    Shell::Bash => ".bashrc",
    Shell::Zsh => ".zshrc",
    Shell::Fish => "config.fish",
  };
  let path = dir.join(name);

  // Keep the prompt sentinel stable even if the hook rewrites PS1.
  let body = format!("{extra}\nPS1='{SENTINEL}'\n");
  std::fs::write(&path, body).unwrap();
  path
}

fn extract_between(haystack: &str, open: &str, close: &str) -> Option<String> {
  // Search from the end: the echoed command line contains the literal
  // markers too, and the value we want is the shell's reply, which comes
  // after it.
  let close_at = haystack.rfind(close)?;
  let open_at = haystack[..close_at].rfind(open)?;
  Some(haystack[open_at + open.len()..close_at].trim().to_string())
}
