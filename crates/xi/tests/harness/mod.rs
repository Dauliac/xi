//! Hermetic test world for the devshell end-to-end suite.
//!
//! Spec: `openspec/changes/devshell-e2e-harness/specs/xi-devshell-test-harness`
//!
//! One `TempDir` owns everything: `HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME`,
//! and `XI_RUNTIME_DIR` all point inside it, so the daemon socket, pid file,
//! per-flake state, A/B slots, generation counters, profile symlinks and trust
//! markers are all reclaimed when the world drops.
//!
//! Two rules shape the whole design:
//!
//! 1. **The environment is data, not ambient state.** [`TestWorld::env`]
//!    returns pairs applied per `Command`. Nothing here calls
//!    `std::env::set_var`, which is `unsafe` in edition 2024 and would race
//!    across the multithreaded test harness. That is what makes concurrent
//!    worlds safe.
//! 2. **Waiting is polling an observable, never sleeping.** See
//!    [`TestWorld::wait_for`].

#![allow(dead_code)] // each test binary uses a different subset

pub mod pty;

use std::{
  collections::HashMap,
  fs,
  io::Write as _,
  path::{Path, PathBuf},
  process::{Command, Output},
  time::{Duration, Instant},
};

/// A 32-character nix store hash. `store_path::parse` skips exactly 33 bytes
/// (hash + dash), so this length is load-bearing: one char off and the fixture
/// yields no packages.
pub const FAKE_STORE_HASH: &str = "abcdefghijklmnopqrstuvwxyz234567";

/// Default wait budget for an observable. Generous on purpose: a deadline that
/// is too tight produces flakes indistinguishable from real failures.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(20);

/// How often to re-check an observable while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

// ───────────────────────────── shells ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
  Bash,
  Zsh,
  Fish,
}

impl Shell {
  pub const ALL: [Self; 3] = [Self::Bash, Self::Zsh, Self::Fish];

  pub fn name(self) -> &'static str {
    match self {
      Self::Bash => "bash",
      Self::Zsh => "zsh",
      Self::Fish => "fish",
    }
  }

  /// Absolute path to the shell binary, or `None` when it is not installed.
  ///
  /// Tests skip rather than fail on a missing shell: a host without `fish`
  /// should still be able to run the rest of the suite.
  pub fn which(self) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
      .map(|d| d.join(self.name()))
      .find(|c| c.is_file())
  }

  /// Report a skip in the same shape across the suite, so a missing shell is
  /// visible in test output instead of silently passing.
  pub fn skip_if_missing(self) -> Option<PathBuf> {
    match self.which() {
      Some(p) => Some(p),
      None => {
        eprintln!("SKIP: {} is not installed on this host", self.name());
        None
      },
    }
  }
}

// ──────────────────────── staged nix responses ────────────────────────

/// One staged answer from the fake nix.
///
/// Expressed in the subset `DevEnv::from_nix_json` actually consumes: only
/// `type == "exported"` string entries are read, `PATH` is colon-split into
/// nix paths, a non-blank `shellHook` is retained, and `SKIP_VARS` are
/// dropped.
#[derive(Debug, Clone)]
pub struct NixResponse {
  pub vars:       Vec<(String, String)>,
  pub paths:      Vec<String>,
  pub shell_hook: Option<String>,
  /// Extra raw entries, to prove the parser ignores what it should ignore.
  pub raw_extra:  Vec<String>,
  pub exit_code:  i32,
  pub stderr:     Option<String>,
  pub delay:      Option<Duration>,
}

impl Default for NixResponse {
  fn default() -> Self {
    Self {
      vars:       vec![("XI_TEST_MARKER".into(), "one".into())],
      paths:      vec![store_path("cargo", "1.97.1")],
      shell_hook: None,
      raw_extra:  Vec::new(),
      exit_code:  0,
      stderr:     None,
      delay:      None,
    }
  }
}

impl NixResponse {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn var(mut self, k: &str, v: &str) -> Self {
    self.vars.push((k.into(), v.into()));
    self
  }

  pub fn paths(mut self, paths: &[String]) -> Self {
    self.paths = paths.to_vec();
    self
  }

  pub fn shell_hook(mut self, hook: &str) -> Self {
    self.shell_hook = Some(hook.into());
    self
  }

  /// Append a verbatim JSON member, for asserting the parser's filters.
  pub fn raw(mut self, json_member: &str) -> Self {
    self.raw_extra.push(json_member.into());
    self
  }

  /// Make this call fail the way a nix eval error does.
  pub fn failing(mut self, stderr: &str) -> Self {
    self.exit_code = 1;
    self.stderr = Some(stderr.into());
    self
  }

  /// Make this call slow, so timeout and backoff paths are reachable without
  /// waiting on real nix.
  pub fn slow(mut self, d: Duration) -> Self {
    self.delay = Some(d);
    self
  }

  fn to_json(&self) -> String {
    let mut members: Vec<String> = Vec::new();

    for (k, v) in &self.vars {
      members.push(format!(
        r#""{}":{{"type":"exported","value":{}}}"#,
        k,
        json_string(v)
      ));
    }
    if !self.paths.is_empty() {
      members.push(format!(
        r#""PATH":{{"type":"exported","value":{}}}"#,
        json_string(&self.paths.join(":"))
      ));
    }
    if let Some(hook) = &self.shell_hook {
      members.push(format!(
        r#""shellHook":{{"type":"exported","value":{}}}"#,
        json_string(hook)
      ));
    }
    members.extend(self.raw_extra.iter().cloned());

    format!(r#"{{"variables":{{{}}}}}"#, members.join(","))
  }
}

/// A plausible nix store path that `store_path::parse` turns into a package.
pub fn store_path(name: &str, version: &str) -> String {
  format!("/nix/store/{FAKE_STORE_HASH}-{name}-{version}/bin")
}

/// Minimal JSON string escaping; fixtures legitimately contain newlines,
/// quotes and backslashes (a real `shellHook` routinely does).
fn json_string(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('"');
  for c in s.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
      c => out.push(c),
    }
  }
  out.push('"');
  out
}

// ─────────────────────────── recorded calls ───────────────────────────

/// One recorded fake-nix invocation.
#[derive(Debug, Clone)]
pub struct NixCall {
  pub cwd:  PathBuf,
  pub args: Vec<String>,
}

impl NixCall {
  pub fn has_arg(&self, needle: &str) -> bool {
    self.args.iter().any(|a| a == needle)
  }

  /// Value following `flag`, e.g. `--profile <path>`.
  pub fn arg_after(&self, flag: &str) -> Option<&str> {
    let i = self.args.iter().position(|a| a == flag)?;
    self.args.get(i + 1).map(String::as_str)
  }

  /// True when `--option <name> <value>` appears as a triple.
  pub fn has_option(&self, name: &str, value: &str) -> bool {
    self
      .args
      .windows(3)
      .any(|w| w[0] == "--option" && w[1] == name && w[2] == value)
  }
}

// ──────────────────────────── flake fixtures ────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlakeFixture {
  /// A flake whose own text mentions `devShells`.
  WithDevShell,
  /// A flake with no devshell anywhere, so activation must stay silent.
  NoDevShell,
  /// Devshell defined only in an imported module, not in `flake.nix` itself.
  /// This is the dendritic shape that regressed before.
  DevShellInModule,
  /// A flake root containing a second, nested flake under `nested/`.
  Nested,
}

// ───────────────────────────── the world ─────────────────────────────

pub struct TestWorld {
  tmp:            tempfile::TempDir,
  flake_root:     PathBuf,
  fake_nix_dir:   PathBuf,
  xi_bin:         PathBuf,
  home:           PathBuf,
  state_home:     PathBuf,
  config_home:    PathBuf,
  runtime_dir:    PathBuf,
  started_flakes: Vec<PathBuf>,
}

pub struct TestWorldBuilder {
  fixture:   FlakeFixture,
  responses: Vec<NixResponse>,
  trusted:   bool,
}

impl TestWorld {
  pub fn builder() -> TestWorldBuilder {
    TestWorldBuilder {
      fixture:   FlakeFixture::WithDevShell,
      responses: vec![NixResponse::new()],
      trusted:   true,
    }
  }

  // ---- paths ----

  pub fn flake_root(&self) -> &Path {
    &self.flake_root
  }

  pub fn nested_flake_root(&self) -> PathBuf {
    self.flake_root.join("nested")
  }

  pub fn root(&self) -> &Path {
    self.tmp.path()
  }

  pub fn home(&self) -> &Path {
    &self.home
  }

  pub fn state_home(&self) -> &Path {
    &self.state_home
  }

  pub fn runtime_dir(&self) -> &Path {
    &self.runtime_dir
  }

  pub fn flake_id(&self) -> String {
    let canonical = fs::canonicalize(&self.flake_root)
      .unwrap_or_else(|_| self.flake_root.clone());
    xi_develop::dirs::flake_id(&canonical)
  }

  /// Per-flake state directory inside this world.
  pub fn state_dir(&self) -> PathBuf {
    self.state_home.join("xi/develop").join(self.flake_id())
  }

  pub fn socket_path(&self) -> PathBuf {
    self.runtime_dir.join(self.flake_id()).join("daemon.sock")
  }

  pub fn pid_path(&self) -> PathBuf {
    self.runtime_dir.join(self.flake_id()).join("daemon.pid")
  }

  // ---- environment as data ----

  /// Every variable a child process needs to live inside this world.
  pub fn env(&self) -> Vec<(String, String)> {
    let fake_nix = fake_nix_script();
    vec![
      ("HOME".into(), self.home.display().to_string()),
      (
        "XDG_STATE_HOME".into(),
        self.state_home.display().to_string(),
      ),
      (
        "XDG_CONFIG_HOME".into(),
        self.config_home.display().to_string(),
      ),
      (
        "XI_RUNTIME_DIR".into(),
        self.runtime_dir.display().to_string(),
      ),
      ("XI_NIX_BIN".into(), fake_nix.display().to_string()),
      (
        "XI_FAKE_NIX_DIR".into(),
        self.fake_nix_dir.display().to_string(),
      ),
      // Keep the real PATH: the fake nix is a bash script and the shells
      // under test are real binaries.
      (
        "PATH".into(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
      ),
    ]
  }

  /// Build a `Command` already placed inside this world.
  pub fn command(&self, program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear();
    for (k, v) in self.env() {
      cmd.env(k, v);
    }
    cmd.current_dir(&self.flake_root);
    cmd
  }

  /// Run `xi develop <args...>` inside this world.
  pub fn xi(&self, args: &[&str], cwd: &Path) -> Output {
    let mut cmd = self.command(&self.xi_bin);
    cmd.arg("develop").args(args).current_dir(cwd);
    cmd.output().expect("failed to spawn xi")
  }

  /// Run `xi develop prompt -s <shell> --pid <pid>`, the exact call the
  /// activation hook makes every prompt.
  pub fn prompt(&self, shell: Shell, cwd: &Path) -> PromptOutput {
    let out = self.xi(&["prompt", "-s", shell.name(), "--pid", "424242"], cwd);
    PromptOutput {
      stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
      stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
      status: out.status.code().unwrap_or(-1),
    }
  }

  // ---- trust ----

  /// Pre-write the trust marker, so trust is a precondition rather than a
  /// step under test.
  pub fn trust(&self) {
    let canonical = fs::canonicalize(&self.flake_root)
      .unwrap_or_else(|_| self.flake_root.clone());
    let dir = self.config_home.join("xi/develop/trusted");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(xi_develop::dirs::flake_id(&canonical)), "").unwrap();
  }

  pub fn untrust(&self) {
    let canonical = fs::canonicalize(&self.flake_root)
      .unwrap_or_else(|_| self.flake_root.clone());
    let marker = self
      .config_home
      .join("xi/develop/trusted")
      .join(xi_develop::dirs::flake_id(&canonical));
    let _ = fs::remove_file(marker);
  }

  // ---- fake nix control ----

  /// Replace the staged responses. Call `n` gets response `n`, and the last
  /// staged response repeats once exhausted.
  pub fn stage(&self, responses: &[NixResponse]) {
    let dir = self.fake_nix_dir.join("responses");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    for (i, r) in responses.iter().enumerate() {
      let base = dir.join(format!("{:03}", i + 1));
      fs::write(base.with_extension("json"), r.to_json()).unwrap();
      if r.exit_code != 0 {
        fs::write(base.with_extension("exit"), r.exit_code.to_string())
          .unwrap();
      }
      if let Some(err) = &r.stderr {
        fs::write(base.with_extension("stderr"), err).unwrap();
      }
      if let Some(d) = r.delay {
        fs::write(
          base.with_extension("delay"),
          format!("{:.3}", d.as_secs_f64()),
        )
        .unwrap();
      }
    }
  }

  /// Every fake-nix invocation so far, in order.
  pub fn nix_calls(&self) -> Vec<NixCall> {
    let log = self.fake_nix_dir.join("calls.log");
    let Ok(text) = fs::read_to_string(&log) else {
      return Vec::new();
    };

    let mut calls = Vec::new();
    let mut cwd = PathBuf::new();
    let mut args = Vec::new();
    for line in text.lines() {
      if let Some(v) = line.strip_prefix("cwd=") {
        cwd = PathBuf::from(v);
      } else if let Some(v) = line.strip_prefix("arg=") {
        args.push(v.to_string());
      } else if line == "end" {
        calls.push(NixCall {
          cwd:  std::mem::take(&mut cwd),
          args: std::mem::take(&mut args),
        });
      }
    }
    calls
  }

  pub fn nix_call_count(&self) -> usize {
    self.nix_calls().len()
  }

  /// Only the `print-dev-env` invocations.
  ///
  /// xi probes the nix binary on startup (`--version`,
  /// `config show experimental-features`), so the raw call log is not a
  /// measure of "did an eval happen". Assertions about evaluation must use
  /// this, or they count preflight noise and pass for the wrong reason.
  pub fn nix_evals(&self) -> Vec<NixCall> {
    self
      .nix_calls()
      .into_iter()
      .filter(|c| c.has_arg("print-dev-env"))
      .collect()
  }

  pub fn nix_eval_count(&self) -> usize {
    self.nix_evals().len()
  }

  // ---- observables, the only legitimate way to wait ----

  /// Poll `probe` until it returns true, or fail with what we were waiting
  /// for. There is deliberately no sleep-based wait in this harness: a fixed
  /// sleep is both slower and less reliable than polling a real signal.
  pub fn wait_for<F>(&self, what: &str, deadline: Duration, mut probe: F)
  where
    F: FnMut(&Self) -> bool,
  {
    let start = Instant::now();
    while start.elapsed() < deadline {
      if probe(self) {
        return;
      }
      std::thread::sleep(POLL_INTERVAL);
    }
    panic!(
      "timed out after {deadline:?} waiting for: {what}\n  socket: {}\n  \
       state dir: {}\n  nix calls: {}",
      self.socket_path().display(),
      self.state_dir().display(),
      self.nix_call_count()
    );
  }

  /// Value of a generation counter, or 0 when it does not exist yet.
  pub fn generation(&self, filename: &str) -> u64 {
    fs::read_to_string(self.state_dir().join(filename))
      .ok()
      .and_then(|s| s.trim().parse().ok())
      .unwrap_or(0)
  }

  pub fn env_generation(&self) -> u64 {
    self.generation("env-generation")
  }

  pub fn hook_generation(&self) -> u64 {
    self.generation("hook-generation")
  }

  pub fn socket_exists(&self) -> bool {
    self.socket_path().exists()
  }

  // ---- real-shell evaluation ----

  /// Evaluate `script` in a real shell inside this world and return the
  /// resulting environment.
  ///
  /// The dump is NUL-delimited so values containing newlines (a `shellHook`
  /// routinely does) survive the round trip intact.
  pub fn eval_in(
    &self,
    shell: Shell,
    script: &str,
  ) -> Option<HashMap<String, String>> {
    let bin = shell.skip_if_missing()?;

    let dump = match shell {
      Shell::Fish => "env -0",
      _ => "env -0",
    };
    let full = format!("{script}\n{dump}\n");

    let mut cmd = self.command(&bin);
    cmd.arg("-c").arg(&full);
    let out = cmd.output().expect("failed to spawn shell");

    assert!(
      out.status.success(),
      "{} failed to evaluate script: {}\n--- script ---\n{full}",
      shell.name(),
      String::from_utf8_lossy(&out.stderr)
    );

    Some(parse_env0(&out.stdout))
  }

  /// Evaluate a script and return its exit status plus the environment, for
  /// scripts that deliberately exit non-zero (the cd-out sentinel).
  pub fn eval_status(&self, shell: Shell, script: &str) -> Option<i32> {
    let bin = shell.skip_if_missing()?;
    let mut cmd = self.command(&bin);
    cmd.arg("-c").arg(script);
    let out = cmd.output().expect("failed to spawn shell");
    Some(out.status.code().unwrap_or(-1))
  }

  // ---- leak assertion ----

  /// Assert nothing escaped the world: no file under the real `HOME`-derived
  /// locations, and nothing in the process-global runtime dir.
  ///
  /// Backs SC-H-001.
  pub fn assert_no_leaks(&self) {
    let global = xi_develop::dirs::platform_runtime_dir().join(self.flake_id());
    assert!(
      !global.exists(),
      "daemon state leaked into the process-global runtime dir: {}",
      global.display()
    );

    // Everything the world did create must be inside the world.
    for p in [self.socket_path(), self.pid_path(), self.state_dir()] {
      if p.exists() {
        assert!(
          p.starts_with(self.tmp.path()),
          "world artifact escaped the temp dir: {}",
          p.display()
        );
      }
    }
  }
}

impl Drop for TestWorld {
  /// Reap any daemon this world started. A daemon left holding a socket in a
  /// deleted temp directory is a slow-burning source of cross-test
  /// interference, so this is not optional cleanup.
  fn drop(&mut self) {
    for flake in self.started_flakes.clone() {
      let mut cmd = self.command(&self.xi_bin);
      cmd.arg("develop").arg("daemon").arg("stop");
      cmd.current_dir(&flake);
      let _ = cmd.output();
    }

    // Graceful stop may not have landed; SIGKILL whatever the pid file names.
    if let Ok(content) = fs::read_to_string(self.pid_path())
      && let Some(pid) = content
        .split(':')
        .next()
        .and_then(|p| p.trim().parse::<i32>().ok())
    {
      let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
      );
    }
  }
}

impl TestWorldBuilder {
  pub fn flake(mut self, fixture: FlakeFixture) -> Self {
    self.fixture = fixture;
    self
  }

  pub fn nix(mut self, responses: &[NixResponse]) -> Self {
    self.responses = responses.to_vec();
    self
  }

  pub fn trusted(mut self, yes: bool) -> Self {
    self.trusted = yes;
    self
  }

  pub fn build(self) -> TestWorld {
    // Short prefix on purpose: the socket path must fit sun_path (103 bytes),
    // and a long temp prefix plus flake id plus "daemon.sock" can exceed it.
    let tmp = tempfile::Builder::new()
      .prefix("xw")
      .tempdir()
      .expect("failed to create world temp dir");

    let base = tmp.path().to_path_buf();
    let home = base.join("h");
    let state_home = base.join("s");
    let config_home = base.join("c");
    let runtime_dir = base.join("r");
    let fake_nix_dir = base.join("n");
    let flake_root = base.join("f");

    for d in [
      &home,
      &state_home,
      &config_home,
      &runtime_dir,
      &fake_nix_dir,
      &flake_root,
    ] {
      fs::create_dir_all(d).unwrap();
    }
    fs::create_dir_all(fake_nix_dir.join("responses")).unwrap();
    fs::create_dir_all(fake_nix_dir.join("store")).unwrap();

    write_flake(&flake_root, self.fixture);

    // Pin the binary under test by state file rather than by PATH order, so
    // every daemon the hook spawns is provably this build.
    let xi_bin = PathBuf::from(env!("CARGO_BIN_EXE_xi"));
    let develop_state = state_home.join("xi/develop");
    fs::create_dir_all(&develop_state).unwrap();
    let mut f = fs::File::create(develop_state.join("xi-bin")).unwrap();
    write!(f, "{}", xi_bin.display()).unwrap();

    let world = TestWorld {
      tmp,
      flake_root,
      fake_nix_dir,
      xi_bin,
      home,
      state_home,
      config_home,
      runtime_dir,
      started_flakes: Vec::new(),
    };

    world.stage(&self.responses);
    if self.trusted {
      world.trust();
    }

    let socket = world.socket_path();
    assert!(
      xi_develop::dirs::check_socket_path_len(&socket).is_ok(),
      "world socket path is too long for a unix socket: {}",
      socket.display()
    );

    world
  }
}

// ───────────────────────────── helpers ─────────────────────────────

/// Captured result of one `xi develop prompt` call.
pub struct PromptOutput {
  pub stdout: String,
  pub stderr: String,
  pub status: i32,
}

impl PromptOutput {
  pub fn is_silent(&self) -> bool {
    self.stdout.trim().is_empty()
  }

  pub fn spawns_subshell(&self) -> bool {
    self.stdout.contains("__XI_IN_DEVSHELL=1")
  }
}

/// Path to the committed fake-nix fixture.
pub fn fake_nix_script() -> PathBuf {
  // CARGO_MANIFEST_DIR is crates/xi; the fixture lives with the crate whose
  // behaviour it fakes.
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../xi-develop/tests/fixtures/fake-nix")
    .canonicalize()
    .expect("fake-nix fixture missing or not readable")
}

fn parse_env0(bytes: &[u8]) -> HashMap<String, String> {
  let mut map = HashMap::new();
  for record in bytes.split(|b| *b == 0) {
    if record.is_empty() {
      continue;
    }
    let text = String::from_utf8_lossy(record);
    if let Some((k, v)) = text.split_once('=') {
      map.insert(k.to_string(), v.to_string());
    }
  }
  map
}

fn write_flake(root: &Path, fixture: FlakeFixture) {
  let with_devshell = r#"{
  description = "harness fixture";
  outputs = { self, ... }: {
    devShells.x86_64-linux.default = { };
  };
}
"#;
  let without = r#"{
  description = "harness fixture without a dev environment";
  outputs = { self, ... }: {
    packages.x86_64-linux.hello = { };
  };
}
"#;

  // The daemon's file watcher is git-aware: `GitWatcher::new` opens the flake
  // root as a git repository and derives its watch set from the tracked
  // files. A fixture that is not a repo makes the watcher unavailable and the
  // daemon falls back to hook-triggered eval, so any test about re-eval on
  // file change would silently never exercise the watcher at all.
  let init_git = |root: &Path| {
    let run = |args: &[&str]| {
      let _ = Command::new("git").args(args).current_dir(root).output();
    };
    run(&["init", "-q"]);
    run(&["add", "-A"]);
  };

  match fixture {
    FlakeFixture::WithDevShell => {
      fs::write(root.join("flake.nix"), with_devshell).unwrap();
      init_git(root);
    },
    FlakeFixture::NoDevShell => {
      fs::write(root.join("flake.nix"), without).unwrap();
      init_git(root);
    },
    FlakeFixture::DevShellInModule => {
      // flake.nix itself has no devshell keyword; the imported module does.
      fs::write(
        root.join("flake.nix"),
        r#"{
  description = "dendritic fixture";
  outputs = { self, import-tree, ... }:
    import-tree ./modules;
}
"#,
      )
      .unwrap();
      fs::create_dir_all(root.join("modules")).unwrap();
      fs::write(
        root.join("modules/shell.nix"),
        "{ perSystem = { devShells.default = { }; }; }\n",
      )
      .unwrap();
      init_git(root);
    },
    FlakeFixture::Nested => {
      fs::write(root.join("flake.nix"), with_devshell).unwrap();
      let nested = root.join("nested");
      fs::create_dir_all(&nested).unwrap();
      fs::write(nested.join("flake.nix"), with_devshell).unwrap();
      // Each flake root is its own repo: nested must be independently
      // watchable, and a parent repo would otherwise swallow it.
      init_git(root);
      init_git(&nested);
    },
  }
}
