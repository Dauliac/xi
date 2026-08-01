use std::{
  path::{Path, PathBuf},
  process::Command,
  time::Instant,
};

use color_eyre::eyre::Result;

use crate::{
  args::StageArgs,
  schema::{
    Diagnostic, Envelope, FlakeReference, GitStatus, StageEntry, StagePayload,
    elapsed_ms, emit,
  },
};

pub const CMD: &str = "stage";

pub fn run(_args: StageArgs) -> Result<()> {
  let start = Instant::now();
  match collect() {
    Ok(payload) => emit(&Envelope::ok(CMD, payload, elapsed_ms(start)))?,
    Err(diag) => {
      let payload = StagePayload {
        entries: Vec::new(),
        clean:   true,
      };
      emit(&Envelope::partial(
        CMD,
        payload,
        elapsed_ms(start),
        vec![diag],
      ))?;
    },
  }
  Ok(())
}

pub fn collect() -> Result<StagePayload, Diagnostic> {
  let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
  let output = Command::new("git")
    .args(["status", "--porcelain=v1", "-uall"])
    .current_dir(&cwd)
    .output()
    .map_err(|e| {
      Diagnostic::new("git.spawn-failed", e.to_string()).with_source("git")
    })?;
  if !output.status.success() {
    return Err(
      Diagnostic::new(
        "git.status-failed",
        String::from_utf8_lossy(&output.stderr).into_owned(),
      )
      .with_source("git"),
    );
  }
  let stdout = String::from_utf8_lossy(&output.stdout);

  let mut entries: Vec<StageEntry> = Vec::new();
  for line in stdout.lines() {
    if line.len() < 4 {
      continue;
    }
    let x = line.as_bytes()[0];
    let y = line.as_bytes()[1];
    let path = line[3..].trim();
    let (git_status, staged) = classify(x, y);
    let entry_path = PathBuf::from(path);
    let referenced_by = flake_references_for(&cwd, &entry_path);
    entries.push(StageEntry {
      path: entry_path,
      git_status,
      staged,
      referenced_by,
    });
  }

  entries.sort_by(|a, b| a.path.cmp(&b.path));
  let clean = entries.is_empty();
  Ok(StagePayload { entries, clean })
}

fn classify(x: u8, y: u8) -> (GitStatus, bool) {
  match (x, y) {
    (b'?', b'?') => (GitStatus::Untracked, false),
    (b'!', b'!') => (GitStatus::Ignored, false),
    (b'D', _) | (_, b'D') => (GitStatus::Deleted, x != b' '),
    (b'R', _) | (_, b'R') => (GitStatus::Renamed, x != b' '),
    _ => (GitStatus::Modified, x != b' ' && x != b'?'),
  }
}

/// Best-effort scan: report `flake.nix` as a referrer when it mentions
/// this path literally. Cheap and deterministic; not a full evaluator.
fn flake_references_for(cwd: &Path, path: &Path) -> Vec<FlakeReference> {
  let flake = cwd.join("flake.nix");
  let Ok(contents) = std::fs::read_to_string(&flake) else {
    return Vec::new();
  };
  let target = path.to_string_lossy();
  if contents.contains(target.as_ref()) {
    vec![FlakeReference {
      from: PathBuf::from("flake.nix"),
      attr: None,
    }]
  } else {
    Vec::new()
  }
}
