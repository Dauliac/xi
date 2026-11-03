//! The `xi-sources-conflict` capability: who wins when the three refs
//! disagree.
//!
//! The policy is one sentence from the user: *the current sources are
//! always right, and a local non-pushed version is a normal state*.
//! Spelled out, that means two rules this module enforces:
//!
//! - **local wins on the interactive path.** `xi <scope> switch` evaluates the
//!   working dir whenever one is configured and the tree is clean. Commits the
//!   operator has not pushed yet are not a problem to be resolved; they are the
//!   thing being deployed.
//! - **the gitops daemon never touches the working dir.** Its only write target
//!   is the sources dir, and [`assert_daemon_write_target`] is the assertion
//!   its write path runs to prove it.
//!
//! Reconciliation in the other direction (sources dir to working dir)
//! happens through [`sync`], a copy-with-erase modelled on chezmoi's
//! `apply`: the sources dir holds the authority and the working dir is
//! rewritten to match. `git pull` is deliberately not used, because
//! pulling into a tree the operator edits by hand creates merge
//! conflicts nobody asked for.
//!
//! See `openspec/changes/xi-sources-conflict/specs/xi-sources-conflict/spec.
//! md`.

use std::{
  ffi::OsString,
  fmt,
  fmt::Write as _,
  io,
  path::{Component, Path, PathBuf},
  process::{Command, Output},
};

use serde::{Deserialize, Serialize};
// `error` is only reached by the release build of `reject_write`, which
// logs where the debug build panics.
#[cfg(not(debug_assertions))] use tracing::error;
use tracing::{debug, info, warn};

use super::{RefLineage, Sha, config::SyncMode};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Anything that can go wrong while reasoning about, or acting on, the
/// three refs.
#[derive(Debug, thiserror::Error)]
pub enum ConflictError {
  /// A git invocation failed. Carries the repo and the subcommand so the
  /// message names which of the two clones misbehaved.
  #[error("git {args} failed in {}: {stderr}", repo.display())]
  Git {
    /// The repository the command ran in.
    repo:   PathBuf,
    /// The arguments, space-joined, without the leading `git -C <repo>`.
    args:   String,
    /// Git's stderr, trimmed.
    stderr: String,
  },

  /// A filesystem operation failed.
  #[error("{}: {source}", path.display())]
  Io {
    /// The path being read or written.
    path:   PathBuf,
    /// The underlying failure.
    #[source]
    source: io::Error,
  },

  /// A write was attempted outside the only root the operation may touch.
  #[error(transparent)]
  Guard(#[from] WriteOutsideRoot),

  /// A git subcommand forbidden in the working dir was attempted.
  #[error(transparent)]
  Forbidden(#[from] ForbiddenWorkingDirGit),

  /// The working dir is dirty and `--force-dirty` was not given.
  #[error("{0}")]
  Dirty(String),

  /// A ref the operation needs is not known.
  #[error("{0}")]
  UnknownRef(String),
}

// ---------------------------------------------------------------------------
// Write guard  -  tasks 4.1 and 5a.3
// ---------------------------------------------------------------------------

/// A write aimed outside the single root the current operation owns.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
  "refusing to write {}: outside {}, the only tree this operation may modify",
  target.display(),
  allowed_root.display()
)]
pub struct WriteOutsideRoot {
  /// The path the caller tried to write.
  pub target:       PathBuf,
  /// The only root the caller is allowed to write under.
  pub allowed_root: PathBuf,
}

/// Decide whether `target` lies under `allowed_root`, without side effects.
///
/// This is the pure half of the guard: [`guard_write`] adds the panic-in-debug
/// policy on top. Keeping them separate is what makes the rejection path
/// testable, since tests themselves run in debug builds.
///
/// Comparison is done on resolved paths: `..` components are folded
/// lexically and the deepest existing ancestor is canonicalised, so a
/// symlinked root (`/tmp` on macOS) and a traversal escape
/// (`<root>/../../etc/passwd`) are both judged on where they actually
/// land rather than on how they were spelled.
///
/// # Errors
///
/// Returns [`WriteOutsideRoot`] when `target` does not resolve under
/// `allowed_root`.
pub fn check_write_target(
  target: &Path,
  allowed_root: &Path,
) -> Result<(), WriteOutsideRoot> {
  if resolve_for_guard(target).starts_with(resolve_for_guard(allowed_root)) {
    return Ok(());
  }
  Err(WriteOutsideRoot {
    target:       target.to_path_buf(),
    allowed_root: allowed_root.to_path_buf(),
  })
}

/// Enforce the write-root invariant with the spec's build-dependent policy.
///
/// A violation is a bug in xi, never operator error, so the two builds
/// treat it differently on purpose: debug panics so the bug cannot be
/// missed in a test run, release logs and returns the error so the
/// calling tick aborts instead of taking the whole daemon down.
///
/// # Errors
///
/// In release builds, returns [`WriteOutsideRoot`] after logging it. In
/// debug builds the same condition panics.
pub fn guard_write(
  target: &Path,
  allowed_root: &Path,
) -> Result<(), WriteOutsideRoot> {
  match check_write_target(target, allowed_root) {
    Ok(()) => Ok(()),
    Err(violation) => Err(reject_write(violation)),
  }
}

/// The assertion the gitops daemon's write path runs before every write.
///
/// The daemon's sanctioned write targets are the sources dir and the
/// state dir's metadata files. Everything else - the working dir above
/// all, but equally the operator's `$HOME` - is off limits, because the
/// daemon fetching a remote must never be able to clobber a tree the
/// operator is editing.
///
/// # Errors
///
/// See [`guard_write`].
pub fn assert_daemon_write_target(
  target: &Path,
  sources_dir: &Path,
) -> Result<(), WriteOutsideRoot> {
  guard_write(target, sources_dir)
}

#[cfg(debug_assertions)]
#[expect(
  clippy::panic,
  reason = "the spec mandates a debug-build panic on this invariant"
)]
fn reject_write(violation: WriteOutsideRoot) -> WriteOutsideRoot {
  panic!("{violation}");
}

#[cfg(not(debug_assertions))]
fn reject_write(violation: WriteOutsideRoot) -> WriteOutsideRoot {
  error!(
    target = %violation.target.display(),
    allowed_root = %violation.allowed_root.display(),
    "rejected write outside the sanctioned root; aborting the operation"
  );
  violation
}

/// Fold `.` and `..` away and canonicalise as much of the path as exists.
fn resolve_for_guard(path: &Path) -> PathBuf {
  let normalized = lexical_normalize(path);

  let mut existing: &Path = normalized.as_path();
  let mut tail: Vec<OsString> = Vec::new();
  loop {
    if let Ok(real) = existing.canonicalize() {
      let mut out = real;
      for part in tail.iter().rev() {
        out.push(part);
      }
      return out;
    }
    match (existing.parent(), existing.file_name()) {
      (Some(parent), Some(name)) => {
        tail.push(name.to_owned());
        existing = parent;
      },
      _ => return normalized,
    }
  }
}

/// Resolve `.` and `..` without touching the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  for component in path.components() {
    match component {
      Component::CurDir => {},
      Component::ParentDir => {
        out.pop();
      },
      other => out.push(other),
    }
  }
  out
}

// ---------------------------------------------------------------------------
// Forbidden git in the working dir  -  task 5a.5
// ---------------------------------------------------------------------------

/// Git subcommands that must never run inside the working dir.
///
/// Every one of them merges remote history into a tree the operator owns,
/// which is exactly the class of surprise [`sync`] exists to avoid. They
/// are legal in the sources dir, which is xi's own clone.
pub const FORBIDDEN_WORKING_DIR_GIT: &[&str] =
  &["pull", "merge", "rebase", "fetch", "cherry-pick", "am"];

/// A forbidden git subcommand aimed at the working dir.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
  "`git {subcommand}` is forbidden in the working dir {}: the only sanctioned \
   write into it is `xi <scope> sources sync`",
  working_dir.display()
)]
pub struct ForbiddenWorkingDirGit {
  /// The working dir the command was aimed at.
  pub working_dir: PathBuf,
  /// The subcommand that was rejected.
  pub subcommand:  String,
}

/// Reject a forbidden subcommand before it reaches `exec`.
///
/// Checked on the argument vector rather than after the fact, so the
/// rejection happens while nothing has been written yet.
///
/// # Errors
///
/// Returns [`ForbiddenWorkingDirGit`] when the first argument is in
/// [`FORBIDDEN_WORKING_DIR_GIT`].
pub fn check_working_dir_git(
  working_dir: &Path,
  args: &[&str],
) -> Result<(), ForbiddenWorkingDirGit> {
  let Some(subcommand) = args.first() else {
    return Ok(());
  };
  if FORBIDDEN_WORKING_DIR_GIT.contains(subcommand) {
    return Err(ForbiddenWorkingDirGit {
      working_dir: working_dir.to_path_buf(),
      subcommand:  (*subcommand).to_owned(),
    });
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// The lint half of task 5a.5
// ---------------------------------------------------------------------------

pub mod lint {
  //! A source lint for merging git subcommands aimed at the working dir.
  //!
  //! [`check_working_dir_git`](super::check_working_dir_git) is the
  //! runtime half of task 5a.5 and only sees what actually reaches
  //! this module's `working_dir_git`. This is the other half: a call
  //! that spells its own `Command::new("git")` bypasses
  //! the guard entirely and would only be caught by whoever reads the
  //! diff.
  //!
  //! It runs as a test rather than as a `rustc` lint for the reason
  //! `xi-sources-observability`'s `redact::lint` gives: the
  //! alternative is a custom clippy driver, and a lint only CI has is
  //! a lint nobody runs locally.
  //!
  //! The rule is deliberately narrow -- a forbidden subcommand *and* a
  //! working-dir marker on the same line -- because the lint has to be
  //! readable by whoever it rejects. Scanning stops at the file's
  //! `#[cfg(test)]`, and `tests/` is skipped outright: a fixture
  //! modelling the operator's own `git fetch` is not xi's code path,
  //! and the operator's git is theirs to run.

  use std::{fs, io, path::Path};

  use super::FORBIDDEN_WORKING_DIR_GIT;

  /// Identifiers that mark a line as talking about the working dir.
  pub const WORKING_DIR_MARKERS: &[&str] =
    &["working_dir", "workingDir", "working-dir"];

  /// Identifiers whose presence means the line *is* the guard, not a
  /// bypass of it.
  const GUARD_IDENTIFIERS: &[&str] = &[
    "FORBIDDEN_WORKING_DIR_GIT",
    "check_working_dir_git",
    "ForbiddenWorkingDirGit",
    "WORKING_DIR_MARKERS",
  ];

  /// One rejected call site.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Violation {
    /// Path of the offending file, as walked.
    pub file:       String,
    /// 1-based line number.
    pub line:       usize,
    /// The forbidden subcommand that triggered the rejection.
    pub subcommand: String,
    /// The source line, trimmed.
    pub text:       String,
  }

  impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(
        f,
        "{}:{}: `git {}` aimed at the working dir: {}",
        self.file, self.line, self.subcommand, self.text
      )
    }
  }

  /// Scan every non-test `.rs` file under `root`.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from walking or reading the tree.
  pub fn scan_workspace(root: &Path) -> io::Result<Vec<Violation>> {
    let mut found = Vec::new();
    scan_dir(root, &mut found)?;
    found.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(found)
  }

  fn scan_dir(dir: &Path, found: &mut Vec<Violation>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
      let path = entry?.path();
      let name = path.file_name().unwrap_or_default().to_string_lossy();
      if path.is_dir() {
        // `target` holds generated code; `tests` is not xi's code path.
        if name != "target" && name != "tests" {
          scan_dir(&path, found)?;
        }
      } else if path.extension().is_some_and(|ext| ext == "rs") {
        scan_file(&path, found)?;
      }
    }
    Ok(())
  }

  fn scan_file(path: &Path, found: &mut Vec<Violation>) -> io::Result<()> {
    let raw = fs::read_to_string(path)?;

    for (index, line) in raw.lines().enumerate() {
      let text = line.trim();
      // Tests live at the bottom of a module here, so the first
      // `#[cfg(test)]` ends the part of the file that ships.
      if text.starts_with("#[cfg(test)]") {
        break;
      }
      if text.starts_with("//") || !names_working_dir(text) {
        continue;
      }
      if GUARD_IDENTIFIERS.iter().any(|id| text.contains(id)) {
        continue;
      }
      if let Some(subcommand) = forbidden_subcommand_in(text) {
        found.push(Violation {
          file:       path.display().to_string(),
          line:       index + 1,
          subcommand: subcommand.to_owned(),
          text:       text.to_owned(),
        });
      }
    }
    Ok(())
  }

  fn names_working_dir(text: &str) -> bool {
    WORKING_DIR_MARKERS
      .iter()
      .any(|marker| text.contains(marker))
  }

  /// The first forbidden subcommand spelled as a git argument on
  /// `text`, if any.
  ///
  /// Matched as a quoted whole word, so `"fetch_head"` and a sentence
  /// mentioning the word are both left alone: the lint is about an
  /// argument vector, not about vocabulary.
  fn forbidden_subcommand_in(text: &str) -> Option<&'static str> {
    FORBIDDEN_WORKING_DIR_GIT
      .iter()
      .copied()
      .find(|subcommand| text.contains(&format!("\"{subcommand}\"")))
  }
}

// ---------------------------------------------------------------------------
// git plumbing
// ---------------------------------------------------------------------------

/// Run git in `repo`, returning the raw output.
///
/// `GIT_TERMINAL_PROMPT=0` matches the adoption path: a missing
/// credential has to be a clean failure, not a hung process.
fn run_git(repo: &Path, args: &[&str]) -> Result<Output, ConflictError> {
  run_git_with(repo, None, args)
}

/// Run git in `repo`, optionally lending it another repository's objects.
///
/// `alternates` points at a second object store, which git reads but
/// never writes. This is how an ancestry question spanning both clones
/// gets answered: the working dir and the sources dir each hold commits
/// the other has never seen, and without the loan `git merge-base` would
/// have to answer "unknown" for exactly the diverged case the policy
/// exists to catch. Lending objects is read-only, so it does not breach
/// the "daemon never touches the working dir" invariant in either
/// direction.
fn run_git_with(
  repo: &Path,
  alternates: Option<&Path>,
  args: &[&str],
) -> Result<Output, ConflictError> {
  let mut command = Command::new("git");
  command.env("GIT_TERMINAL_PROMPT", "0");
  if let Some(objects) = alternates.and_then(objects_dir) {
    command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects);
  }
  command
    .arg("-C")
    .arg(repo)
    .args(args)
    .output()
    .map_err(|source| {
      ConflictError::Io {
        path: repo.to_path_buf(),
        source,
      }
    })
}

/// The object store of a repository, worktree or bare.
fn objects_dir(repo: &Path) -> Option<PathBuf> {
  [repo.join(".git").join("objects"), repo.join("objects")]
    .into_iter()
    .find(|dir| dir.is_dir())
}

fn git_error(repo: &Path, args: &[&str], out: &Output) -> ConflictError {
  ConflictError::Git {
    repo:   repo.to_path_buf(),
    args:   args.join(" "),
    stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
  }
}

/// Run git in `repo` and return stdout as text.
fn git_text(repo: &Path, args: &[&str]) -> Result<String, ConflictError> {
  let out = run_git(repo, args)?;
  if out.status.success() {
    return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
  }
  Err(git_error(repo, args, &out))
}

/// Run git in `repo` and return stdout as bytes.
///
/// Used for blob contents, which are not text in general.
fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>, ConflictError> {
  let out = run_git(repo, args)?;
  if out.status.success() {
    return Ok(out.stdout);
  }
  Err(git_error(repo, args, &out))
}

/// Run git in the working dir, through the forbidden-subcommand check.
///
/// This is the *only* function in the capability that executes git
/// against the working dir. Routing every call through one place is what
/// makes [`check_working_dir_git`] an enforced invariant rather than a
/// convention.
fn working_dir_git(
  working_dir: &Path,
  args: &[&str],
) -> Result<String, ConflictError> {
  check_working_dir_git(working_dir, args)?;
  git_text(working_dir, args)
}

/// [`working_dir_git`], but against a `GIT_INDEX_FILE` other than the
/// real one.
///
/// [`tracked_dirty_paths`] is the only caller: comparing the literal
/// disk state to `HEAD` needs an index the real one's staged state
/// cannot leak into, so this still goes through
/// [`check_working_dir_git`] - the forbidden-subcommand invariant is
/// about the working *tree*, not the index file a call happens to
/// use - but cannot share [`working_dir_git`]'s signature.
fn working_dir_git_with_index(
  working_dir: &Path,
  index_file: &Path,
  args: &[&str],
) -> Result<String, ConflictError> {
  check_working_dir_git(working_dir, args)?;
  let out = Command::new("git")
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_INDEX_FILE", index_file)
    .arg("-C")
    .arg(working_dir)
    .args(args)
    .output()
    .map_err(|source| {
      ConflictError::Io {
        path: working_dir.to_path_buf(),
        source,
      }
    })?;
  if out.status.success() {
    return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
  }
  Err(git_error(working_dir, args, &out))
}

/// Whether `repo` has the object named by `rev`.
fn has_object(repo: &Path, rev: &str) -> bool {
  run_git(repo, &["cat-file", "-e", &format!("{rev}^{{commit}}")])
    .is_ok_and(|out| out.status.success())
}

/// Resolve `HEAD` in `repo`, or `None` when there is no readable HEAD.
///
/// A fresh clone with no commits, a path that is not a repository, and a
/// missing git binary all collapse to `None` on purpose: the caller's
/// next question is always "do I know this ref", never "why not".
#[must_use]
pub fn head_of(repo: &Path) -> Option<Sha> {
  let head = working_dir_git(repo, &["rev-parse", "HEAD"]).ok()?;
  let trimmed = head.trim();
  (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

// ---------------------------------------------------------------------------
// Dirty-tree detection  -  section 3
// ---------------------------------------------------------------------------

/// Markers git leaves in `.git` while a multi-step operation is open.
///
/// Their presence means the operator is mid-operation, which is dirty
/// even when the tree itself happens to have no modified files.
pub const IN_PROGRESS_MARKERS: &[&str] =
  &["MERGE_HEAD", "REBASE_HEAD", "CHERRY_PICK_HEAD"];

/// How many dirty paths a refusal names before it counts the rest.
pub const DIRTY_PATHS_SHOWN: usize = 20;

/// The flag that overrides every dirty-tree refusal in this capability.
pub const FORCE_DIRTY_FLAG: &str = "--force-dirty";

/// Everything that makes a working dir dirty, as observed at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyTree {
  /// Paths `git status --porcelain=v2` reported, in git's order.
  pub paths:       Vec<String>,
  /// In-progress markers found, from [`IN_PROGRESS_MARKERS`].
  pub in_progress: Vec<String>,
  /// Whether `git stash list` printed anything.
  pub stashed:     bool,
}

impl DirtyTree {
  /// Dirty is the disjunction, clean is the absence of all three.
  #[must_use]
  pub const fn is_dirty(&self) -> bool {
    !self.paths.is_empty() || !self.in_progress.is_empty() || self.stashed
  }

  /// The refusal message: what is dirty, and the one flag that overrides it.
  ///
  /// `what` names the operation being refused (`xi home switch`,
  /// `xi home sources sync`) so the message reads as a sentence about
  /// the command the operator actually typed.
  #[must_use]
  pub fn refusal(&self, what: &str) -> String {
    let mut message = format!("{what} refuses to run on a dirty working dir.");

    if !self.paths.is_empty() {
      message.push_str("\n\nuncommitted changes:");
      for path in self.paths.iter().take(DIRTY_PATHS_SHOWN) {
        message.push_str("\n  ");
        message.push_str(path);
      }
      if let Some(rest) = self.paths.len().checked_sub(DIRTY_PATHS_SHOWN)
        && rest > 0
      {
        let _ = write!(message, "\n  ... and {rest} more");
      }
    }
    if !self.in_progress.is_empty() {
      let _ = write!(
        message,
        "\n\noperation in progress: {}",
        self.in_progress.join(", ")
      );
    }
    if self.stashed {
      message.push_str("\n\nthe stash is not empty");
    }

    let _ = write!(
      message,
      "\n\ncommit, stash, or re-run with {FORCE_DIRTY_FLAG}."
    );
    message
  }
}

impl fmt::Display for DirtyTree {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.is_dirty() {
      write!(f, "dirty ({} path(s))", self.paths.len())
    } else {
      f.write_str("clean")
    }
  }
}

/// Observe whether `working_dir` is dirty, and how.
///
/// A plain `git status` is read first; when it reports nothing, that is
/// unambiguous and `paths` is empty. When it reports *something*,
/// [`tracked_dirty_paths`] is asked to settle what it means: `git
/// status`'s staged/unstaged split goes through the index, and
/// [`sync`]'s `reset --soft` is a deliberate case where the index
/// disagrees with both `HEAD` and the working tree it just wrote - see
/// xi-5ytq. Gating the heavier check on "status saw something" keeps
/// the common fully-clean path as cheap as before this fix.
///
/// # Errors
///
/// Propagates the git failure when `working_dir` is not a readable
/// repository. "Not a repo" is an error here rather than "clean",
/// because treating an unreadable tree as safe is how a `--force-dirty`
/// style accident happens without anyone asking for it.
pub fn inspect_working_tree(
  working_dir: &Path,
) -> Result<DirtyTree, ConflictError> {
  let status = working_dir_git(working_dir, &["status", "--porcelain=v2"])?;
  let status_paths = parse_porcelain_v2(&status);
  let git_dir = resolve_git_dir(working_dir);

  let paths = if status_paths.is_empty() {
    status_paths
  } else {
    tracked_dirty_paths(working_dir, &git_dir).unwrap_or(status_paths)
  };

  let in_progress = IN_PROGRESS_MARKERS
    .iter()
    .filter(|marker| git_dir.join(marker).exists())
    .map(|marker| (*marker).to_owned())
    .collect();

  // A stash entry is uncommitted work the operator parked; a sync that
  // rewrites the tree under it would leave them holding a patch that no
  // longer applies.
  let stashed = working_dir_git(working_dir, &["stash", "list"])
    .is_ok_and(|out| !out.trim().is_empty());

  let tree = DirtyTree {
    paths,
    in_progress,
    stashed,
  };
  debug!(working_dir = %working_dir.display(), state = %tree, "inspected working tree");
  Ok(tree)
}

/// The real `.git` location, following a worktree/submodule `.git` file.
fn resolve_git_dir(working_dir: &Path) -> PathBuf {
  working_dir_git(working_dir, &["rev-parse", "--absolute-git-dir"])
    .ok()
    .map(|out| PathBuf::from(out.trim()))
    .filter(|dir| dir.is_dir())
    .unwrap_or_else(|| working_dir.join(".git"))
}

/// Pull the paths out of `git status --porcelain=v2`.
///
/// The format is documented in git-status(1). Four record kinds carry a
/// path, at a fixed field offset each:
///
/// - `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
/// - `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>\t<origPath>`
/// - `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
/// - `? <path>` and `! <path>`
///
/// Paths can contain spaces, so each kind takes "the rest of the line"
/// after a fixed number of fields rather than splitting on whitespace.
fn parse_porcelain_v2(output: &str) -> Vec<String> {
  output
    .lines()
    .filter(|line| !line.trim().is_empty())
    .filter_map(|line| {
      let (kind, rest) = line.split_once(' ')?;
      let path = match kind {
        "1" => field_rest(rest, 7),
        "2" => field_rest(rest, 8).map(|p| rename_target(&p).to_owned()),
        "u" => field_rest(rest, 9),
        "?" | "!" => Some(rest.to_owned()),
        // `# branch.*` headers and anything git adds later.
        _ => None,
      }?;
      (!path.is_empty()).then_some(path)
    })
    .collect()
}

/// Everything after the first `skip` space-separated fields of `rest`.
fn field_rest(rest: &str, skip: usize) -> Option<String> {
  let mut remainder = rest;
  for _ in 0..skip {
    let (_, tail) = remainder.split_once(' ')?;
    remainder = tail;
  }
  Some(remainder.to_owned())
}

/// A rename record's path is `<path>\t<origPath>`; report the new name.
fn rename_target(path: &str) -> &str {
  path.split_once('\t').map_or(path, |(new, _orig)| new)
}

/// Non-empty lines of `output`, each its own owned `String`.
fn non_empty_lines(output: &str) -> Vec<String> {
  output
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect()
}

/// A throw-away `GIT_INDEX_FILE`, removed when dropped.
///
/// Lives under the real `.git` dir - always the same filesystem as the
/// index it stands in for - and never under `working_dir` itself, so
/// nothing it touches is a tracked or untracked path that could leak
/// into the very comparison it exists to make. Best-effort cleanup: a
/// leaked temp file next to a real index is litter, not a correctness
/// problem, so a failed removal is not propagated.
struct TempIndex(PathBuf);

impl TempIndex {
  fn new(git_dir: &Path) -> Self {
    let nanos = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |elapsed| elapsed.as_nanos());
    Self(git_dir.join(format!(
      "xi-dirty-check-index-{}-{nanos}",
      std::process::id()
    )))
  }
}

impl Drop for TempIndex {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

/// Tracked and untracked paths that differ from `HEAD`, measured
/// against the literal disk state. `None` when `working_dir` has no
/// `HEAD` to diff against (an unborn branch) or the index build
/// otherwise fails, so the caller can fall back to the plain
/// index-based view.
///
/// This is the fix for xi-5ytq, and it takes two attempts to get
/// right - both tried and rejected while writing the BDD suite for
/// this bead, kept here so the next person does not retry them:
///
/// - **`git diff --name-only HEAD`**, comparing the working tree
///   straight to `HEAD` without the real index as an intermediate.
///   This handles a *modified* tracked file correctly, but
///   `sync`'s write pass can also add a path the index has never
///   seen, and `git diff <commit>` resolves "`HEAD` has this path,
///   the index does not" as a deletion without ever reading the
///   working tree - so a file `sync` just wrote still read as dirty.
/// - **`git stash create`**, snapshotting index and worktree into a
///   commit with no ref and no index/worktree side effects. This
///   handles additions correctly, but its worktree layer does not
///   reliably represent a path `sync`'s delete pass removed from
///   disk while the stale index entry survives: the resulting tree
///   kept the pre-sync blob, so a file `sync` just erased still read
///   as dirty too.
///
/// What works: build a **fresh** index from the literal working tree
/// (`git add -A` against a throw-away [`TempIndex`], touching neither
/// the real index nor the working tree) and diff *that* against
/// `HEAD`. This is the same question a commit would answer - would
/// committing this disk content right now be a no-op against `HEAD` -
/// and it is the question xi actually has: a working tree identical to
/// `HEAD` is reproducible from `sources_head` regardless of what the
/// real index happens to be sitting on.
///
/// A genuine edit - staged, unstaged, or both - still differs from
/// `HEAD` regardless of the index, so SC-X-002's refusal is unaffected:
/// this changes what counts as dirt, not whether dirt is caught. The
/// trade-off is cost: unlike a plain `git status`, this hashes every
/// file in the working tree, which is why [`inspect_working_tree`]
/// only reaches for it once a plain `git status` has already reported
/// something to disambiguate.
fn tracked_dirty_paths(working_dir: &Path, git_dir: &Path) -> Option<Vec<String>> {
  let temp_index = TempIndex::new(git_dir);
  working_dir_git_with_index(working_dir, &temp_index.0, &["add", "-A"]).ok()?;
  let diff = working_dir_git_with_index(
    working_dir,
    &temp_index.0,
    &["diff", "--cached", "--name-only", "HEAD"],
  )
  .ok()?;
  Some(non_empty_lines(&diff))
}

// ---------------------------------------------------------------------------
// The three-ref state matrix  -  section 2
// ---------------------------------------------------------------------------

/// How one ref stands relative to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefRelation {
  /// Same sha.
  Identical,
  /// The subject is a descendant of the baseline.
  Ahead,
  /// The subject is an ancestor of the baseline.
  Behind,
  /// Both have commits the other lacks.
  Diverged,
  /// Ancestry could not be decided: a sha is missing, or the objects are
  /// not present in the repository that was asked.
  Unknown,
}

/// Decide how `subject` stands relative to `baseline`, as seen from `repo`.
///
/// Ancestry is a question about objects, so it can only be answered by a
/// repository that has both. When either object is missing the answer is
/// [`RefRelation::Unknown`] rather than a guess: two clones of the same
/// remote usually share history, but "usually" is not something to
/// decide a deployment on.
#[must_use]
pub fn relate(repo: &Path, subject: &str, baseline: &str) -> RefRelation {
  if subject == baseline {
    return RefRelation::Identical;
  }
  if !has_object(repo, subject) || !has_object(repo, baseline) {
    return RefRelation::Unknown;
  }

  let is_ancestor = |ancestor: &str, descendant: &str| {
    run_git(repo, &["merge-base", "--is-ancestor", ancestor, descendant])
      .is_ok_and(|out| out.status.success())
  };

  match (
    is_ancestor(baseline, subject),
    is_ancestor(subject, baseline),
  ) {
    (true, _) => RefRelation::Ahead,
    (false, true) => RefRelation::Behind,
    (false, false) => RefRelation::Diverged,
  }
}

/// One of the six states the spec's matrix enumerates.
///
/// The variant names describe the state; [`ConflictState::case`] gives
/// the matrix row number so a failing test can be read against the spec
/// table directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictState {
  /// Row 1: all three refs agree.
  InSync,
  /// Row 2: the working dir has unpushed commits. A normal state.
  WorkingAhead,
  /// Row 3: the sources dir holds something the working dir does not, so
  /// `xi <scope> sources sync` has work to do.
  SyncAvailable,
  /// Row 4: rows 2 and 3 at once. Local still wins; the operator
  /// resolves it with `git push` or an explicit sync.
  WorkingAheadSyncAvailable,
  /// Row 5: the working dir and the sources dir both moved past their
  /// common ancestor. xi refuses rather than choosing for the operator.
  WorkingDiverged,
  /// Row 6: the remote is not a descendant of the recorded
  /// `sources_head`, i.e. somebody force-pushed.
  RemoteForcePush,
  /// Ancestry could not be established, so no row applies.
  Unknown,
}

impl ConflictState {
  /// The matrix row number, or `None` for [`ConflictState::Unknown`].
  #[must_use]
  pub const fn case(self) -> Option<u8> {
    match self {
      Self::InSync => Some(1),
      Self::WorkingAhead => Some(2),
      Self::SyncAvailable => Some(3),
      Self::WorkingAheadSyncAvailable => Some(4),
      Self::WorkingDiverged => Some(5),
      Self::RemoteForcePush => Some(6),
      Self::Unknown => None,
    }
  }

  /// The machine-readable reason a refusing state carries into `Degraded`.
  #[must_use]
  pub const fn refusal_reason(self) -> Option<&'static str> {
    match self {
      Self::WorkingDiverged => Some("working-diverged"),
      Self::RemoteForcePush => Some("remote-force-push"),
      _ => None,
    }
  }

  /// Whether `switch` may evaluate the working dir in this state.
  ///
  /// True for rows 1 to 4, which is the local-wins policy: unpushed
  /// commits and a lagging sources dir are both normal.
  /// [`ConflictState::Unknown`] is permissive for the same reason - the
  /// capability's bias is that the operator's tree is what they meant to
  /// deploy, and an undecidable ancestry is not evidence against it.
  #[must_use]
  pub const fn evaluates_working_dir(self) -> bool {
    self.refusal_reason().is_none()
  }

  /// Whether the sources dir holds a sha the working dir has not seen.
  #[must_use]
  pub const fn sync_available(self) -> bool {
    matches!(self, Self::SyncAvailable | Self::WorkingAheadSyncAvailable)
  }
}

/// Place a pair of relations in the matrix.
///
/// Row 5 is checked first because the spec's "Sources vs remote" column
/// reads `any` for it: a diverged working dir is a refusal whatever the
/// remote is doing. Row 6 is next for the mirror reason.
///
/// `sources_vs_remote = Ahead` is not a matrix row. It means the sources
/// dir holds commits the recorded remote head lacks, which only happens
/// between a fetch and the state write that records it, so it is folded
/// into the `identical` column.
#[must_use]
pub const fn classify(
  working_vs_sources: RefRelation,
  sources_vs_remote: RefRelation,
) -> ConflictState {
  use RefRelation as R;

  match (working_vs_sources, sources_vs_remote) {
    (R::Diverged, _) => ConflictState::WorkingDiverged,
    (_, R::Diverged) => ConflictState::RemoteForcePush,
    (R::Unknown, _) | (_, R::Unknown) => ConflictState::Unknown,
    (R::Identical, R::Identical | R::Ahead) => ConflictState::InSync,
    (R::Ahead, R::Identical | R::Ahead) => ConflictState::WorkingAhead,
    (R::Identical | R::Behind, R::Behind) | (R::Behind, _) => {
      ConflictState::SyncAvailable
    },
    (R::Ahead, R::Behind) => ConflictState::WorkingAheadSyncAvailable,
  }
}

/// The three refs as actually observed, rather than as last recorded.
///
/// `remote_head` has no live source on the interactive path: only the
/// gitops daemon talks to the remote, so the recorded value is the
/// freshest thing available. The other two are read live, because the
/// matrix is a question about the machine now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedRefs {
  /// Last sha the daemon fetched, from the state dir.
  pub remote_head:  Option<Sha>,
  /// Live HEAD of the working dir, when one is configured.
  pub working_head: Option<Sha>,
  /// Live HEAD of the sources dir.
  pub sources_head: Option<Sha>,
}

impl ObservedRefs {
  /// Read the live heads, falling back to `recorded` for anything absent.
  #[must_use]
  pub fn observe(
    sources_dir: &Path,
    working_dir: Option<&Path>,
    recorded: &RefLineage,
  ) -> Self {
    Self {
      remote_head:  recorded.remote_head.clone(),
      working_head: working_dir
        .and_then(head_of)
        .or_else(|| recorded.working_head.clone()),
      sources_head: head_of(sources_dir)
        .or_else(|| recorded.sources_head.clone()),
    }
  }

  /// Place this observation in the matrix.
  ///
  /// An unknown ref on either side of a comparison is read as
  /// `Identical`: a machine with no gitops daemon has no `remote_head`
  /// at all, and that is the single most common state, not a conflict.
  #[must_use]
  pub fn state(
    &self,
    sources_dir: &Path,
    working_dir: Option<&Path>,
  ) -> ConflictState {
    let working_vs_sources = match (&self.working_head, &self.sources_head) {
      (Some(working), Some(sources)) => {
        // The working dir is the clone most likely to hold both objects:
        // it is the one the operator fetches and commits in.
        let repo = working_dir.unwrap_or(sources_dir);
        relate(repo, working, sources)
      },
      _ => RefRelation::Identical,
    };
    let sources_vs_remote = match (&self.sources_head, &self.remote_head) {
      (Some(sources), Some(remote)) => relate(sources_dir, sources, remote),
      _ => RefRelation::Identical,
    };
    classify(working_vs_sources, sources_vs_remote)
  }
}

// ---------------------------------------------------------------------------
// resolve_evaluation_source  -  task 2.1
// ---------------------------------------------------------------------------

/// Everything the resolver needs about one invocation.
#[derive(Debug, Clone, Copy)]
pub struct EvaluationInput<'a> {
  /// The managed clone xi owns.
  pub sources_dir: &'a Path,
  /// The operator's editable clone, when one is configured.
  pub working_dir: Option<&'a Path>,
  /// The refs as last recorded in the state dir.
  pub recorded:    &'a RefLineage,
  /// Whether the operator passed `--force-dirty`.
  pub force_dirty: bool,
  /// What to name in a refusal, e.g. `xi home switch`.
  pub what:        &'a str,
}

/// Which tree `switch` evaluates, or why it will not evaluate anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationSource {
  /// Evaluate the operator's clone. Rows 1 to 4 of the matrix.
  WorkingDir {
    /// The tree to evaluate.
    path:         PathBuf,
    /// Set when `--force-dirty` let uncommitted edits through. The
    /// resulting generation is marked with [`DIRTY_SOURCE_MARKER`].
    dirty_source: bool,
  },
  /// Evaluate xi's own clone. The no-working-dir case, where the sources
  /// dir is the single authority and conflict is undefined.
  SourcesDir {
    /// The tree to evaluate.
    path: PathBuf,
  },
  /// Refuse. Rows 5 and 6, and a dirty tree without `--force-dirty`.
  Error {
    /// The operator-facing explanation.
    reason: String,
  },
}

/// Suffix appended to the `active-generation` entry of a forced switch.
///
/// A generation built from uncommitted edits is not reproducible from
/// any sha, so the state dir has to say so; otherwise a later `xi sources
/// status` would name a sha that never produced the running system.
pub const DIRTY_SOURCE_MARKER: &str = "dirty-source";

impl EvaluationSource {
  /// The tree to evaluate, for the two non-refusing variants.
  #[must_use]
  pub fn path(&self) -> Option<&Path> {
    match self {
      Self::WorkingDir { path, .. } | Self::SourcesDir { path } => Some(path),
      Self::Error { .. } => None,
    }
  }
}

/// Decide which tree this invocation evaluates.
///
/// The order of the checks is the policy:
///
/// 1. No working dir: the sources dir is the only authority and there is
///    nothing to conflict with.
/// 2. Dirty working dir without `--force-dirty`: refuse, naming the files.
/// 3. Rows 5 and 6 of the matrix: refuse with the machine-readable reason.
/// 4. Everything else: the working dir wins.
///
/// Dirtiness is checked before the matrix on purpose. A dirty tree is
/// about the operator's own edits, which they can fix in a second; a
/// divergence needs a decision. Reporting the cheap problem first keeps
/// the message actionable.
#[must_use]
pub fn resolve_evaluation_source(
  input: &EvaluationInput<'_>,
) -> EvaluationSource {
  let Some(working_dir) = input.working_dir else {
    debug!("no working dir configured; evaluating the sources dir");
    return EvaluationSource::SourcesDir {
      path: input.sources_dir.to_path_buf(),
    };
  };

  let tree = match inspect_working_tree(working_dir) {
    Ok(tree) => tree,
    Err(err) => {
      return EvaluationSource::Error {
        reason: format!(
          "cannot read the working dir {}: {err}",
          working_dir.display()
        ),
      };
    },
  };

  if tree.is_dirty() && !input.force_dirty {
    return EvaluationSource::Error {
      reason: tree.refusal(input.what),
    };
  }

  let refs =
    ObservedRefs::observe(input.sources_dir, Some(working_dir), input.recorded);
  let state = refs.state(input.sources_dir, Some(working_dir));

  if let Some(reason) = state.refusal_reason() {
    return EvaluationSource::Error {
      reason: refusal_for(state, reason, &refs),
    };
  }

  if tree.is_dirty() {
    warn!(
      working_dir = %working_dir.display(),
      "{FORCE_DIRTY_FLAG}: evaluating uncommitted edits; the generation will \
       be marked {DIRTY_SOURCE_MARKER}"
    );
  }
  info!(
    working_dir = %working_dir.display(),
    case = state.case().unwrap_or(0),
    "evaluating the working dir"
  );
  EvaluationSource::WorkingDir {
    path:         working_dir.to_path_buf(),
    dirty_source: tree.is_dirty(),
  }
}

/// The operator-facing text for a refusing matrix row.
fn refusal_for(
  state: ConflictState,
  reason: &str,
  refs: &ObservedRefs,
) -> String {
  let unknown = "unknown";
  let working = refs.working_head.as_deref().unwrap_or(unknown);
  let sources = refs.sources_head.as_deref().unwrap_or(unknown);
  let remote = refs.remote_head.as_deref().unwrap_or(unknown);

  let explanation = match state {
    ConflictState::WorkingDiverged => {
      format!(
        "the working dir ({working}) and the sources dir ({sources}) have \
         both moved past their common ancestor.\n\nxi will not pick a winner \
         for you. Push the working dir with `git push`, or discard it and run \
         `xi <scope> sources sync`."
      )
    },
    ConflictState::RemoteForcePush => {
      format!(
        "the remote ({remote}) is not a descendant of the recorded sources \
         head ({sources}): somebody force-pushed.\n\nThe sources dir has been \
         left on {sources}. Inspect the remote, then re-adopt with `xi \
         <scope> switch <remote> --force` if the rewrite was intended."
      )
    },
    _ => String::new(),
  };

  format!("status: Degraded, reason: {reason}\n\n{explanation}")
}

// ---------------------------------------------------------------------------
// `xi <scope> sources diff`  -  section 5
// ---------------------------------------------------------------------------

/// One labelled leg of the diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSection {
  /// The spec's label, e.g. `remote → sources`.
  pub label: String,
  /// The sha the diff starts from, when known.
  pub from:  Option<Sha>,
  /// The sha the diff ends at, when known.
  pub to:    Option<Sha>,
  /// `git diff` output, empty when the leg could not be computed.
  pub body:  String,
  /// Why this leg is empty, when it is empty for a reason.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
}

/// The whole three-way (or two-way) diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffReport {
  /// One entry per leg, in the spec's order.
  pub sections: Vec<DiffSection>,
}

impl DiffReport {
  /// The `Degraded` reason when any leg failed, else `None`.
  #[must_use]
  pub fn degraded_reason(&self) -> Option<String> {
    let failed: Vec<&str> = self
      .sections
      .iter()
      .filter(|section| section.error.is_some())
      .map(|section| section.label.as_str())
      .collect();
    (!failed.is_empty())
      .then(|| format!("unreachable ref in: {}", failed.join(", ")))
  }

  /// Render the legs for a terminal, skipping the ones with no content.
  #[must_use]
  pub fn render(&self) -> String {
    let mut out = String::new();
    for section in &self.sections {
      let _ = writeln!(out, "=== {} ===", section.label);
      if let Some(error) = section.error.as_ref() {
        let _ = writeln!(out, "{error}");
      } else if section.body.trim().is_empty() {
        out.push_str("no difference\n");
      } else {
        out.push_str(&section.body);
        if !section.body.ends_with('\n') {
          out.push('\n');
        }
      }
    }
    out
  }
}

/// Outcome of a verb that either worked or named what was wrong.
///
/// Spelled with the same `status` tag as
/// [`super::SourcesStatus`] so one envelope shape covers the whole
/// `sources` group: a caller can read `.status` without knowing which
/// verb produced the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VerbStatus {
  /// The verb did what it was asked.
  Ready,
  /// The verb ran but something is wrong and the operator should know.
  Degraded {
    /// What is wrong, in user-facing terms.
    reason: String,
  },
}

impl VerbStatus {
  /// `Degraded` when `reason` is set, `Ready` otherwise.
  #[must_use]
  pub fn from_reason(reason: Option<String>) -> Self {
    reason.map_or(Self::Ready, |reason| Self::Degraded { reason })
  }
}

/// The envelope `xi <scope> sources diff --json` emits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcesDiffResponse {
  /// Envelope schema version; see [`super::SOURCES_SCHEMA_VERSION`].
  pub schema_version: u32,
  /// Whether every leg could be computed.
  #[serde(flatten)]
  pub status:         VerbStatus,
  /// One entry per leg, in the spec's order.
  pub sections:       Vec<DiffSection>,
}

impl SourcesDiffResponse {
  /// Wrap a report in the current envelope version.
  #[must_use]
  pub fn new(report: DiffReport) -> Self {
    Self {
      schema_version: super::SOURCES_SCHEMA_VERSION,
      status:         VerbStatus::from_reason(report.degraded_reason()),
      sections:       report.sections,
    }
  }
}

/// The envelope `xi <scope> sources sync --json` emits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcesSyncResponse {
  /// Envelope schema version; see [`super::SOURCES_SCHEMA_VERSION`].
  pub schema_version: u32,
  /// `Degraded` when the sync ran over uncommitted edits.
  #[serde(flatten)]
  pub status:         VerbStatus,
  /// What the sync did.
  #[serde(flatten)]
  pub outcome:        SyncOutcome,
}

impl SourcesSyncResponse {
  /// Wrap an outcome in the current envelope version.
  ///
  /// A forced sync reports `Degraded`: it succeeded, but it destroyed
  /// uncommitted work, and a caller polling status should not read that
  /// as a clean run.
  #[must_use]
  pub fn new(outcome: SyncOutcome) -> Self {
    let reason = outcome.forced.then(|| {
      format!(
        "{FORCE_DIRTY_FLAG}: uncommitted edits in the working dir were \
         overwritten and cannot be recovered"
      )
    });
    Self {
      schema_version: super::SOURCES_SCHEMA_VERSION,
      status: VerbStatus::from_reason(reason),
      outcome,
    }
  }
}

/// Diff the three refs pairwise.
///
/// With no working dir configured the report reduces to the one leg that
/// is still meaningful (`remote → sources`), which is the two-way case
/// of task 5.2 rather than a special mode.
///
/// Each leg is computed in the clone most likely to hold both objects:
/// the sources dir for the remote leg, the working dir for the rest. A
/// leg whose objects are missing records an `error` and leaves the body
/// empty instead of failing the whole report, because one unreachable
/// ref should not hide the two diffs that did work.
#[must_use]
pub fn three_way_diff(
  sources_dir: &Path,
  working_dir: Option<&Path>,
  refs: &ObservedRefs,
) -> DiffReport {
  let mut sections = vec![diff_section(
    sources_dir,
    "remote → sources",
    refs.remote_head.as_deref(),
    refs.sources_head.as_deref(),
  )];

  if let Some(working_dir) = working_dir {
    sections.push(diff_section(
      working_dir,
      "sources → working",
      refs.sources_head.as_deref(),
      refs.working_head.as_deref(),
    ));
    sections.push(diff_section(
      working_dir,
      "remote → working",
      refs.remote_head.as_deref(),
      refs.working_head.as_deref(),
    ));
  }

  DiffReport { sections }
}

fn diff_section(
  repo: &Path,
  label: &str,
  from: Option<&str>,
  to: Option<&str>,
) -> DiffSection {
  let section = |body: String, error: Option<String>| {
    DiffSection {
      label: label.to_owned(),
      from: from.map(str::to_owned),
      to: to.map(str::to_owned),
      body,
      error,
    }
  };

  let (Some(from_sha), Some(to_sha)) = (from, to) else {
    return section(
      String::new(),
      Some(format!("{label}: a ref on this leg is not known yet")),
    );
  };

  if !has_object(repo, from_sha) || !has_object(repo, to_sha) {
    return section(
      String::new(),
      Some(format!(
        "{label}: {} does not have both objects; fetch it and retry",
        repo.display()
      )),
    );
  }

  match git_text(repo, &["diff", from_sha, to_sha]) {
    Ok(body) => section(body, None),
    Err(err) => section(String::new(), Some(format!("{label}: {err}"))),
  }
}

// ---------------------------------------------------------------------------
// The hermetic sync  -  section 5a
// ---------------------------------------------------------------------------

/// One invocation of the sources-to-working copy-with-erase.
#[derive(Debug, Clone, Copy)]
pub struct SyncRequest<'a> {
  /// The authority. Read-only for the whole operation.
  pub sources_dir: &'a Path,
  /// The destination. The only tree this operation may write.
  pub working_dir: &'a Path,
  /// Whether the operator passed `--force-dirty`.
  pub force_dirty: bool,
  /// What to name in a refusal, e.g. `xi home sources sync`.
  pub what:        &'a str,
}

/// What a completed sync did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOutcome {
  /// Paths written, relative to the working dir.
  pub written:      Vec<String>,
  /// Paths deleted, relative to the working dir.
  pub deleted:      Vec<String>,
  /// The sha the working dir's HEAD now points at.
  pub sources_head: Sha,
  /// Whether uncommitted edits were overwritten under `--force-dirty`.
  pub forced:       bool,
}

/// One entry of a git tree listing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
  /// The six-digit git mode, e.g. `100644`.
  mode: String,
  /// The path, relative to the repository root.
  path: String,
}

impl TreeEntry {
  /// Whether git recorded this blob as executable.
  const fn is_executable(&self) -> bool {
    matches!(self.mode.as_bytes(), b"100755")
  }

  /// Whether this entry is a symlink or a gitlink, which the copy skips.
  const fn is_copyable_blob(&self) -> bool {
    matches!(self.mode.as_bytes(), b"100644" | b"100755")
  }
}

/// Copy the sources dir's tree at `sources_head` into the working dir.
///
/// The semantics are chezmoi's `apply`, not `git pull`:
///
/// 1. every path tracked at `sources_head` is written into the working dir,
///    replacing whatever is there,
/// 2. every path tracked at the working dir's previous HEAD but absent at
///    `sources_head` is deleted,
/// 3. nothing else is touched - not `.git`, not untracked files outside the
///    tracked set, and nothing outside the working dir at all,
/// 4. `git reset --soft <sources_head>` moves HEAD, leaving the index and any
///    staged edits intact for review.
///
/// Every write goes through [`guard_write`] against the working dir, so
/// a bug that computed a path escaping the destination aborts instead of
/// writing. The sources dir is only ever read.
///
/// # Errors
///
/// Returns [`ConflictError::Dirty`] when the working dir is dirty and
/// `force_dirty` is unset, [`ConflictError::UnknownRef`] when the
/// sources dir has no HEAD, and propagates git and filesystem failures.
pub fn sync(request: &SyncRequest<'_>) -> Result<SyncOutcome, ConflictError> {
  let tree = inspect_working_tree(request.working_dir)?;
  if tree.is_dirty() && !request.force_dirty {
    return Err(ConflictError::Dirty(tree.refusal(request.what)));
  }

  let sources_head = head_of(request.sources_dir).ok_or_else(|| {
    ConflictError::UnknownRef(format!(
      "the sources dir {} has no HEAD to sync from",
      request.sources_dir.display()
    ))
  })?;

  let wanted = ls_tree(request.sources_dir, &sources_head)?;
  let previous = match head_of(request.working_dir) {
    Some(working_head) => ls_tree(request.working_dir, &working_head)?,
    None => Vec::new(),
  };

  let written = write_pass(request, &sources_head, &wanted)?;
  let deleted = delete_pass(request, &wanted, &previous)?;

  // `--soft`, by spec: the operator keeps their index so they can
  // inspect, amend, or commit a follow-up without another fetch.
  working_dir_git(request.working_dir, &["reset", "--soft", &sources_head])?;

  info!(
    written = written.len(),
    deleted = deleted.len(),
    sources_head = %sources_head,
    "synced the sources dir into the working dir"
  );

  Ok(SyncOutcome {
    written,
    deleted,
    sources_head,
    forced: tree.is_dirty(),
  })
}

/// Write every blob tracked at `sources_head` into the working dir.
fn write_pass(
  request: &SyncRequest<'_>,
  sources_head: &str,
  wanted: &[TreeEntry],
) -> Result<Vec<String>, ConflictError> {
  let mut written = Vec::with_capacity(wanted.len());

  for entry in wanted {
    if !entry.is_copyable_blob() {
      debug!(
        path = %entry.path,
        mode = %entry.mode,
        "skipping a non-blob tree entry"
      );
      continue;
    }

    let dest = request.working_dir.join(&entry.path);
    guard_write(&dest, request.working_dir)?;

    if let Some(parent) = dest.parent() {
      guard_write(parent, request.working_dir)?;
      std::fs::create_dir_all(parent).map_err(|source| {
        ConflictError::Io {
          path: parent.to_path_buf(),
          source,
        }
      })?;
    }

    let blob = git_bytes(request.sources_dir, &[
      "show",
      &format!("{sources_head}:{}", entry.path),
    ])?;
    std::fs::write(&dest, blob).map_err(|source| {
      ConflictError::Io {
        path: dest.clone(),
        source,
      }
    })?;
    set_executable(&dest, entry.is_executable())?;

    written.push(entry.path.clone());
  }

  Ok(written)
}

/// Delete every path tracked at the old working HEAD but gone at
/// `sources_head`.
fn delete_pass(
  request: &SyncRequest<'_>,
  wanted: &[TreeEntry],
  previous: &[TreeEntry],
) -> Result<Vec<String>, ConflictError> {
  let kept: std::collections::BTreeSet<&str> =
    wanted.iter().map(|entry| entry.path.as_str()).collect();
  let mut deleted = Vec::new();

  for entry in previous {
    if kept.contains(entry.path.as_str()) {
      continue;
    }

    let dest = request.working_dir.join(&entry.path);
    guard_write(&dest, request.working_dir)?;
    match std::fs::remove_file(&dest) {
      Ok(()) => deleted.push(entry.path.clone()),
      // Already gone is the desired end state, not a failure.
      Err(err) if err.kind() == io::ErrorKind::NotFound => {},
      Err(source) => {
        return Err(ConflictError::Io { path: dest, source });
      },
    }
  }

  Ok(deleted)
}

/// Mirror git's executable bit onto the written file.
#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<(), ConflictError> {
  use std::os::unix::fs::PermissionsExt as _;

  let mode = if executable { 0o755 } else { 0o644 };
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(
    |source| {
      ConflictError::Io {
        path: path.to_path_buf(),
        source,
      }
    },
  )
}

#[cfg(not(unix))]
fn set_executable(
  _path: &Path,
  _executable: bool,
) -> Result<(), ConflictError> {
  Ok(())
}

/// The authoritative tracked-file set for a commit.
///
/// `-z` rather than newline separation: a path may legitimately contain a
/// newline, and a file list that silently splits one entry into two would
/// make the delete pass remove a file nobody asked it to.
fn ls_tree(repo: &Path, rev: &str) -> Result<Vec<TreeEntry>, ConflictError> {
  let raw = git_text(repo, &["ls-tree", "-r", "-z", rev])?;
  Ok(parse_ls_tree(&raw))
}

/// Parse `git ls-tree -r -z` records: `<mode> <type> <sha>\t<path>\0`.
fn parse_ls_tree(raw: &str) -> Vec<TreeEntry> {
  raw
    .split('\0')
    .filter(|record| !record.is_empty())
    .filter_map(|record| {
      let (meta, path) = record.split_once('\t')?;
      let mode = meta.split_whitespace().next()?;
      Some(TreeEntry {
        mode: mode.to_owned(),
        path: path.to_owned(),
      })
    })
    .collect()
}

// ---------------------------------------------------------------------------
// sync_mode  -  section 5b
// ---------------------------------------------------------------------------

/// What the daemon does after it advances the sources dir.
///
/// The daemon itself lives in `xi-sources-gitops`; this is the decision
/// it looks up, so the mapping from config to behaviour is specified and
/// tested in one place rather than inside the tick loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostAdvance {
  /// `prompt`: let status surface the available sync, write nothing.
  Announce,
  /// `auto-copy`: run the sync on the next interactive command.
  QueueSync,
  /// `never`: log the available sync and stop there.
  LogOnly,
}

/// The post-advance hook for a resolved [`SyncMode`].
#[must_use]
pub const fn post_advance(mode: SyncMode) -> PostAdvance {
  match mode {
    SyncMode::Prompt => PostAdvance::Announce,
    SyncMode::AutoCopy => PostAdvance::QueueSync,
    SyncMode::Never => PostAdvance::LogOnly,
  }
}

/// What the auto-sync attempt on an interactive command did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoSync {
  /// `sync_mode` is not `auto-copy`, or there is no working dir.
  NotRequested,
  /// The working dir already matches the sources dir.
  UpToDate,
  /// The working dir is dirty, so the sync was deferred. Logged as
  /// `sync-deferred-dirty`; the main command still runs.
  DeferredDirty,
  /// The sync ran.
  Applied(SyncOutcome),
}

/// The event name logged when auto-copy declines a dirty working dir.
pub const SYNC_DEFERRED_DIRTY: &str = "sync-deferred-dirty";

/// Run the queued sync, if `mode` asked for one and the tree allows it.
///
/// Called at the top of every interactive command. It is deliberately
/// not fatal: a deferred or failed sync must not stop the command the
/// operator actually typed, so the caller logs and carries on.
///
/// # Errors
///
/// Propagates git and filesystem failures from [`sync`]. A dirty working
/// dir is *not* an error here - it is [`AutoSync::DeferredDirty`],
/// because auto-copy's contract is to decline silently rather than to
/// fail a command the operator ran for another reason.
pub fn apply_auto_sync(
  mode: SyncMode,
  sources_dir: &Path,
  working_dir: Option<&Path>,
  what: &str,
) -> Result<AutoSync, ConflictError> {
  let (PostAdvance::QueueSync, Some(working_dir)) =
    (post_advance(mode), working_dir)
  else {
    return Ok(AutoSync::NotRequested);
  };

  let sources_head = head_of(sources_dir);
  if sources_head.is_none() || sources_head == head_of(working_dir) {
    return Ok(AutoSync::UpToDate);
  }

  if inspect_working_tree(working_dir)?.is_dirty() {
    warn!(
      event = SYNC_DEFERRED_DIRTY,
      working_dir = %working_dir.display(),
      "sync_mode = auto-copy but the working dir is dirty; leaving it alone"
    );
    return Ok(AutoSync::DeferredDirty);
  }

  let outcome = sync(&SyncRequest {
    sources_dir,
    working_dir,
    force_dirty: false,
    what,
  })?;
  Ok(AutoSync::Applied(outcome))
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

  use super::*;

  // -- 2.2 / 6.1 the six matrix cells, on pure relations ------------------

  #[test]
  fn case_1_everything_identical_is_in_sync() {
    let state = classify(RefRelation::Identical, RefRelation::Identical);
    assert_eq!(state, ConflictState::InSync);
    assert_eq!(state.case(), Some(1));
    assert!(state.evaluates_working_dir());
    assert!(!state.sync_available());
  }

  #[test]
  fn case_2_working_ahead_evaluates_the_working_dir() {
    let state = classify(RefRelation::Ahead, RefRelation::Identical);
    assert_eq!(state, ConflictState::WorkingAhead);
    assert_eq!(state.case(), Some(2));
    assert!(state.evaluates_working_dir());
    assert!(!state.sync_available());
  }

  #[test]
  fn case_3_sources_behind_remote_offers_a_sync() {
    let state = classify(RefRelation::Identical, RefRelation::Behind);
    assert_eq!(state, ConflictState::SyncAvailable);
    assert_eq!(state.case(), Some(3));
    assert!(state.evaluates_working_dir());
    assert!(state.sync_available());
  }

  #[test]
  fn case_3_also_covers_a_working_dir_behind_the_sources_dir() {
    // SC-X-008's starting state: the daemon advanced the sources dir, so
    // the working dir is now an ancestor of it.
    let state = classify(RefRelation::Behind, RefRelation::Identical);
    assert_eq!(state, ConflictState::SyncAvailable);
    assert!(state.sync_available());
  }

  #[test]
  fn case_4_working_ahead_and_sources_behind_is_still_local_wins() {
    let state = classify(RefRelation::Ahead, RefRelation::Behind);
    assert_eq!(state, ConflictState::WorkingAheadSyncAvailable);
    assert_eq!(state.case(), Some(4));
    assert!(state.evaluates_working_dir());
    assert!(state.sync_available());
  }

  #[test]
  fn case_5_working_diverged_refuses_whatever_the_remote_does() {
    for sources_vs_remote in [
      RefRelation::Identical,
      RefRelation::Ahead,
      RefRelation::Behind,
      RefRelation::Diverged,
      RefRelation::Unknown,
    ] {
      let state = classify(RefRelation::Diverged, sources_vs_remote);
      assert_eq!(
        state,
        ConflictState::WorkingDiverged,
        "row 5 reads `any` for the remote column, got {state:?}"
      );
      assert_eq!(state.case(), Some(5));
      assert!(!state.evaluates_working_dir());
      assert_eq!(state.refusal_reason(), Some("working-diverged"));
    }
  }

  #[test]
  fn case_6_remote_force_push_refuses() {
    let state = classify(RefRelation::Identical, RefRelation::Diverged);
    assert_eq!(state, ConflictState::RemoteForcePush);
    assert_eq!(state.case(), Some(6));
    assert!(!state.evaluates_working_dir());
    assert_eq!(state.refusal_reason(), Some("remote-force-push"));
  }

  #[test]
  fn undecidable_ancestry_is_its_own_state_and_stays_permissive() {
    let state = classify(RefRelation::Unknown, RefRelation::Identical);
    assert_eq!(state, ConflictState::Unknown);
    assert_eq!(state.case(), None);
    assert!(state.evaluates_working_dir());
  }

  // -- write guard --------------------------------------------------------

  #[test]
  fn a_path_under_the_root_is_allowed() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    assert!(check_write_target(&root.join("a/b/c.nix"), root).is_ok());
    assert!(check_write_target(root, root).is_ok());
  }

  #[test]
  fn a_sibling_of_the_root_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("working");
    std::fs::create_dir_all(&root).unwrap();
    let violation = check_write_target(&tmp.path().join("sources/x"), &root)
      .expect_err("a sibling tree must be rejected");
    assert_eq!(violation.allowed_root, root);
  }

  #[test]
  fn a_dot_dot_escape_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("working");
    std::fs::create_dir_all(&root).unwrap();
    let escape = root.join("../sources/evil.nix");
    check_write_target(&escape, &root)
      .expect_err("`..` must not escape the root");
  }

  #[test]
  fn a_dot_dot_that_stays_inside_is_allowed() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    assert!(check_write_target(&root.join("a/../b.nix"), root).is_ok());
  }

  #[cfg(debug_assertions)]
  #[test]
  #[should_panic(expected = "refusing to write")]
  fn the_guard_panics_in_debug_builds() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("working");
    std::fs::create_dir_all(&root).unwrap();
    drop(guard_write(&tmp.path().join("elsewhere"), &root));
  }

  // -- 5a.5 forbidden git in the working dir ------------------------------

  #[test]
  fn every_merging_subcommand_is_refused_in_the_working_dir() {
    let dir = Path::new("/tmp/working");
    for subcommand in FORBIDDEN_WORKING_DIR_GIT {
      let err = check_working_dir_git(dir, &[subcommand])
        .expect_err("{subcommand} must be refused");
      assert_eq!(err.subcommand, *subcommand);
    }
  }

  #[test]
  fn read_only_and_reset_subcommands_are_allowed() {
    let dir = Path::new("/tmp/working");
    for args in [
      vec!["status", "--porcelain=v2"],
      vec!["rev-parse", "HEAD"],
      vec!["ls-tree", "-r", "-z", "HEAD"],
      vec!["reset", "--soft", "deadbeef"],
      vec!["diff", "a", "b"],
      vec!["stash", "list"],
    ] {
      assert!(
        check_working_dir_git(dir, &args).is_ok(),
        "`git {}` must stay available",
        args.join(" ")
      );
    }
  }

  // -- porcelain v2 parsing ----------------------------------------------

  #[test]
  fn porcelain_v2_headers_are_not_dirty_paths() {
    let out = "# branch.oid deadbeef\n# branch.head main\n";
    assert!(parse_porcelain_v2(out).is_empty());
  }

  #[test]
  fn porcelain_v2_records_yield_their_paths() {
    let out = concat!(
      "# branch.head main\n",
      "1 .M N... 100644 100644 100644 aaa bbb flake.nix\n",
      "? untracked file.nix\n",
      "2 R. N... 100644 100644 100644 aaa bbb R100 new.nix\told.nix\n",
      "u UU N... 100644 100644 100644 100644 aaa bbb ccc conflicted.nix\n",
    );
    assert_eq!(parse_porcelain_v2(out), vec![
      "flake.nix".to_owned(),
      "untracked file.nix".to_owned(),
      "new.nix".to_owned(),
      "conflicted.nix".to_owned(),
    ]);
  }

  #[test]
  fn non_empty_lines_drops_blanks() {
    assert_eq!(non_empty_lines("a\n\nb\n"), vec![
      "a".to_owned(),
      "b".to_owned()
    ]);
    assert!(non_empty_lines("").is_empty());
  }

  // -- xi-5ytq: tracked_dirty_paths measures against HEAD, not the index --

  /// A repo with one commit, so every case starts from a known `HEAD`.
  fn repo_with_one_commit(tmp: &TempDir) -> PathBuf {
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    let git = |args: &[&str]| {
      let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args([
          "-c",
          "user.email=t@e",
          "-c",
          "user.name=t",
          "-c",
          "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
      assert!(out.status.success(), "git {args:?} failed: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    dir
  }

  fn git_dir_of(repo: &Path) -> PathBuf {
    repo.join(".git")
  }

  #[test]
  fn tracked_dirty_paths_is_empty_against_the_commit_it_came_from() {
    let tmp = TempDir::new().unwrap();
    let repo = repo_with_one_commit(&tmp);
    let paths = tracked_dirty_paths(&repo, &git_dir_of(&repo))
      .expect("a repo with a HEAD must answer");
    assert!(paths.is_empty(), "a freshly committed tree is not dirty");
  }

  #[test]
  fn tracked_dirty_paths_sees_a_genuine_edit() {
    let tmp = TempDir::new().unwrap();
    let repo = repo_with_one_commit(&tmp);
    std::fs::write(repo.join("a.txt"), "a2\n").unwrap();
    let paths = tracked_dirty_paths(&repo, &git_dir_of(&repo)).unwrap();
    assert_eq!(paths, vec!["a.txt".to_owned()]);
  }

  #[test]
  fn tracked_dirty_paths_sees_a_genuinely_new_untracked_file() {
    let tmp = TempDir::new().unwrap();
    let repo = repo_with_one_commit(&tmp);
    std::fs::write(repo.join("c.txt"), "c\n").unwrap();
    let paths = tracked_dirty_paths(&repo, &git_dir_of(&repo)).unwrap();
    assert_eq!(paths, vec!["c.txt".to_owned()]);
  }

  #[test]
  fn tracked_dirty_paths_sees_a_genuine_deletion() {
    let tmp = TempDir::new().unwrap();
    let repo = repo_with_one_commit(&tmp);
    std::fs::remove_file(repo.join("a.txt")).unwrap();
    let paths = tracked_dirty_paths(&repo, &git_dir_of(&repo)).unwrap();
    assert_eq!(paths, vec!["a.txt".to_owned()]);
  }

  #[test]
  fn tracked_dirty_paths_is_none_without_a_head() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("unborn");
    std::fs::create_dir_all(&repo).unwrap();
    Command::new("git")
      .arg("-C")
      .arg(&repo)
      .args(["init", "-q", "-b", "main"])
      .status()
      .unwrap();
    assert_eq!(tracked_dirty_paths(&repo, &git_dir_of(&repo)), None);
  }

  #[test]
  fn tracked_dirty_paths_does_not_touch_the_real_index() {
    let tmp = TempDir::new().unwrap();
    let repo = repo_with_one_commit(&tmp);
    std::fs::write(repo.join("a.txt"), "a2\n").unwrap();
    let real_index = repo.join(".git").join("index");
    let before = std::fs::read(&real_index).unwrap();

    drop(tracked_dirty_paths(&repo, &git_dir_of(&repo)));

    assert_eq!(
      std::fs::read(&real_index).unwrap(),
      before,
      "the temp-index comparison must not mutate the real index"
    );
    // And it cleans up after itself.
    assert!(
      std::fs::read_dir(repo.join(".git"))
        .unwrap()
        .filter_map(Result::ok)
        .all(|entry| !entry
          .file_name()
          .to_string_lossy()
          .starts_with("xi-dirty-check-index-")),
      "the temp index file must not survive the call"
    );
  }

  /// The regression this bead exists for: a `sync`-shaped `reset --soft`
  /// residue (index stuck on the pre-sync tree; `HEAD` and the working
  /// tree both on the new tree) must read as clean.
  #[test]
  fn tracked_dirty_paths_ignores_a_sync_shaped_soft_reset() {
    let tmp = TempDir::new().unwrap();
    let repo = repo_with_one_commit(&tmp);
    let git = |args: &[&str]| {
      let out = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args([
          "-c",
          "user.email=t@e",
          "-c",
          "user.name=t",
          "-c",
          "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
      assert!(out.status.success(), "git {args:?} failed: {out:?}");
      String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };

    // A second commit: add c.txt, delete b.txt, modify a.txt.
    std::fs::write(repo.join("a.txt"), "a2\n").unwrap();
    std::fs::write(repo.join("c.txt"), "c\n").unwrap();
    std::fs::remove_file(repo.join("b.txt")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "second"]);
    let new_head = git(&["rev-parse", "HEAD"]);

    // Roll back to the first commit, as if the working dir had never
    // seen the second - then play `sync`'s write/delete pass by hand.
    git(&["reset", "-q", "--hard", "HEAD~1"]);
    std::fs::write(repo.join("a.txt"), "a2\n").unwrap();
    std::fs::write(repo.join("c.txt"), "c\n").unwrap();
    std::fs::remove_file(repo.join("b.txt")).unwrap();
    git(&["reset", "--soft", &new_head]);

    // Confirm the fixture actually reproduces the bug's precondition:
    // plain `git status` is not empty.
    let status = git(&["status", "--porcelain"]);
    assert!(!status.is_empty(), "fixture did not reproduce: {status}");

    let paths = tracked_dirty_paths(&repo, &git_dir_of(&repo)).unwrap();
    assert!(
      paths.is_empty(),
      "a working tree identical to HEAD must not read as dirty, got \
       {paths:?} (raw status: {status:?})"
    );
  }

  // -- refusal message ---------------------------------------------------

  #[test]
  fn the_refusal_names_the_files_and_the_escape_hatch() {
    let tree = DirtyTree {
      paths:       vec!["flake.nix".to_owned()],
      in_progress: Vec::new(),
      stashed:     false,
    };
    let message = tree.refusal("xi home switch");
    assert!(message.contains("flake.nix"), "got: {message}");
    assert!(message.contains(FORCE_DIRTY_FLAG), "got: {message}");
    assert!(message.contains("xi home switch"), "got: {message}");
  }

  #[test]
  fn the_refusal_truncates_long_file_lists_and_counts_the_rest() {
    let paths: Vec<String> = (0..DIRTY_PATHS_SHOWN + 5)
      .map(|i| format!("f{i}.nix"))
      .collect();
    let tree = DirtyTree {
      paths,
      ..DirtyTree::default()
    };
    let message = tree.refusal("xi home switch");
    assert!(message.contains("f0.nix"));
    assert!(
      message.contains(&format!("f{}.nix", DIRTY_PATHS_SHOWN - 1)),
      "the {DIRTY_PATHS_SHOWN}th path should still be shown: {message}"
    );
    assert!(
      !message.contains(&format!("\n  f{DIRTY_PATHS_SHOWN}.nix")),
      "the list must stop at {DIRTY_PATHS_SHOWN}: {message}"
    );
    assert!(message.contains("and 5 more"), "got: {message}");
  }

  #[test]
  fn an_in_progress_merge_is_dirty_even_with_no_modified_files() {
    let tree = DirtyTree {
      in_progress: vec!["MERGE_HEAD".to_owned()],
      ..DirtyTree::default()
    };
    assert!(tree.is_dirty());
    assert!(tree.refusal("xi home switch").contains("MERGE_HEAD"));
  }

  #[test]
  fn a_non_empty_stash_is_dirty() {
    let tree = DirtyTree {
      stashed: true,
      ..DirtyTree::default()
    };
    assert!(tree.is_dirty());
  }

  #[test]
  fn nothing_dirty_is_clean() {
    assert!(!DirtyTree::default().is_dirty());
  }

  // -- ls-tree parsing ---------------------------------------------------

  #[test]
  fn ls_tree_records_carry_mode_and_path() {
    let raw = "100644 blob aaa\tflake.nix\0100755 blob \
               bbb\tscripts/run.sh\0120000 blob ccc\tlink\0";
    let entries = parse_ls_tree(raw);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].path, "flake.nix");
    assert!(!entries[0].is_executable());
    assert!(entries[0].is_copyable_blob());
    assert!(entries[1].is_executable());
    assert!(!entries[2].is_copyable_blob(), "symlinks are not copied");
  }

  #[test]
  fn a_path_with_a_newline_survives_the_nul_split() {
    let raw = "100644 blob aaa\tweird\nname.nix\0";
    assert_eq!(parse_ls_tree(raw)[0].path, "weird\nname.nix");
  }

  // -- 5b.1 / 5b.2 the sync_mode mapping ---------------------------------

  #[test]
  fn each_sync_mode_has_exactly_one_post_advance_behaviour() {
    assert_eq!(post_advance(SyncMode::Prompt), PostAdvance::Announce);
    assert_eq!(post_advance(SyncMode::AutoCopy), PostAdvance::QueueSync);
    assert_eq!(post_advance(SyncMode::Never), PostAdvance::LogOnly);
  }

  #[test]
  fn prompt_is_the_default_mode() {
    assert_eq!(SyncMode::default(), SyncMode::Prompt);
    assert_eq!(post_advance(SyncMode::default()), PostAdvance::Announce);
  }

  // -- 2.1 resolver, the no-working-dir branch ----------------------------

  #[test]
  fn with_no_working_dir_the_sources_dir_is_the_authority() {
    let tmp = TempDir::new().unwrap();
    let sources_dir = tmp.path().join("sources");
    let refs = RefLineage::default();
    let got = resolve_evaluation_source(&EvaluationInput {
      sources_dir: &sources_dir,
      working_dir: None,
      recorded:    &refs,
      force_dirty: false,
      what:        "xi home switch",
    });
    assert_eq!(got, EvaluationSource::SourcesDir { path: sources_dir });
  }

  #[test]
  fn an_unreadable_working_dir_is_an_error_not_a_silent_fallback() {
    let tmp = TempDir::new().unwrap();
    let not_a_repo = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&not_a_repo).unwrap();
    let refs = RefLineage::default();
    let got = resolve_evaluation_source(&EvaluationInput {
      sources_dir: &tmp.path().join("sources"),
      working_dir: Some(&not_a_repo),
      recorded:    &refs,
      force_dirty: false,
      what:        "xi home switch",
    });
    match got {
      EvaluationSource::Error { reason } => {
        assert!(reason.contains("cannot read the working dir"), "{reason}");
      },
      other => panic!("expected Error, got {other:?}"),
    }
  }

  // -- diff report -------------------------------------------------------

  #[test]
  fn an_unknown_ref_degrades_only_its_own_leg() {
    let tmp = TempDir::new().unwrap();
    let report = three_way_diff(tmp.path(), None, &ObservedRefs::default());
    assert_eq!(report.sections.len(), 1, "two-way diff has one leg");
    assert_eq!(report.sections[0].label, "remote → sources");
    assert!(report.degraded_reason().is_some());
  }

  #[test]
  fn a_report_with_no_failures_is_not_degraded() {
    let report = DiffReport {
      sections: vec![DiffSection {
        label: "remote → sources".to_owned(),
        from:  Some("a".to_owned()),
        to:    Some("b".to_owned()),
        body:  String::new(),
        error: None,
      }],
    };
    assert!(report.degraded_reason().is_none());
    assert!(report.render().contains("no difference"));
  }

  // -- 5a.5, the lint half -----------------------------------------------

  /// A one-file crate tree, so each case scans only its own source.
  fn lint_tree(source: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let crate_dir = tmp.path().join("some-crate/src");
    std::fs::create_dir_all(&crate_dir).unwrap();
    std::fs::write(crate_dir.join("lib.rs"), source).unwrap();
    tmp
  }

  #[test]
  fn the_lint_rejects_a_merging_subcommand_aimed_at_the_working_dir() {
    let tmp = lint_tree(
      "fn tick(working_dir: &Path) {\n  let _ = \
       Command::new(\"git\").arg(\"-C\").arg(working_dir).args([\"pull\", \
       \"--ff-only\"]);\n}\n",
    );

    let found = lint::scan_workspace(tmp.path()).unwrap();
    assert_eq!(found.len(), 1, "expected one violation, got {found:?}");
    assert_eq!(found[0].subcommand, "pull");
    assert!(found[0].to_string().contains("aimed at the working dir"));
  }

  #[test]
  fn the_lint_catches_every_forbidden_subcommand() {
    for subcommand in FORBIDDEN_WORKING_DIR_GIT {
      let tmp = lint_tree(&format!(
        "fn tick() {{\n  working_dir_git(&[\"{subcommand}\"]);\n}}\n"
      ));
      let found = lint::scan_workspace(tmp.path()).unwrap();
      assert_eq!(
        found.len(),
        1,
        "`git {subcommand}` slipped through: {found:?}"
      );
    }
  }

  #[test]
  fn the_lint_allows_a_merging_subcommand_in_the_sources_dir() {
    let tmp = lint_tree(
      "fn tick(sources_dir: &Path) {\n  run_git(sources_dir, &[\"fetch\", \
       \"origin\"]);\n}\n",
    );
    assert!(lint::scan_workspace(tmp.path()).unwrap().is_empty());
  }

  #[test]
  fn the_lint_allows_the_word_outside_an_argument_vector() {
    let tmp = lint_tree(
      "fn tick(working_dir: &Path) {\n  // never pull into the working dir\n  \
       let _fetch_head = working_dir.join(\"FETCH_HEAD\");\n}\n",
    );
    assert!(lint::scan_workspace(tmp.path()).unwrap().is_empty());
  }

  #[test]
  fn the_lint_stops_at_the_cfg_test_boundary() {
    let tmp = lint_tree(
      "fn tick() {}\n\n#[cfg(test)]\nmod tests {\n  fn fixture(working_dir: \
       &Path) {\n    git(working_dir, &[\"fetch\"]);\n  }\n}\n",
    );
    assert!(
      lint::scan_workspace(tmp.path()).unwrap().is_empty(),
      "a fixture modelling the operator's own git is not xi's code path"
    );
  }

  /// The lint is only worth having if it runs against the real tree.
  #[test]
  fn the_real_workspace_has_no_working_dir_merge() {
    // `crates/` from this crate's manifest dir.
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
      .parent()
      .expect("crates/ is the manifest dir's parent");

    let found = lint::scan_workspace(crates_dir).expect("scan the workspace");
    assert!(
      found.is_empty(),
      "every working-dir git call must go through working_dir_git:\n{}",
      found
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
    );
  }
}
