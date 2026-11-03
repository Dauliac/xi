//! The `xi-sources-bootstrap` capability: turning a bare machine into a
//! managed one, reversibly.
//!
//! The field has converged on a two-stage bootstrap and so has xi:
//!
//! 1. `scripts/bootstrap.sh` detects the host, downloads the binary, verifies
//!    it against an inline SHA-256, and `exec`s it. That is all it does.
//! 2. This module carries everything else. The planner, the preflight checks,
//!    the installer delegation, and the receipt that makes the whole thing
//!    reversible all live in the binary, which is what lets a bare machine and
//!    a managed machine take the exact same command.
//!
//! Nothing here reaches the machine except through [`Runner`], so the
//! paths that install Nix, call `sudo`, or `scp` a binary to another host
//! are all exercised in tests without doing any of those things.
//!
//! Spec:
//! `openspec/changes/xi-sources-bootstrap/specs/xi-sources-bootstrap/spec.md`

use std::{
  cell::RefCell,
  fmt,
  fs,
  io,
  path::{Path, PathBuf},
  process::{Command, Stdio},
  time::SystemTime,
};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{
  Scope,
  config::{Installer, ScopePaths},
  lock::{ActorName, LockOptions, SourcesLock},
};

// ---------------------------------------------------------------------------
// Env toggles
// ---------------------------------------------------------------------------

/// Suppresses every confirmation prompt on the bootstrap path.
///
/// Read by both halves of the bootstrap: the shell stub honours it when
/// it cannot reattach a tty, and the planner honours it instead of
/// printing its plan and asking.
pub const NO_CONFIRM_ENV: &str = "XI_BOOTSTRAP_NO_CONFIRM";

/// Base URL the shell stub downloads the binary from.
///
/// Named here so the stub and the documentation cannot drift; the binary
/// itself never downloads anything.
pub const URL_OVERRIDE_ENV: &str = "XI_BOOTSTRAP_URL_OVERRIDE";

/// Whether confirmation prompts are suppressed for this process.
#[must_use]
pub fn no_confirm_from_env() -> bool {
  std::env::var(NO_CONFIRM_ENV).is_ok_and(|v| {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
  })
}

// ---------------------------------------------------------------------------
// Runner  -  the only way this module touches the machine
// ---------------------------------------------------------------------------

/// The exit status and captured output of one command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutcome {
  /// Exit code, or 128 + signal when the child was killed.
  pub code:   i32,
  /// Captured stdout, empty for the streaming variants.
  pub stdout: String,
  /// Captured stderr, empty for the streaming variants.
  pub stderr: String,
}

impl CommandOutcome {
  /// A clean exit.
  #[must_use]
  pub const fn success(&self) -> bool {
    self.code == 0
  }

  /// The outcome of a command that was never run.
  #[must_use]
  pub const fn skipped() -> Self {
    Self {
      code:   0,
      stdout: String::new(),
      stderr: String::new(),
    }
  }
}

/// Everything the planner is allowed to do to the machine.
///
/// One trait rather than scattered `Command::new` calls, because the
/// actions this capability performs (install Nix, call sudo, `scp` a
/// binary to a host) are precisely the actions a test must never
/// actually perform. [`RecordingRunner`] is the double.
pub trait Runner {
  /// Whether `program` resolves on `PATH`.
  fn has_program(&self, program: &str) -> bool;

  /// Run `argv`, capturing stdout and stderr.
  ///
  /// # Errors
  ///
  /// Fails when the program cannot be spawned. A command that ran and
  /// exited non-zero is an `Ok` outcome carrying its code: the caller
  /// decides whether that code is fatal.
  fn run(&self, argv: &[String]) -> io::Result<CommandOutcome>;

  /// Run `argv` with the operator's stdio, so its logs stream live.
  ///
  /// # Errors
  ///
  /// Fails when the program cannot be spawned; see [`Runner::run`].
  fn run_streaming(&self, argv: &[String]) -> io::Result<CommandOutcome>;

  /// Run a shell pipeline with the operator's stdio.
  ///
  /// The upstream Nix installers are documented, supported, and tested
  /// as `curl … | sh -s -- install …`. Decomposing that into an argv
  /// would mean re-implementing their redirect handling and owning the
  /// result; shelling out keeps the command byte-identical to the one
  /// their documentation prints.
  ///
  /// # Errors
  ///
  /// Fails when the shell itself cannot be spawned; see
  /// [`Runner::run`].
  fn run_shell(&self, script: &str) -> io::Result<CommandOutcome>;
}

/// The real machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl SystemRunner {
  fn finish(mut cmd: Command, capture: bool) -> io::Result<CommandOutcome> {
    if capture {
      let out = cmd.output()?;
      return Ok(CommandOutcome {
        code:   exit_code(out.status),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
      });
    }
    let status = cmd
      .stdin(Stdio::inherit())
      .stdout(Stdio::inherit())
      .stderr(Stdio::inherit())
      .status()?;
    Ok(CommandOutcome {
      code:   exit_code(status),
      stdout: String::new(),
      stderr: String::new(),
    })
  }
}

/// A process's exit code, folding a signal death into `128 + signal`.
fn exit_code(status: std::process::ExitStatus) -> i32 {
  status.code().unwrap_or_else(|| {
    #[cfg(unix)]
    {
      use std::os::unix::process::ExitStatusExt as _;
      status.signal().map_or(1, |sig| 128 + sig)
    }
    #[cfg(not(unix))]
    {
      1
    }
  })
}

impl Runner for SystemRunner {
  fn has_program(&self, program: &str) -> bool {
    which::which(program).is_ok()
  }

  fn run(&self, argv: &[String]) -> io::Result<CommandOutcome> {
    let Some((program, args)) = argv.split_first() else {
      return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    };
    debug!("running {argv:?}");
    let mut cmd = Command::new(program);
    cmd.args(args);
    Self::finish(cmd, true)
  }

  fn run_streaming(&self, argv: &[String]) -> io::Result<CommandOutcome> {
    let Some((program, args)) = argv.split_first() else {
      return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    };
    debug!("running {argv:?}");
    let mut cmd = Command::new(program);
    cmd.args(args);
    Self::finish(cmd, false)
  }

  fn run_shell(&self, script: &str) -> io::Result<CommandOutcome> {
    debug!("running shell: {script}");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    Self::finish(cmd, false)
  }
}

/// A runner that records instead of running.
///
/// This is the dry-run executor and the test double in one: it answers
/// `has_program` from a fixed list, returns a scripted exit code for any
/// command whose text matches a registered substring, and keeps every
/// invocation in order so a test can assert on what the planner *would*
/// have done.
#[derive(Debug, Default)]
pub struct RecordingRunner {
  programs: Vec<String>,
  failures: Vec<(String, i32)>,
  calls:    RefCell<Vec<String>>,
}

impl RecordingRunner {
  /// A runner where no program exists and every command succeeds.
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Declare `program` present on `PATH`.
  #[must_use]
  pub fn with_program(mut self, program: &str) -> Self {
    self.programs.push(program.to_owned());
    self
  }

  /// Make any command whose text contains `needle` exit with `code`.
  #[must_use]
  pub fn failing(mut self, needle: &str, code: i32) -> Self {
    self.failures.push((needle.to_owned(), code));
    self
  }

  /// Every command this runner was asked to run, in order.
  #[must_use]
  pub fn calls(&self) -> Vec<String> {
    self.calls.borrow().clone()
  }

  /// Whether any recorded command contains `needle`.
  #[must_use]
  pub fn ran(&self, needle: &str) -> bool {
    self.calls.borrow().iter().any(|c| c.contains(needle))
  }

  fn record(&self, text: String) -> CommandOutcome {
    let code = self
      .failures
      .iter()
      .find_map(|(needle, code)| text.contains(needle).then_some(*code))
      .unwrap_or(0);
    self.calls.borrow_mut().push(text);
    CommandOutcome {
      code,
      stdout: String::new(),
      stderr: String::new(),
    }
  }
}

impl Runner for RecordingRunner {
  fn has_program(&self, program: &str) -> bool {
    self.programs.iter().any(|p| p == program)
  }

  fn run(&self, argv: &[String]) -> io::Result<CommandOutcome> {
    Ok(self.record(argv.join(" ")))
  }

  fn run_streaming(&self, argv: &[String]) -> io::Result<CommandOutcome> {
    Ok(self.record(argv.join(" ")))
  }

  fn run_shell(&self, script: &str) -> io::Result<CommandOutcome> {
    Ok(self.record(script.to_owned()))
  }
}

// ---------------------------------------------------------------------------
// Host facts  -  everything preflight needs to know, as data
// ---------------------------------------------------------------------------

/// `SELinux` enforcement state, as reported by `/sys/fs/selinux/enforce`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Selinux {
  /// No `SELinux` on this host.
  Absent,
  /// Loaded but not enforcing.
  Permissive,
  /// Enforcing. The upstream installers cannot label `/nix` here.
  Enforcing,
}

/// Which flavour of WSL, if any, this host is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wsl {
  /// Not WSL.
  No,
  /// WSL1, which has no working `systemd` and no real kernel.
  One,
  /// WSL2, which is a normal Linux kernel and is supported.
  Two,
}

/// Where `SELinux` publishes its enforcement state.
pub const SELINUX_ENFORCE_PATH: &str = "/sys/fs/selinux/enforce";

/// Where Linux publishes the kernel release string.
pub const OSRELEASE_PATH: &str = "/proc/sys/kernel/osrelease";

/// Classify the contents of [`SELINUX_ENFORCE_PATH`].
///
/// `None` means the file is absent, which is how a host without `SELinux`
/// presents. Pure so the three outcomes are testable on any machine.
#[must_use]
pub fn classify_selinux(enforce_file: Option<&str>) -> Selinux {
  match enforce_file.map(str::trim) {
    None => Selinux::Absent,
    Some("1") => Selinux::Enforcing,
    Some(_) => Selinux::Permissive,
  }
}

/// Classify the kernel release string into a WSL flavour.
///
/// WSL2 advertises itself in the release string (`…-microsoft-standard-WSL2`);
/// WSL1 says only `Microsoft`. Anything without `microsoft` is not WSL.
#[must_use]
pub fn classify_wsl(osrelease: Option<&str>) -> Wsl {
  let Some(release) = osrelease.map(str::trim).filter(|r| !r.is_empty()) else {
    return Wsl::No;
  };
  let lower = release.to_ascii_lowercase();
  if !lower.contains("microsoft") {
    return Wsl::No;
  }
  if lower.contains("wsl2") {
    Wsl::Two
  } else {
    Wsl::One
  }
}

/// The `uname -s` family this binary was built for, lowercased.
///
/// Spelled the way `uname -s` spells it so the mismatch error reads the
/// way the spec states the rule.
#[must_use]
pub const fn host_os() -> &'static str {
  if cfg!(target_os = "macos") {
    "darwin"
  } else {
    std::env::consts::OS
  }
}

/// Everything [`check_preflight`] inspects, gathered once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
  /// `uname -s`, lowercased: `linux`, `darwin`, …
  pub os:                 String,
  /// Whether a usable `nix` is on `PATH`.
  pub nix_present:        bool,
  /// Whether a receipt from a previous bootstrap exists.
  pub receipt:            Option<PathBuf>,
  /// Whether `sudo -n true` succeeds (or this process is already root).
  pub passwordless_sudo:  bool,
  /// `SELinux` enforcement state.
  pub selinux:            Selinux,
  /// WSL flavour.
  pub wsl:                Wsl,
  /// The substituters the resolved config repo declares, if readable.
  pub declares_lix_cache: bool,
}

impl HostFacts {
  /// Probe the live machine.
  ///
  /// Every probe is cheap and local: a `PATH` lookup, two small reads
  /// under `/sys` and `/proc`, one `sudo -n true`, and a bounded text
  /// scan of the config repo. Nothing here touches the network, because
  /// preflight has to work before Nix exists.
  #[must_use]
  pub fn probe(
    runner: &dyn Runner,
    receipt_path: &Path,
    sources_dir: &Path,
  ) -> Self {
    let sudo = is_root()
      || runner
        .run(&argv(["sudo", "-n", "true"]))
        .is_ok_and(|out| out.success());

    Self {
      os:                 host_os().to_owned(),
      nix_present:        runner.has_program("nix"),
      receipt:            receipt_path
        .is_file()
        .then(|| receipt_path.to_path_buf()),
      passwordless_sudo:  sudo,
      selinux:            classify_selinux(
        fs::read_to_string(SELINUX_ENFORCE_PATH).ok().as_deref(),
      ),
      wsl:                classify_wsl(
        fs::read_to_string(OSRELEASE_PATH).ok().as_deref(),
      ),
      declares_lix_cache: declares_lix_cache(sources_dir),
    }
  }
}

/// Whether this process is already root, in which case `sudo` is moot.
fn is_root() -> bool {
  nix::unistd::Uid::effective().is_root()
}

/// Build an owned argv from string literals.
fn argv<const N: usize>(parts: [&str; N]) -> Vec<String> {
  parts.iter().map(|p| (*p).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

/// The escape hatches that let preflight proceed anyway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreflightOpts {
  /// Bootstrap a host that already carries a receipt.
  pub force:            bool,
  /// Let `sudo` prompt instead of requiring `sudo -n true`.
  pub interactive_sudo: bool,
  /// Proceed on an SELinux-enforcing host.
  pub allow_selinux:    bool,
  /// The `uname -s` value this verb manages, when the verb pins one.
  ///
  /// `None` skips the check. Scope alone cannot answer it: `system` and
  /// `darwin` are both [`Scope::System`] and disagree about the OS.
  pub expect_os:        Option<&'static str>,
}

/// Why preflight refused.
///
/// Every variant names the flag or the command that gets past it: a
/// refusal the operator cannot act on is just an outage.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
  /// A receipt from a previous bootstrap is already on the host.
  #[error(
    "this host is already managed by xi: {} records a previous \
     bootstrap.\nRe-run with --force to bootstrap it again, or `xi system \
     uninstall` to reverse it first.",
    path.display()
  )]
  AlreadyBootstrapped {
    /// The receipt that was found.
    path: PathBuf,
  },

  /// `sudo -n true` failed and no interactive fallback was permitted.
  #[error(
    "`sudo -n true` failed: xi cannot elevate without a password.\nRun `sudo \
     -v` first, or pass --interactive-sudo to let sudo prompt."
  )]
  NoPasswordlessSudo,

  /// The host OS is not the one this verb manages.
  #[error(
    "`uname -s` reports {actual}, but this verb manages {expected} hosts"
  )]
  OsMismatch {
    /// What the verb manages.
    expected: String,
    /// What the host actually is.
    actual:   String,
  },

  /// `SELinux` is enforcing, which breaks `/nix` labelling.
  #[error(
    "SELinux is in Enforcing mode; the Nix installer cannot label /nix.\nSet \
     it permissive (`setenforce 0`) or pass --allow-selinux to try anyway."
  )]
  SelinuxEnforcing,

  /// WSL1 cannot run the Nix daemon.
  #[error(
    "this host is WSL1, which cannot run the Nix daemon.\nUpgrade the \
     distribution with `wsl --set-version <distro> 2`."
  )]
  Wsl1,
}

/// Decide whether this host may be bootstrapped.
///
/// Pure: every input is in `facts`, so each refusal has a unit test and
/// none of them need the condition to be true of the test machine.
///
/// Checks run in the spec's order, which is also cheapest-consequence
/// first: an already-managed host is a no-op the operator probably
/// wants, a missing sudo is a fixable setup problem, and the three
/// platform rejections are the ones that would otherwise fail halfway
/// through installing Nix.
///
/// # Errors
///
/// Returns the first refusal that applies.
pub fn check_preflight(
  facts: &HostFacts,
  opts: &PreflightOpts,
) -> Result<(), PreflightError> {
  if let Some(path) = facts.receipt.as_ref()
    && !opts.force
  {
    return Err(PreflightError::AlreadyBootstrapped { path: path.clone() });
  }

  if !facts.passwordless_sudo && !opts.interactive_sudo {
    return Err(PreflightError::NoPasswordlessSudo);
  }

  if let Some(expected) = opts.expect_os
    && expected != facts.os
  {
    return Err(PreflightError::OsMismatch {
      expected: expected.to_owned(),
      actual:   facts.os.clone(),
    });
  }

  if facts.selinux == Selinux::Enforcing && !opts.allow_selinux {
    return Err(PreflightError::SelinuxEnforcing);
  }

  if facts.wsl == Wsl::One {
    return Err(PreflightError::Wsl1);
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// Installer backends
// ---------------------------------------------------------------------------

/// The substituter whose presence makes [`Installer::Lix`] the default.
pub const LIX_SUBSTITUTER: &str = "cache.lix.systems";

/// The documented one-liner for each installer backend.
///
/// Byte-identical to what each project's own documentation prints, on
/// purpose: xi delegates the planner complexity (`SELinux`, WSL, APFS
/// volumes, daemon units) rather than vendoring it, and a command that
/// has drifted from upstream's is a command upstream will not support.
#[must_use]
pub const fn installer_script(backend: Installer) -> &'static str {
  match backend {
    Installer::Lix => {
      "curl -sSf -L https://install.lix.systems/lix | sh -s -- install \
       --no-confirm"
    },
    Installer::Detsys | Installer::None => {
      "curl -LsSf https://install.determinate.systems/nix | sh -s -- install \
       --no-confirm --determinate=false"
    },
  }
}

/// The command that reverses an install by `backend`.
///
/// Both backends are the Determinate Systems installer - Lix ships a
/// fork of it - so both leave the same `/nix/nix-installer` behind and
/// both reverse through it. The backend is still recorded in the
/// receipt and still branched on here, so the day the two diverge there
/// is one place to change.
#[must_use]
pub fn nix_uninstall_argv(backend: Installer) -> Option<Vec<String>> {
  match backend {
    Installer::Detsys | Installer::Lix => {
      Some(argv(["/nix/nix-installer", "uninstall", "--no-confirm"]))
    },
    // Nothing was installed, so there is nothing to reverse.
    Installer::None => None,
  }
}

/// Resolve which backend to use when no layer named one.
///
/// The heuristic is deliberately textual rather than evaluated: this
/// runs when `nix` may not exist yet, so `nix eval` is not available to
/// ask the flake what its substituters are. A config repo that mentions
/// [`LIX_SUBSTITUTER`] anywhere in its Nix sources is declaring it wants
/// the Lix cache, and installing Lix is then the choice that makes its
/// first build hit that cache.
#[must_use]
pub const fn default_installer(declares_lix_cache: bool) -> Installer {
  if declares_lix_cache {
    Installer::Lix
  } else {
    Installer::Detsys
  }
}

/// Resolve the backend, honouring an explicit setting over the heuristic.
#[must_use]
pub fn resolve_installer(
  explicit: Option<Installer>,
  declares_lix_cache: bool,
) -> Installer {
  explicit.unwrap_or_else(|| default_installer(declares_lix_cache))
}

/// How deep into the config repo the substituter scan looks.
pub const SCAN_MAX_DEPTH: usize = 3;

/// How many files the substituter scan is willing to read.
pub const SCAN_MAX_FILES: usize = 512;

/// Hard cap on one scanned file, so a vendored blob cannot stall a switch.
pub const SCAN_MAX_BYTES: u64 = 1024 * 1024;

/// Whether the config repo declares the Lix cache as a substituter.
///
/// A bounded scan of the repo's Nix sources: at most
/// [`SCAN_MAX_FILES`] files, [`SCAN_MAX_DEPTH`] levels deep, skipping
/// anything bigger than [`SCAN_MAX_BYTES`]. A missing directory answers
/// `false`, which is the right answer on a machine that has not cloned
/// anything yet.
#[must_use]
pub fn declares_lix_cache(sources_dir: &Path) -> bool {
  let mut budget = SCAN_MAX_FILES;
  scan_for(sources_dir, LIX_SUBSTITUTER, SCAN_MAX_DEPTH, &mut budget)
}

fn scan_for(
  dir: &Path,
  needle: &str,
  depth: usize,
  budget: &mut usize,
) -> bool {
  let Ok(entries) = fs::read_dir(dir) else {
    return false;
  };

  for entry in entries.flatten() {
    if *budget == 0 {
      return false;
    }
    let path = entry.path();
    let Ok(meta) = entry.metadata() else { continue };

    if meta.is_dir() {
      // `.git` is the clone's own metadata and holds no configuration.
      if path.file_name().is_some_and(|n| n == ".git") || depth == 0 {
        continue;
      }
      if scan_for(&path, needle, depth - 1, budget) {
        return true;
      }
      continue;
    }

    let is_nix = path.extension().is_some_and(|e| e == "nix")
      || path.file_name().is_some_and(|n| n == "nix.conf");
    if !is_nix || meta.len() > SCAN_MAX_BYTES {
      continue;
    }

    *budget -= 1;
    if fs::read_to_string(&path).is_ok_and(|raw| raw.contains(needle)) {
      debug!("{} declares {needle}", path.display());
      return true;
    }
  }

  false
}

/// The error text for `installer = "none"` on a host without Nix.
///
/// Names the key and all three of its values, because the only way out
/// of this refusal is to change that key or install Nix by hand.
#[must_use]
pub fn installer_none_error() -> String {
  format!(
    "nix is not on PATH and `installer = \"none\"` opts out of installing \
     it.\nSet `installer` to one of {} in [sources], or install Nix yourself \
     and re-run.",
    Installer::ALL.join(", ")
  )
}

// ---------------------------------------------------------------------------
// Receipt
// ---------------------------------------------------------------------------

/// Filename of the receipt, at both scopes.
pub const RECEIPT_FILE: &str = "bootstrap-receipt.json";

/// Schema version of the receipt document.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Directory the receipt lives in for `scope`.
///
/// System scope puts it at `/var/lib/xi/`, one level above the state
/// dir, because the receipt describes the bootstrap of the *host* rather
/// than the state of one adopted source. User scope has nowhere above
/// `${XDG_STATE_HOME}/xi` to put it, so it goes in the state dir. Both
/// follow `XI_SOURCES_STATE_DIR` when it redirects the state dir, which
/// is what keeps the whole capability testable without writing to
/// `/var`.
#[must_use]
pub fn receipt_dir(scope: Scope, paths: &ScopePaths) -> PathBuf {
  match scope {
    Scope::User => paths.state_dir.clone(),
    Scope::System => {
      paths
        .state_dir
        .parent()
        .map_or_else(|| paths.state_dir.clone(), Path::to_path_buf)
    },
  }
}

/// Full path of the receipt for `scope`.
#[must_use]
pub fn receipt_path(scope: Scope, paths: &ScopePaths) -> PathBuf {
  receipt_dir(scope, paths).join(RECEIPT_FILE)
}

/// One planner phase, as it appears in the receipt.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
  /// Preflight gate.
  CheckPreflight,
  /// Delegated Nix install.
  InstallNix,
  /// Clone of the config repo into the sources dir.
  CloneSources,
  /// Substituter configuration, before the first build.
  CachePrestage,
  /// The first build and activation.
  FirstSwitch,
  /// Enabling the gitops daemon.
  EnableGitops,
  /// Closing the receipt.
  WriteReceipt,
}

impl ActionKind {
  /// The wire spelling, matching the serde representation.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::CheckPreflight => "check-preflight",
      Self::InstallNix => "install-nix",
      Self::CloneSources => "clone-sources",
      Self::CachePrestage => "cache-prestage",
      Self::FirstSwitch => "first-switch",
      Self::EnableGitops => "enable-gitops",
      Self::WriteReceipt => "write-receipt",
    }
  }
}

impl fmt::Display for ActionKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// How one applied phase is undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reversal", rename_all = "kebab-case")]
pub enum Reversal {
  /// Nothing to undo. A gate that only read the machine, or a phase
  /// whose effect is not something `uninstall` should replay.
  Nothing {
    /// Why there is nothing to do, for the operator reading the file.
    note: String,
  },
  /// Run this command.
  Run {
    /// The exact argv, so the receipt is readable without xi.
    argv: Vec<String>,
  },
  /// Delete this path, recursively.
  RemovePath {
    /// What to delete.
    path: PathBuf,
  },
  /// A sibling capability owns the reversal and has not landed yet.
  ///
  /// Recorded rather than silently dropped: a receipt that claims a
  /// phase is reversible when nothing can reverse it is worse than one
  /// that says who owes the implementation.
  Pending {
    /// The capability that owns it, e.g. `xi-sources-cache-prestage`.
    capability: String,
    /// What it will have to undo.
    note:       String,
  },
}

impl Reversal {
  /// Nothing to undo, with the reason.
  #[must_use]
  pub fn nothing(note: &str) -> Self {
    Self::Nothing {
      note: note.to_owned(),
    }
  }

  /// A sibling capability's job.
  #[must_use]
  pub fn pending(capability: &str, note: &str) -> Self {
    Self::Pending {
      capability: capability.to_owned(),
      note:       note.to_owned(),
    }
  }

  /// Execute this reversal.
  ///
  /// # Errors
  ///
  /// Propagates I/O failures and non-zero exits from [`Reversal::Run`].
  pub fn execute(&self, runner: &dyn Runner) -> Result<(), BootstrapError> {
    match self {
      Self::Nothing { note } => {
        debug!("nothing to reverse: {note}");
        Ok(())
      },
      Self::Run { argv } => {
        let out = runner.run_streaming(argv)?;
        if out.success() {
          return Ok(());
        }
        Err(BootstrapError::ReversalFailed {
          command: argv.join(" "),
          code:    out.code,
        })
      },
      Self::RemovePath { path } => {
        if !path.exists() {
          debug!("{} is already gone", path.display());
          return Ok(());
        }
        info!("removing {}", path.display());
        if path.is_dir() {
          fs::remove_dir_all(path)?;
        } else {
          fs::remove_file(path)?;
        }
        Ok(())
      },
      Self::Pending { capability, note } => {
        warn!(
          "cannot reverse {note}: {capability} owns that reversal and is not \
           implemented yet"
        );
        Ok(())
      },
    }
  }
}

/// One applied phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptEntry {
  /// Which phase this was.
  pub action:           ActionKind,
  /// When it was applied, RFC 3339 to the second.
  pub applied_at:       String,
  /// How to undo it.
  #[serde(flatten)]
  pub reversal:         Reversal,
  /// Which backend installed Nix, present only on `install-nix`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub nix_installed_by: Option<Installer>,
}

impl ReceiptEntry {
  /// Stamp an entry with the current time.
  #[must_use]
  pub fn now(action: ActionKind, reversal: Reversal) -> Self {
    Self {
      action,
      applied_at: timestamp(),
      reversal,
      nix_installed_by: None,
    }
  }

  /// Record which backend installed Nix.
  #[must_use]
  pub const fn installed_by(mut self, backend: Installer) -> Self {
    self.nix_installed_by = Some(backend);
    self
  }
}

/// The current time, RFC 3339 to the second.
fn timestamp() -> String {
  humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}

/// What a bootstrap did to this host, and how to undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
  /// See [`RECEIPT_SCHEMA_VERSION`].
  pub schema_version: u32,
  /// Which half of the machine was bootstrapped.
  pub scope:          Scope,
  /// The config repo that was adopted, if the run got that far.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub remote:         Option<String>,
  /// Applied phases, in the order they were applied.
  pub entries:        Vec<ReceiptEntry>,
}

impl Receipt {
  /// An empty receipt for `scope`.
  #[must_use]
  pub const fn new(scope: Scope) -> Self {
    Self {
      schema_version: RECEIPT_SCHEMA_VERSION,
      scope,
      remote: None,
      entries: Vec::new(),
    }
  }

  /// Name the config repo this bootstrap adopted.
  #[must_use]
  pub fn for_remote(mut self, remote: Option<String>) -> Self {
    self.remote = remote;
    self
  }

  /// Load the receipt at `path`.
  ///
  /// An absent file is `Ok(None)`: "never bootstrapped" is a normal
  /// state. A present but unparsable file is an error, because
  /// treating it as absent would let a second bootstrap run over the
  /// first one's state.
  ///
  /// # Errors
  ///
  /// Returns [`BootstrapError::BadReceipt`] for a malformed file and
  /// propagates other I/O errors.
  pub fn load(path: &Path) -> Result<Option<Self>, BootstrapError> {
    let raw = match fs::read_to_string(path) {
      Ok(raw) => raw,
      Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(e) => return Err(BootstrapError::Io(e)),
    };
    serde_json::from_str(&raw).map(Some).map_err(|source| {
      BootstrapError::BadReceipt {
        path: path.to_path_buf(),
        source,
      }
    })
  }

  /// Write the receipt to `path`, owner-only.
  ///
  /// Written through a temporary file in the same directory and renamed
  /// into place, so a crash mid-write leaves the previous receipt rather
  /// than a truncated one. See `xi-sources-crash-recovery`.
  ///
  /// # Errors
  ///
  /// Propagates I/O and serialisation errors.
  pub fn save(&self, path: &Path) -> Result<(), BootstrapError> {
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(self)
      .map_err(|e| BootstrapError::Io(io::Error::other(e)))?;

    // Through the shared helper rather than a hand-rolled
    // temp-and-rename. Both are atomic, but only the helper's
    // `<name>.tmp.<pid>` naming is one `super::repair` can sweep and
    // `unknown_state_files` can tell apart from corruption: a receipt
    // write killed before its rename used to leave a
    // `bootstrap-receipt.json.tmp` that nothing would ever clean up and
    // that degraded the status envelope for good.
    // `0600`, the mode `harden` applied before, and applied to the temp
    // file so the receipt is never briefly readable at the umask.
    super::atomic::write_atomic(path, body.as_bytes(), Some(0o600))?;
    Ok(())
  }

  /// Which backend installed Nix during this bootstrap, if any did.
  #[must_use]
  pub fn nix_installed_by(&self) -> Option<Installer> {
    self
      .entries
      .iter()
      .find(|e| e.action == ActionKind::InstallNix)
      .and_then(|e| e.nix_installed_by)
  }

  /// Whether `action` was applied.
  #[must_use]
  pub fn applied(&self, action: ActionKind) -> bool {
    self.entries.iter().any(|e| e.action == action)
  }

  /// Every applied phase, in order.
  #[must_use]
  pub fn applied_kinds(&self) -> Vec<ActionKind> {
    self.entries.iter().map(|e| e.action).collect()
  }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a bootstrap stopped.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
  /// Preflight refused.
  #[error(transparent)]
  Preflight(#[from] PreflightError),

  /// The delegated installer exited non-zero.
  #[error(
    "the {backend} Nix installer exited {code}.\nNothing was recorded; fix \
     the installer's complaint and re-run, or pick another backend with \
     --installer."
  )]
  InstallerFailed {
    /// Which backend was invoked.
    backend: Installer,
    /// Its exit code.
    code:    i32,
  },

  /// `installer = "none"`, and Nix is still absent.
  #[error("{0}")]
  NixStillAbsent(String),

  /// A phase's command failed.
  #[error("{action} failed: `{command}` exited {code}")]
  PhaseFailed {
    /// Which phase.
    action:  ActionKind,
    /// The command it ran.
    command: String,
    /// Its exit code.
    code:    i32,
  },

  /// A reversal command failed during `uninstall`.
  #[error("reversal `{command}` exited {code}")]
  ReversalFailed {
    /// The command that failed.
    command: String,
    /// Its exit code.
    code:    i32,
  },

  /// The operator declined the plan.
  #[error("bootstrap declined; nothing was changed")]
  Declined,

  /// There is no receipt to reverse.
  #[error(
    "no bootstrap receipt at {}: this host was not bootstrapped by xi, so \
     there is nothing to uninstall",
    path.display()
  )]
  NoReceipt {
    /// Where the receipt was looked for.
    path: PathBuf,
  },

  /// The receipt exists but cannot be parsed.
  #[error("the receipt at {} is malformed: {source}", path.display())]
  BadReceipt {
    /// The malformed file.
    path:   PathBuf,
    /// The parse failure.
    #[source]
    source: serde_json::Error,
  },

  /// The sources lock could not be acquired: another actor holds it
  /// (named, with its intent and the estimated wait), or
  /// `XI_SOURCES_LOCK_WAIT` is set to something unparsable.
  #[error("{0}")]
  Lock(String),

  /// An SSH target that is not a bare `ssh://[user@]host[:port]`.
  #[error(
    "`{raw}` is not a remote-bootstrap target.\nExpected \
     `ssh://[user@]host[:port]` with no path, as in `ssh://admin@example.net`."
  )]
  BadSshTarget {
    /// What the operator wrote.
    raw: String,
  },

  /// Something underneath failed.
  #[error(transparent)]
  Io(#[from] io::Error),
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Which rebuild command activates a given scope's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SwitchBackend {
  /// NixOS.
  NixosRebuild,
  /// nix-darwin.
  DarwinRebuild,
  /// home-manager.
  HomeManager,
}

impl SwitchBackend {
  /// The argv that performs the first switch from `sources_dir`.
  #[must_use]
  pub fn argv(self, sources_dir: &Path, host: Option<&str>) -> Vec<String> {
    let flake = host.map_or_else(
      || sources_dir.display().to_string(),
      |h| format!("{}#{h}", sources_dir.display()),
    );
    match self {
      Self::NixosRebuild => {
        vec![
          "nixos-rebuild".to_owned(),
          "switch".to_owned(),
          "--flake".to_owned(),
          flake,
        ]
      },
      Self::DarwinRebuild => {
        vec![
          "darwin-rebuild".to_owned(),
          "switch".to_owned(),
          "--flake".to_owned(),
          flake,
        ]
      },
      Self::HomeManager => {
        vec![
          "home-manager".to_owned(),
          "switch".to_owned(),
          "--flake".to_owned(),
          flake,
        ]
      },
    }
  }
}

/// One planner phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
  /// Gate the whole run.
  CheckPreflight(PreflightOpts),
  /// Delegate the Nix install, unless Nix is already present.
  InstallNix {
    /// The resolved backend.
    backend: Installer,
  },
  /// Clone the config repo.
  CloneSources {
    /// Remote git URL.
    remote: String,
    /// Branch to track.
    branch: String,
    /// Where the clone goes.
    dest:   PathBuf,
  },
  /// Configure substituters before the first build.
  CachePrestage {
    /// The clone whose substituters are read.
    sources_dir: PathBuf,
  },
  /// Build and activate.
  FirstSwitch {
    /// Which rebuild command to invoke.
    backend:     SwitchBackend,
    /// The flake to build from.
    sources_dir: PathBuf,
    /// The configuration name within the flake.
    host:        Option<String>,
  },
  /// Turn the gitops daemon on.
  EnableGitops {
    /// Which scope's daemon.
    scope: Scope,
  },
  /// Close the receipt.
  WriteReceipt {
    /// Where the receipt lives.
    path: PathBuf,
  },
}

impl Action {
  /// Which phase this is.
  #[must_use]
  pub const fn kind(&self) -> ActionKind {
    match self {
      Self::CheckPreflight(..) => ActionKind::CheckPreflight,
      Self::InstallNix { .. } => ActionKind::InstallNix,
      Self::CloneSources { .. } => ActionKind::CloneSources,
      Self::CachePrestage { .. } => ActionKind::CachePrestage,
      Self::FirstSwitch { .. } => ActionKind::FirstSwitch,
      Self::EnableGitops { .. } => ActionKind::EnableGitops,
      Self::WriteReceipt { .. } => ActionKind::WriteReceipt,
    }
  }

  /// One line for the plan preview.
  #[must_use]
  pub fn describe(&self) -> String {
    match self {
      Self::CheckPreflight(..) => {
        "check this host can be bootstrapped".to_owned()
      },
      Self::InstallNix { backend } => {
        format!("install Nix with the {backend} installer, if nix is absent")
      },
      Self::CloneSources {
        remote,
        branch,
        dest,
      } => {
        format!("clone {remote} ({branch}) into {}", dest.display())
      },
      Self::CachePrestage { .. } => {
        "configure substituters before the first build".to_owned()
      },
      Self::FirstSwitch {
        backend,
        sources_dir,
        host,
      } => {
        format!(
          "build and activate with `{}`",
          backend.argv(sources_dir, host.as_deref()).join(" ")
        )
      },
      Self::EnableGitops { scope } => {
        format!("enable the gitops daemon for the {scope} scope")
      },
      Self::WriteReceipt { path } => {
        format!("record what was done in {}", path.display())
      },
    }
  }

  /// Perform this phase.
  ///
  /// `Ok(None)` means the phase was a no-op that must not appear in the
  /// receipt: the only case is [`Action::InstallNix`] on a host that
  /// already has Nix, which the spec requires to be silent.
  ///
  /// # Errors
  ///
  /// Returns the phase's failure. A phase that fails records nothing,
  /// so the receipt never claims an effect that did not happen.
  pub fn apply(
    &self,
    runner: &dyn Runner,
    facts: &HostFacts,
  ) -> Result<Option<Reversal>, BootstrapError> {
    match self {
      Self::CheckPreflight(opts) => {
        check_preflight(facts, opts)?;
        Ok(Some(Reversal::nothing("preflight only read the host")))
      },

      Self::InstallNix { backend } => install_nix(*backend, runner, facts),

      Self::CloneSources {
        remote,
        branch,
        dest,
      } => {
        let out = runner.run_streaming(&[
          "git".to_owned(),
          "clone".to_owned(),
          "--branch".to_owned(),
          branch.clone(),
          "--".to_owned(),
          remote.clone(),
          dest.display().to_string(),
        ])?;
        if !out.success() {
          return Err(BootstrapError::PhaseFailed {
            action:  ActionKind::CloneSources,
            command: format!("git clone {remote}"),
            code:    out.code,
          });
        }
        Ok(Some(Reversal::RemovePath { path: dest.clone() }))
      },

      // Owned by xi-sources-cache-prestage. Recorded as pending rather
      // than silently skipped: the first build on this host did not get
      // the operator's substituters, and the receipt should say so.
      Self::CachePrestage { sources_dir } => {
        warn!(
          "cache prestage is owned by xi-sources-cache-prestage and is not \
           implemented yet; the first build will use whatever substituters {} \
           already has",
          sources_dir.display()
        );
        Ok(Some(Reversal::pending(
          "xi-sources-cache-prestage",
          "restore the substituter configuration",
        )))
      },

      Self::FirstSwitch {
        backend,
        sources_dir,
        host,
      } => {
        let command = backend.argv(sources_dir, host.as_deref());
        let out = runner.run_streaming(&command)?;
        if !out.success() {
          return Err(BootstrapError::PhaseFailed {
            action:  ActionKind::FirstSwitch,
            command: command.join(" "),
            code:    out.code,
          });
        }
        // Undoing an activation is a generation rollback, which is
        // `xi <scope> rollback` and not something `uninstall` should
        // replay from a receipt: the generation it would roll back to
        // may not be the one that was current at bootstrap time.
        Ok(Some(Reversal::nothing(
          "activation is undone by a generation rollback, not by uninstall",
        )))
      },

      // Owned by xi-sources-gitops.
      Self::EnableGitops { scope } => {
        warn!(
          "enabling the gitops daemon is owned by xi-sources-gitops and is \
           not implemented yet; the {scope} scope will not self-update"
        );
        Ok(Some(Reversal::pending(
          "xi-sources-gitops",
          "disable the gitops daemon",
        )))
      },

      Self::WriteReceipt { path } => {
        Ok(Some(Reversal::RemovePath { path: path.clone() }))
      },
    }
  }
}

/// Phase 2: delegate the Nix install.
///
/// Three outcomes, per spec: Nix already present is a silent no-op that
/// records nothing; `installer = "none"` on a host without Nix halts
/// naming the key; anything else shells out to the backend's own
/// documented one-liner.
fn install_nix(
  backend: Installer,
  runner: &dyn Runner,
  facts: &HostFacts,
) -> Result<Option<Reversal>, BootstrapError> {
  if facts.nix_present {
    debug!("nix is already present; the installer phase is a no-op");
    return Ok(None);
  }

  if backend == Installer::None {
    return Err(BootstrapError::NixStillAbsent(installer_none_error()));
  }

  info!("installing Nix with the {backend} installer");
  let out = runner.run_shell(installer_script(backend))?;
  if !out.success() {
    return Err(BootstrapError::InstallerFailed {
      backend,
      code: out.code,
    });
  }

  // The installer claiming success while leaving no `nix` on PATH is
  // rare but not impossible (a half-applied APFS volume, a daemon that
  // failed to start). Checking is cheap and the alternative is a
  // confusing failure three phases later.
  if !runner.has_program("nix") {
    return Err(BootstrapError::NixStillAbsent(format!(
      "the {backend} installer exited 0 but `nix` is still not on PATH.\nOpen \
       a new shell (the installer edits your profile) and re-run, or check \
       its output above."
    )));
  }

  Ok(Some(nix_uninstall_argv(backend).map_or_else(
    || Reversal::nothing("nothing was installed"),
    |argv| Reversal::Run { argv },
  )))
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

/// An ordered, previewable, reversible bootstrap.
#[derive(Debug, Clone)]
pub struct BootstrapPlan {
  /// Which half of the machine this plan bootstraps.
  pub scope:          Scope,
  /// Where the receipt is written.
  pub receipt_path:   PathBuf,
  /// `ScopePaths::state_dir` for this scope - where the sources lock
  /// lives. Not derivable from `receipt_path`: at system scope the
  /// receipt sits in the state dir's *parent* (see
  /// [`receipt_dir`]), so the two have to be named separately.
  pub lock_state_dir: PathBuf,
  /// The config repo being adopted, recorded in the receipt.
  pub remote:         Option<String>,
  /// The phases, in execution order.
  pub actions:        Vec<Action>,
}

impl BootstrapPlan {
  /// An empty plan for `scope`.
  #[must_use]
  pub const fn new(
    scope: Scope,
    receipt_path: PathBuf,
    lock_state_dir: PathBuf,
  ) -> Self {
    Self {
      scope,
      receipt_path,
      lock_state_dir,
      remote: None,
      actions: Vec::new(),
    }
  }

  /// Name the config repo being adopted.
  #[must_use]
  pub fn for_remote(mut self, remote: Option<String>) -> Self {
    self.remote = remote;
    self
  }

  /// Append a phase.
  #[must_use]
  pub fn with(mut self, action: Action) -> Self {
    self.actions.push(action);
    self
  }

  /// The plan as the operator sees it before confirming.
  #[must_use]
  pub fn preview(&self) -> String {
    let body = self
      .actions
      .iter()
      .enumerate()
      .map(|(i, action)| format!("  {}. {}", i + 1, action.describe()))
      .collect::<Vec<_>>()
      .join("\n");
    format!(
      "xi will bootstrap the {} scope of this host:\n{body}\n\nEvery step is \
       recorded in {} so `xi system uninstall` can reverse it.",
      self.scope,
      self.receipt_path.display()
    )
  }

  /// Print the plan and ask, unless prompting is suppressed.
  ///
  /// # Errors
  ///
  /// Returns [`BootstrapError::Declined`] when the operator says no, and
  /// the same when there is no terminal to ask on - a bootstrap that
  /// cannot ask must not assume yes. [`NO_CONFIRM_ENV`] is the way
  /// through for scripted runs.
  pub fn confirm(&self, no_confirm: bool) -> Result<(), BootstrapError> {
    if no_confirm || no_confirm_from_env() {
      debug!("plan confirmation suppressed");
      return Ok(());
    }

    eprintln!("{}", self.preview());
    let answer = inquire::Confirm::new("Proceed?")
      .with_default(false)
      .prompt();

    match answer {
      Ok(true) => Ok(()),
      Ok(false) => Err(BootstrapError::Declined),
      Err(e) => {
        warn!(
          "cannot prompt for confirmation ({e}); set {NO_CONFIRM_ENV}=1 to \
           run unattended"
        );
        Err(BootstrapError::Declined)
      },
    }
  }

  /// Execute every phase in order, writing the receipt as it goes.
  ///
  /// The receipt is saved after each applied phase, not at the end, so a
  /// crash or a power loss halfway through still leaves something
  /// `xi system uninstall` can reverse.
  ///
  /// Takes the sources lock (`ActorName::Bootstrap`) before the first
  /// phase and holds it for every phase, not just `CloneSources`: a
  /// concurrent daemon tick or switch must not race the machine's own
  /// first-run adoption. Released by `Drop` once this returns, success or
  /// failure alike.
  ///
  /// # Errors
  ///
  /// Returns [`BootstrapError::Lock`] when the sources lock is held by
  /// another actor or `XI_SOURCES_LOCK_WAIT` is unparsable, and otherwise
  /// the first phase failure. The receipt on disk is accurate about what
  /// ran: a failing phase contributes no entry.
  pub fn execute(
    &self,
    runner: &dyn Runner,
    facts: &HostFacts,
  ) -> Result<Receipt, BootstrapError> {
    let intent = format!("xi {} bootstrap", self.scope.as_str());
    let opts = LockOptions::from_env(ActorName::Bootstrap, intent)
      .map_err(BootstrapError::Lock)?;
    let _lock = SourcesLock::acquire(&self.lock_state_dir, &opts)
      .map_err(|err| BootstrapError::Lock(err.to_string()))?;

    let mut receipt = Receipt::new(self.scope).for_remote(self.remote.clone());

    for action in &self.actions {
      let kind = action.kind();
      debug!("phase {kind}: {}", action.describe());

      match action.apply(runner, facts) {
        Ok(None) => {
          info!("phase {kind} was a no-op");
        },
        Ok(Some(reversal)) => {
          let mut entry = ReceiptEntry::now(kind, reversal);
          if let Action::InstallNix { backend } = action {
            entry = entry.installed_by(*backend);
          }
          receipt.entries.push(entry);
          receipt.save(&self.receipt_path)?;
        },
        Err(e) => {
          // Persist what did happen before surfacing the failure, so
          // the operator can reverse a partial bootstrap.
          if !receipt.entries.is_empty() {
            receipt.save(&self.receipt_path)?;
          }
          return Err(e);
        },
      }
    }

    Ok(receipt)
  }
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

/// Reverse a recorded bootstrap.
///
/// Entries are reversed last-applied-first, with one exception: the
/// `write-receipt` entry is held back and the receipt file is deleted at
/// the very end. Reversing it in strict order would delete the receipt
/// before the entries it describes had been reversed, which is exactly
/// the state a crash must not be able to produce.
///
/// # Errors
///
/// Returns [`BootstrapError::NoReceipt`] when there is nothing to
/// reverse, and the first reversal failure otherwise.
pub fn uninstall(
  receipt_path: &Path,
  runner: &dyn Runner,
) -> Result<Vec<ActionKind>, BootstrapError> {
  let Some(receipt) = Receipt::load(receipt_path)? else {
    return Err(BootstrapError::NoReceipt {
      path: receipt_path.to_path_buf(),
    });
  };

  let mut reversed = Vec::new();
  for entry in receipt.entries.iter().rev() {
    if entry.action == ActionKind::WriteReceipt {
      continue;
    }
    info!("reversing {}", entry.action);
    entry.reversal.execute(runner)?;
    reversed.push(entry.action);
  }

  // The receipt goes last, and only once everything it describes is
  // undone.
  if receipt_path.exists() {
    fs::remove_file(receipt_path)?;
    reversed.push(ActionKind::WriteReceipt);
  }

  Ok(reversed)
}

// ---------------------------------------------------------------------------
// Remote SSH form
// ---------------------------------------------------------------------------

/// Where the xi binary is staged on the remote host.
pub const REMOTE_BINARY: &str = "./xi-bootstrap";

/// A `ssh://[user@]host[:port]` bootstrap target.
///
/// Deliberately *not* a config-repo remote: those always carry a path,
/// and that difference is what tells the two apart in the same argument
/// position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
  /// Login user, when the operator named one.
  pub user: Option<String>,
  /// Hostname or address.
  pub host: String,
  /// Non-default port, when the operator named one.
  pub port: Option<u16>,
}

impl SshTarget {
  /// Parse a bare `ssh://[user@]host[:port]`.
  ///
  /// Returns `None` for anything else, including an `ssh://` URL with a
  /// path - that is a config repo, and reading it as a host would
  /// `scp` a binary to a machine the operator never named.
  #[must_use]
  pub fn parse(raw: &str) -> Option<Self> {
    let rest = raw.trim().strip_prefix("ssh://")?;
    if rest.is_empty() || rest.contains('/') {
      return None;
    }

    let (user, authority) = match rest.split_once('@') {
      Some((user, authority)) if !user.is_empty() => {
        (Some(user.to_owned()), authority)
      },
      Some(_) => return None,
      None => (None, rest),
    };

    let (host, port) = match authority.rsplit_once(':') {
      Some((host, port)) => (host, Some(port.parse::<u16>().ok()?)),
      None => (authority, None),
    };
    if host.is_empty() {
      return None;
    }

    Some(Self {
      user,
      host: host.to_owned(),
      port,
    })
  }

  /// The `[user@]host` string ssh and scp both take.
  #[must_use]
  pub fn destination(&self) -> String {
    self
      .user
      .as_ref()
      .map_or_else(|| self.host.clone(), |u| format!("{u}@{}", self.host))
  }
}

/// Bootstrap another host over SSH, running the same planner there.
///
/// The transport is three commands and no magic: copy this binary over,
/// run it with the operator's arguments, remove it. Preflight runs on
/// the far side, which is the only place that can authoritatively
/// report that host's OS, `SELinux` state, and WSL flavour.
///
/// # Errors
///
/// Returns [`BootstrapError::PhaseFailed`] for a non-zero `scp` or
/// `ssh`. The cleanup step is best-effort: a leftover binary is worth a
/// warning, not a failed bootstrap.
pub fn bootstrap_over_ssh(
  target: &SshTarget,
  remote_url: &str,
  local_binary: &Path,
  verb: &str,
  runner: &dyn Runner,
) -> Result<(), BootstrapError> {
  let dest = target.destination();

  let mut scp = vec!["scp".to_owned()];
  if let Some(port) = target.port {
    // scp spells the port -P; ssh spells it -p. Getting this backwards
    // silently copies to the wrong place.
    scp.push("-P".to_owned());
    scp.push(port.to_string());
  }
  scp.push(local_binary.display().to_string());
  scp.push(format!("{dest}:{REMOTE_BINARY}"));

  info!("copying xi to {dest}");
  let out = runner.run_streaming(&scp)?;
  if !out.success() {
    return Err(BootstrapError::PhaseFailed {
      action:  ActionKind::CheckPreflight,
      command: scp.join(" "),
      code:    out.code,
    });
  }

  let mut ssh = vec!["ssh".to_owned()];
  if let Some(port) = target.port {
    ssh.push("-p".to_owned());
    ssh.push(port.to_string());
  }
  ssh.push(dest.clone());
  ssh.push(format!("{REMOTE_BINARY} {verb} switch {remote_url}"));

  info!("bootstrapping {dest} from {remote_url}");
  let run = runner.run_streaming(&ssh)?;

  // Clean up whether or not the bootstrap succeeded: leaving a binary
  // in the login dir of a host that failed to bootstrap is worse than
  // leaving nothing.
  let mut cleanup = vec!["ssh".to_owned()];
  if let Some(port) = target.port {
    cleanup.push("-p".to_owned());
    cleanup.push(port.to_string());
  }
  cleanup.push(dest.clone());
  cleanup.push(format!("rm -f {REMOTE_BINARY}"));
  if let Ok(out) = runner.run(&cleanup)
    && !out.success()
  {
    warn!("could not remove {REMOTE_BINARY} from {dest}");
  }

  if !run.success() {
    return Err(BootstrapError::PhaseFailed {
      action:  ActionKind::FirstSwitch,
      command: ssh.join(" "),
      code:    run.code,
    });
  }

  Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(
  clippy::expect_used,
  clippy::panic,
  clippy::unwrap_used,
  reason = "Fine in tests"
)]
mod tests {
  use tempfile::TempDir;

  use super::{super::lock, *};

  fn facts() -> HostFacts {
    HostFacts {
      os:                 "linux".to_owned(),
      nix_present:        true,
      receipt:            None,
      passwordless_sudo:  true,
      selinux:            Selinux::Absent,
      wsl:                Wsl::No,
      declares_lix_cache: false,
    }
  }

  // -- 4.3 platform classifiers -------------------------------------------

  #[test]
  fn selinux_classifier_covers_its_three_states() {
    assert_eq!(classify_selinux(None), Selinux::Absent);
    assert_eq!(classify_selinux(Some("1\n")), Selinux::Enforcing);
    assert_eq!(classify_selinux(Some("0")), Selinux::Permissive);
  }

  #[test]
  fn wsl2_is_told_apart_from_wsl1() {
    assert_eq!(classify_wsl(None), Wsl::No);
    assert_eq!(classify_wsl(Some("6.1.0-generic")), Wsl::No);
    assert_eq!(classify_wsl(Some("4.4.0-19041-Microsoft")), Wsl::One);
    assert_eq!(
      classify_wsl(Some("5.15.90.1-microsoft-standard-WSL2")),
      Wsl::Two
    );
  }

  // -- 4.1 / 4.2 preflight ------------------------------------------------

  #[test]
  fn an_existing_receipt_is_refused_without_force() {
    let mut f = facts();
    f.receipt = Some(PathBuf::from("/var/lib/xi/bootstrap-receipt.json"));

    let err = check_preflight(&f, &PreflightOpts::default())
      .expect_err("a managed host must be refused");
    assert!(matches!(err, PreflightError::AlreadyBootstrapped { .. }));
    assert!(err.to_string().contains("--force"), "got: {err}");

    check_preflight(&f, &PreflightOpts {
      force: true,
      ..PreflightOpts::default()
    })
    .expect("--force must get past the receipt");
  }

  #[test]
  fn missing_sudo_is_refused_unless_interactive() {
    let mut f = facts();
    f.passwordless_sudo = false;

    assert_eq!(
      check_preflight(&f, &PreflightOpts::default()),
      Err(PreflightError::NoPasswordlessSudo)
    );
    check_preflight(&f, &PreflightOpts {
      interactive_sudo: true,
      ..PreflightOpts::default()
    })
    .expect("--interactive-sudo must get past it");
  }

  #[test]
  fn enforcing_selinux_is_refused_unless_allowed() {
    let mut f = facts();
    f.selinux = Selinux::Enforcing;

    assert_eq!(
      check_preflight(&f, &PreflightOpts::default()),
      Err(PreflightError::SelinuxEnforcing)
    );
    check_preflight(&f, &PreflightOpts {
      allow_selinux: true,
      ..PreflightOpts::default()
    })
    .expect("--allow-selinux must get past it");
  }

  #[test]
  fn permissive_selinux_is_fine() {
    let mut f = facts();
    f.selinux = Selinux::Permissive;
    check_preflight(&f, &PreflightOpts::default()).expect("permissive is ok");
  }

  #[test]
  fn wsl1_is_refused_and_wsl2_is_not() {
    let mut f = facts();
    f.wsl = Wsl::One;
    assert_eq!(
      check_preflight(&f, &PreflightOpts::default()),
      Err(PreflightError::Wsl1)
    );

    // No flag gets past WSL1: there is no "try anyway" that works.
    assert!(
      check_preflight(&f, &PreflightOpts {
        force:            true,
        interactive_sudo: true,
        allow_selinux:    true,
        expect_os:        None,
      })
      .is_err()
    );

    f.wsl = Wsl::Two;
    check_preflight(&f, &PreflightOpts::default()).expect("WSL2 is supported");
  }

  #[test]
  fn an_os_mismatch_names_both_sides() {
    let f = facts();
    let err = check_preflight(&f, &PreflightOpts {
      expect_os: Some("darwin"),
      ..PreflightOpts::default()
    })
    .expect_err("linux host, darwin verb");
    assert_eq!(err, PreflightError::OsMismatch {
      expected: "darwin".to_owned(),
      actual:   "linux".to_owned(),
    });
    assert!(err.to_string().contains("linux"));
    assert!(err.to_string().contains("darwin"));
  }

  // -- 5.x installer selection -------------------------------------------

  #[test]
  fn the_default_backend_follows_the_declared_cache() {
    assert_eq!(default_installer(false), Installer::Detsys);
    assert_eq!(default_installer(true), Installer::Lix);
  }

  #[test]
  fn an_explicit_backend_wins_over_the_heuristic() {
    assert_eq!(
      resolve_installer(Some(Installer::Detsys), true),
      Installer::Detsys
    );
    assert_eq!(
      resolve_installer(Some(Installer::None), true),
      Installer::None
    );
  }

  #[test]
  fn the_lix_cache_is_found_in_a_nix_file() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("hosts/laptop")).unwrap();
    assert!(!declares_lix_cache(tmp.path()));

    fs::write(
      tmp.path().join("hosts/laptop/nix.nix"),
      "{ nix.settings.substituters = [ \"https://cache.lix.systems\" ]; }\n",
    )
    .unwrap();
    assert!(declares_lix_cache(tmp.path()));
  }

  #[test]
  fn the_scan_ignores_non_nix_files_and_missing_dirs() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("README.md"), LIX_SUBSTITUTER).unwrap();
    assert!(!declares_lix_cache(tmp.path()));
    assert!(!declares_lix_cache(&tmp.path().join("nope")));
  }

  #[test]
  fn both_installer_commands_are_the_documented_one_liners() {
    assert!(
      installer_script(Installer::Detsys)
        .contains("install.determinate.systems/nix")
    );
    assert!(
      installer_script(Installer::Detsys).contains("--determinate=false")
    );
    assert!(installer_script(Installer::Lix).contains("install.lix.systems"));
    for backend in [Installer::Detsys, Installer::Lix] {
      assert!(installer_script(backend).contains("--no-confirm"));
    }
  }

  #[test]
  fn installer_none_names_every_accepted_value() {
    let msg = installer_none_error();
    for value in Installer::ALL {
      assert!(msg.contains(value), "{value} missing from: {msg}");
    }
    assert!(msg.contains("installer"));
  }

  // -- 5.1 / 5.3 / 5.4 / 5.5 / 5.6 install delegation --------------------

  #[test]
  fn nix_present_skips_both_backends() {
    for backend in [Installer::Detsys, Installer::Lix, Installer::None] {
      let runner = RecordingRunner::new().with_program("nix");
      let applied = install_nix(backend, &runner, &facts())
        .expect("a present nix is never an error");
      assert!(applied.is_none(), "{backend} must record nothing");
      assert!(runner.calls().is_empty(), "{backend} must run nothing");
    }
  }

  #[test]
  fn installer_none_halts_when_nix_is_absent() {
    let mut f = facts();
    f.nix_present = false;
    let runner = RecordingRunner::new();

    let err = install_nix(Installer::None, &runner, &f)
      .expect_err("none plus no nix must halt");
    assert!(matches!(err, BootstrapError::NixStillAbsent(..)));
    assert!(runner.calls().is_empty());
  }

  #[test]
  fn detsys_shells_out_and_records_the_reversal() {
    let mut f = facts();
    f.nix_present = false;
    let runner = RecordingRunner::new().with_program("nix");

    let reversal = install_nix(Installer::Detsys, &runner, &f)
      .expect("install must succeed")
      .expect("an install must be recorded");

    assert!(runner.ran("install.determinate.systems/nix"));
    assert_eq!(reversal, Reversal::Run {
      argv: argv(["/nix/nix-installer", "uninstall", "--no-confirm"]),
    });
  }

  #[test]
  fn lix_shells_out_to_the_lix_installer() {
    let mut f = facts();
    f.nix_present = false;
    let runner = RecordingRunner::new().with_program("nix");

    install_nix(Installer::Lix, &runner, &f).expect("install must succeed");
    assert!(runner.ran("install.lix.systems"));
    assert!(!runner.ran("determinate.systems"));
  }

  #[test]
  fn a_failing_installer_is_an_error_naming_the_backend() {
    let mut f = facts();
    f.nix_present = false;
    let runner = RecordingRunner::new()
      .with_program("nix")
      .failing("install.lix.systems", 3);

    let err = install_nix(Installer::Lix, &runner, &f)
      .expect_err("a non-zero installer must fail the phase");
    match err {
      BootstrapError::InstallerFailed { backend, code } => {
        assert_eq!(backend, Installer::Lix);
        assert_eq!(code, 3);
      },
      other => panic!("expected InstallerFailed, got {other:?}"),
    }
  }

  #[test]
  fn an_installer_that_leaves_no_nix_is_still_a_failure() {
    let mut f = facts();
    f.nix_present = false;
    // No `nix` registered: the shell-out "succeeds" but PATH is empty.
    let runner = RecordingRunner::new();

    let err = install_nix(Installer::Detsys, &runner, &f)
      .expect_err("exit 0 without nix must not pass");
    assert!(matches!(err, BootstrapError::NixStillAbsent(..)));
  }

  // -- 3.3 receipt paths --------------------------------------------------

  #[test]
  fn the_system_receipt_sits_above_the_state_dir() {
    let paths = ScopePaths {
      sources_dir: PathBuf::from("/var/lib/xi/sources"),
      state_dir:   PathBuf::from("/var/lib/xi/state"),
      config_file: PathBuf::from("/etc/xi/sources.toml"),
    };
    assert_eq!(
      receipt_path(Scope::System, &paths),
      PathBuf::from("/var/lib/xi/bootstrap-receipt.json")
    );
  }

  #[test]
  fn the_user_receipt_sits_in_the_state_dir() {
    let paths = ScopePaths {
      sources_dir: PathBuf::from("/home/u/.local/share/xi/sources"),
      state_dir:   PathBuf::from("/home/u/.local/state/xi"),
      config_file: PathBuf::from("/home/u/.config/xi/sources.toml"),
    };
    assert_eq!(
      receipt_path(Scope::User, &paths),
      PathBuf::from("/home/u/.local/state/xi/bootstrap-receipt.json")
    );
  }

  // -- 3.3 / 8.3 receipt round trip --------------------------------------

  #[test]
  fn a_receipt_round_trips_through_json() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(RECEIPT_FILE);

    let mut receipt = Receipt::new(Scope::System)
      .for_remote(Some("https://git.example/cfg".to_owned()));
    receipt.entries.push(
      ReceiptEntry::now(ActionKind::InstallNix, Reversal::Run {
        argv: argv(["/nix/nix-installer", "uninstall"]),
      })
      .installed_by(Installer::Lix),
    );
    receipt.entries.push(ReceiptEntry::now(
      ActionKind::CloneSources,
      Reversal::RemovePath {
        path: PathBuf::from("/var/lib/xi/sources"),
      },
    ));

    receipt.save(&path).expect("save");
    let loaded = Receipt::load(&path).expect("load").expect("present");

    assert_eq!(loaded, receipt);
    assert_eq!(loaded.nix_installed_by(), Some(Installer::Lix));
    assert_eq!(loaded.schema_version, RECEIPT_SCHEMA_VERSION);
  }

  #[test]
  fn an_absent_receipt_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    assert!(
      Receipt::load(&tmp.path().join(RECEIPT_FILE))
        .expect("absent is fine")
        .is_none()
    );
  }

  #[test]
  fn a_malformed_receipt_is_an_error_naming_the_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(RECEIPT_FILE);
    fs::write(&path, "{ not json").unwrap();

    let err = Receipt::load(&path).expect_err("malformed must not be ignored");
    assert!(matches!(err, BootstrapError::BadReceipt { .. }));
    assert!(err.to_string().contains(RECEIPT_FILE));
  }

  #[cfg(unix)]
  #[test]
  fn a_saved_receipt_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("deep").join(RECEIPT_FILE);
    Receipt::new(Scope::User).save(&path).expect("save");

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
  }

  // -- 6.1 ssh target parsing --------------------------------------------

  #[test]
  fn an_ssh_host_is_parsed_with_and_without_user_and_port() {
    assert_eq!(
      SshTarget::parse("ssh://example.net"),
      Some(SshTarget {
        user: None,
        host: "example.net".to_owned(),
        port: None,
      })
    );
    assert_eq!(
      SshTarget::parse("ssh://admin@example.net"),
      Some(SshTarget {
        user: Some("admin".to_owned()),
        host: "example.net".to_owned(),
        port: None,
      })
    );
    assert_eq!(
      SshTarget::parse("ssh://admin@example.net:2222"),
      Some(SshTarget {
        user: Some("admin".to_owned()),
        host: "example.net".to_owned(),
        port: Some(2222),
      })
    );
  }

  /// An `ssh://` URL *with* a path is a config repo, and reading it as a
  /// host would `scp` a binary to a machine nobody named.
  #[test]
  fn an_ssh_config_repo_url_is_not_a_bootstrap_target() {
    assert_eq!(SshTarget::parse("ssh://git.example/dotfiles"), None);
    assert_eq!(SshTarget::parse("ssh://git@git.example/cfg.git"), None);
    assert_eq!(SshTarget::parse("https://git.example/cfg"), None);
    assert_eq!(SshTarget::parse("ssh://"), None);
    assert_eq!(SshTarget::parse("ssh://@host"), None);
  }

  #[test]
  fn the_ssh_destination_omits_an_absent_user() {
    assert_eq!(
      SshTarget::parse("ssh://example.net").unwrap().destination(),
      "example.net"
    );
    assert_eq!(
      SshTarget::parse("ssh://admin@example.net")
        .unwrap()
        .destination(),
      "admin@example.net"
    );
  }

  // -- 6.2 ssh transport --------------------------------------------------

  #[test]
  fn remote_bootstrap_copies_runs_and_cleans_up() {
    let runner = RecordingRunner::new();
    let target = SshTarget::parse("ssh://admin@example.net:2222").unwrap();

    bootstrap_over_ssh(
      &target,
      "https://git.example/dotfiles",
      Path::new("/usr/bin/xi"),
      "system",
      &runner,
    )
    .expect("the transport must succeed");

    let calls = runner.calls();
    assert_eq!(calls.len(), 3, "scp, ssh, cleanup: {calls:?}");
    assert!(calls[0].starts_with("scp -P 2222 /usr/bin/xi "));
    assert!(calls[0].contains("admin@example.net:"));
    assert!(calls[1].contains("ssh -p 2222 admin@example.net"));
    assert!(calls[1].contains("system switch https://git.example/dotfiles"));
    assert!(calls[2].contains("rm -f"));
  }

  #[test]
  fn a_failed_remote_switch_still_cleans_up_the_copied_binary() {
    let runner = RecordingRunner::new().failing("system switch", 7);
    let target = SshTarget::parse("ssh://example.net").unwrap();

    let err = bootstrap_over_ssh(
      &target,
      "https://git.example/cfg",
      Path::new("/usr/bin/xi"),
      "system",
      &runner,
    )
    .expect_err("a failing remote switch must surface");
    assert!(matches!(err, BootstrapError::PhaseFailed { code: 7, .. }));
    assert!(
      runner.ran("rm -f"),
      "cleanup must still run: {:?}",
      runner.calls()
    );
  }

  #[test]
  fn a_failed_scp_does_not_run_anything_else() {
    let runner = RecordingRunner::new().failing("scp", 1);
    let target = SshTarget::parse("ssh://example.net").unwrap();

    bootstrap_over_ssh(
      &target,
      "https://git.example/cfg",
      Path::new("/usr/bin/xi"),
      "system",
      &runner,
    )
    .expect_err("a failing scp must stop the transport");
    assert_eq!(runner.calls().len(), 1);
  }

  // -- 3.1 / 3.2 the plan -------------------------------------------------

  fn full_plan(tmp: &TempDir) -> BootstrapPlan {
    let sources = tmp.path().join("sources");
    let plan = BootstrapPlan::new(
      Scope::System,
      tmp.path().join(RECEIPT_FILE),
      tmp.path().join("state"),
    );
    plan
      .for_remote(Some("https://git.example/cfg".to_owned()))
      .with(Action::CheckPreflight(PreflightOpts::default()))
      .with(Action::InstallNix {
        backend: Installer::Detsys,
      })
      .with(Action::CloneSources {
        remote: "https://git.example/cfg".to_owned(),
        branch: "main".to_owned(),
        dest:   sources.clone(),
      })
      .with(Action::CachePrestage {
        sources_dir: sources.clone(),
      })
      .with(Action::FirstSwitch {
        backend:     SwitchBackend::NixosRebuild,
        sources_dir: sources,
        host:        Some("laptop".to_owned()),
      })
      .with(Action::EnableGitops {
        scope: Scope::System,
      })
      .with(Action::WriteReceipt {
        path: tmp.path().join(RECEIPT_FILE),
      })
  }

  #[test]
  fn the_preview_lists_every_phase_in_order() {
    let tmp = TempDir::new().unwrap();
    let preview = full_plan(&tmp).preview();

    for fragment in [
      "check this host",
      "install Nix with the detsys installer",
      "clone https://git.example/cfg",
      "configure substituters",
      "nixos-rebuild switch --flake",
      "enable the gitops daemon",
      "record what was done",
    ] {
      assert!(preview.contains(fragment), "{fragment} missing: {preview}");
    }
    assert!(preview.contains("uninstall"), "got: {preview}");
  }

  #[test]
  fn the_phases_execute_in_the_spec_order() {
    let tmp = TempDir::new().unwrap();
    let runner = RecordingRunner::new().with_program("nix");

    let receipt = full_plan(&tmp)
      .execute(&runner, &facts())
      .expect("the plan must run");

    // InstallNix is absent: `nix` was already present, which the spec
    // requires to be a silent no-op recording nothing.
    assert_eq!(receipt.applied_kinds(), vec![
      ActionKind::CheckPreflight,
      ActionKind::CloneSources,
      ActionKind::CachePrestage,
      ActionKind::FirstSwitch,
      ActionKind::EnableGitops,
      ActionKind::WriteReceipt,
    ]);
    assert_eq!(receipt.remote.as_deref(), Some("https://git.example/cfg"));
  }

  /// xi-wtrq: `execute` takes the sources lock (`ActorName::Bootstrap`)
  /// before the first phase and releases it once it returns, so a
  /// concurrent daemon tick or switch cannot race first-run adoption.
  #[test]
  fn execute_takes_the_sources_lock_and_releases_it() {
    let tmp = TempDir::new().unwrap();
    let runner = RecordingRunner::new().with_program("nix");

    full_plan(&tmp)
      .execute(&runner, &facts())
      .expect("the plan must run");

    let lock_path = tmp.path().join("state").join(lock::LOCK_FILE);
    assert!(
      lock_path.is_file(),
      "execute must have taken the lock, which creates {}",
      lock_path.display()
    );
    assert_eq!(
      fs::read_to_string(&lock_path).unwrap().trim(),
      "",
      "a released lock leaves an empty record"
    );
  }

  /// A plan cannot run while another actor holds the sources lock: the
  /// error names the holder rather than racing it.
  ///
  /// `#[serial]` plus `EnvGuard`, the house pattern
  /// (`crate::installable::tests`) for a test that has to mutate
  /// `XI_SOURCES_LOCK_WAIT`: `execute` has no `wait_for` of its own to
  /// shorten, only the env override `LockOptions::from_env` reads, and
  /// mutating process env is not safe across parallel tests.
  #[test]
  #[serial_test::serial]
  fn execute_refuses_a_held_lock_and_names_the_holder() {
    let _env = crate::test_utils::EnvGuard::new(lock::LOCK_WAIT_ENV, "50ms");
    let tmp = TempDir::new().unwrap();
    let state_dir = tmp.path().join("state");
    let runner = RecordingRunner::new().with_program("nix");

    let held = SourcesLock::acquire(
      &state_dir,
      &LockOptions::new(ActorName::Daemon, "tick").without_refresh(),
    )
    .expect("the test takes the free lock first");

    let err = full_plan(&tmp)
      .execute(&runner, &facts())
      .expect_err("a held lock must refuse the plan");
    let BootstrapError::Lock(message) = &err else {
      panic!("expected BootstrapError::Lock, got {err:?}");
    };
    assert!(message.contains("daemon"), "got: {message}");
    assert!(
      !runner.ran("git clone"),
      "a plan refused on the lock must not have run any phase"
    );

    held.release().expect("release");
  }

  /// Scenario: Nix install fails, bootstrap halts.
  #[test]
  fn a_failing_install_stops_the_plan_and_records_only_preflight() {
    let tmp = TempDir::new().unwrap();
    let runner = RecordingRunner::new().failing("determinate.systems", 1);
    let mut f = facts();
    f.nix_present = false;

    let err = full_plan(&tmp)
      .execute(&runner, &f)
      .expect_err("a failing installer must stop the plan");
    assert!(matches!(err, BootstrapError::InstallerFailed { .. }));

    let receipt = Receipt::load(&tmp.path().join(RECEIPT_FILE))
      .expect("load")
      .expect("the partial receipt must exist");
    assert_eq!(receipt.applied_kinds(), vec![ActionKind::CheckPreflight]);
    assert!(!runner.ran("git clone"), "phase 3 must not have run");
  }

  /// Scenario: Sources clone fails, Nix install reversal available.
  #[test]
  fn a_failing_clone_leaves_the_nix_install_reversible() {
    let tmp = TempDir::new().unwrap();
    let runner = RecordingRunner::new()
      .with_program("nix")
      .failing("git clone", 128);
    let mut f = facts();
    f.nix_present = false;

    full_plan(&tmp)
      .execute(&runner, &f)
      .expect_err("a failing clone must stop the plan");

    let receipt = Receipt::load(&tmp.path().join(RECEIPT_FILE))
      .expect("load")
      .expect("partial receipt");
    assert_eq!(receipt.applied_kinds(), vec![
      ActionKind::CheckPreflight,
      ActionKind::InstallNix,
    ]);
    assert_eq!(receipt.nix_installed_by(), Some(Installer::Detsys));

    let reversed = uninstall(&tmp.path().join(RECEIPT_FILE), &runner)
      .expect("the partial bootstrap must be reversible");
    assert!(reversed.contains(&ActionKind::InstallNix));
    assert!(runner.ran("/nix/nix-installer uninstall"));
  }

  // -- 7.x / 8.3 uninstall ------------------------------------------------

  /// Scenario: Full round trip is reversible.
  #[test]
  fn a_full_bootstrap_reverses_to_the_state_it_started_from() {
    let tmp = TempDir::new().unwrap();
    let receipt_path = tmp.path().join(RECEIPT_FILE);
    let sources = tmp.path().join("sources");
    let runner = RecordingRunner::new().with_program("nix");

    // Stand in for the clone the real phase 3 would have made.
    fs::create_dir_all(sources.join(".git")).unwrap();

    let plan = BootstrapPlan::new(
      Scope::System,
      receipt_path.clone(),
      tmp.path().join("state"),
    )
    .with(Action::CheckPreflight(PreflightOpts::default()))
    .with(Action::CloneSources {
      remote: "https://git.example/cfg".to_owned(),
      branch: "main".to_owned(),
      dest:   sources.clone(),
    })
    .with(Action::WriteReceipt {
      path: receipt_path.clone(),
    });
    plan.execute(&runner, &facts()).expect("bootstrap");
    assert!(receipt_path.is_file());

    let reversed = uninstall(&receipt_path, &runner).expect("uninstall");

    assert_eq!(reversed, vec![
      ActionKind::CloneSources,
      ActionKind::CheckPreflight,
      ActionKind::WriteReceipt,
    ]);
    assert!(!sources.exists(), "the clone must be gone");
    assert!(!receipt_path.exists(), "the receipt must be gone");
  }

  #[test]
  fn uninstalling_an_unmanaged_host_says_so() {
    let tmp = TempDir::new().unwrap();
    let runner = RecordingRunner::new();
    let err = uninstall(&tmp.path().join(RECEIPT_FILE), &runner)
      .expect_err("nothing to reverse must be an error");
    assert!(matches!(err, BootstrapError::NoReceipt { .. }));
  }

  #[test]
  fn uninstall_calls_the_backend_that_installed_nix() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(RECEIPT_FILE);
    let mut receipt = Receipt::new(Scope::System);
    receipt.entries.push(
      ReceiptEntry::now(ActionKind::InstallNix, Reversal::Run {
        argv: nix_uninstall_argv(Installer::Lix).unwrap(),
      })
      .installed_by(Installer::Lix),
    );
    receipt.save(&path).unwrap();

    let runner = RecordingRunner::new();
    uninstall(&path, &runner).expect("uninstall");
    assert!(runner.ran("/nix/nix-installer uninstall --no-confirm"));
  }

  #[test]
  fn a_failing_reversal_stops_the_uninstall() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(RECEIPT_FILE);
    let mut receipt = Receipt::new(Scope::System);
    receipt.entries.push(ReceiptEntry::now(
      ActionKind::InstallNix,
      Reversal::Run {
        argv: argv(["/nix/nix-installer", "uninstall"]),
      },
    ));
    receipt.save(&path).unwrap();

    let runner = RecordingRunner::new().failing("nix-installer", 1);
    let err = uninstall(&path, &runner).expect_err("a failing reversal");
    assert!(matches!(err, BootstrapError::ReversalFailed { .. }));
    assert!(
      path.is_file(),
      "the receipt must survive a failed reversal so it can be retried"
    );
  }

  #[test]
  fn the_switch_backends_build_from_the_sources_dir() {
    let dir = Path::new("/var/lib/xi/sources");
    assert_eq!(
      SwitchBackend::NixosRebuild.argv(dir, Some("laptop")),
      argv([
        "nixos-rebuild",
        "switch",
        "--flake",
        "/var/lib/xi/sources#laptop",
      ])
    );
    assert_eq!(
      SwitchBackend::HomeManager.argv(dir, None),
      argv(["home-manager", "switch", "--flake", "/var/lib/xi/sources"])
    );
    assert!(
      SwitchBackend::DarwinRebuild
        .argv(dir, Some("mac"))
        .contains(&"darwin-rebuild".to_owned())
    );
  }
}
