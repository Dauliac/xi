use std::{
  io::Write as _,
  path::{Path, PathBuf},
  time::Instant,
};

use color_eyre::eyre::{Result, eyre};
use directories::BaseDirs;
use include_dir::{Dir, DirEntry};
use sha2::{Digest, Sha256};

use crate::{
  SKILLS,
  args::{InstallArgs, InstallScope, InstallTarget},
  schema::{
    Diagnostic, Envelope, InstallAction, InstallEntry, InstallPayload, elapsed_ms, emit,
  },
};

pub const CMD: &str = "install";

pub fn run(args: InstallArgs) -> Result<()> {
  let start = Instant::now();
  let base_dirs = BaseDirs::new().ok_or_else(|| eyre!("cannot resolve base dirs"))?;
  let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

  let mut entries: Vec<InstallEntry> = Vec::new();
  let mut diags: Vec<Diagnostic> = Vec::new();

  let targets = match args.target {
    InstallTarget::All => vec![
      TargetSpec { runtime: "claude-code", subdir: ".claude/skills" },
      TargetSpec { runtime: "codex",       subdir: ".codex/skills" },
    ],
    InstallTarget::ClaudeCode => {
      vec![TargetSpec { runtime: "claude-code", subdir: ".claude/skills" }]
    },
    InstallTarget::Codex => {
      vec![TargetSpec { runtime: "codex", subdir: ".codex/skills" }]
    },
  };

  let base = match args.scope {
    InstallScope::User => base_dirs.home_dir().to_path_buf(),
    InstallScope::Project => cwd.clone(),
  };

  for tgt in &targets {
    let root = base.join(tgt.subdir);
    if let Err(e) = std::fs::create_dir_all(&root) {
      diags.push(
        Diagnostic::new("install.mkdir-failed", e.to_string())
          .with_source(root.display().to_string()),
      );
      continue;
    }
    for skill_dir in SKILLS.dirs() {
      install_skill(skill_dir, &root, tgt.runtime, args.force, args.dry_run, &mut entries)?;
    }
  }

  let payload = InstallPayload {
    scope:   match args.scope {
      InstallScope::User => "user".into(),
      InstallScope::Project => "project".into(),
    },
    target:  match args.target {
      InstallTarget::All => "all".into(),
      InstallTarget::ClaudeCode => "claude-code".into(),
      InstallTarget::Codex => "codex".into(),
    },
    entries,
  };
  let env = if diags.is_empty() {
    Envelope::ok(CMD, payload, elapsed_ms(start))
  } else {
    Envelope::partial(CMD, payload, elapsed_ms(start), diags)
  };
  emit(&env)?;
  Ok(())
}

struct TargetSpec {
  runtime: &'static str,
  subdir:  &'static str,
}

fn install_skill(
  dir: &Dir<'_>,
  root: &Path,
  runtime: &str,
  force: bool,
  dry_run: bool,
  out: &mut Vec<InstallEntry>,
) -> Result<()> {
  let Some(skill_name) = dir.path().file_name().and_then(|s| s.to_str()) else {
    return Ok(());
  };
  let skill_root = root.join(skill_name);
  if !dry_run
    && let Err(e) = std::fs::create_dir_all(&skill_root)
  {
    return Err(eyre!("mkdir {}: {}", skill_root.display(), e));
  }

  for entry in dir.entries() {
    if let DirEntry::File(f) = entry {
      let rel = f.path().strip_prefix(dir.path()).unwrap_or(f.path());
      let dst = skill_root.join(rel);
      let bytes = f.contents();
      let sha = sha256_hex(bytes);
      let action = write_if_needed(&dst, bytes, &sha, force, dry_run)?;
      let entry = InstallEntry {
        skill:  format!("{runtime}/{skill_name}"),
        path:   dst,
        action,
        sha256: sha,
      };
      out.push(entry);
    }
  }
  Ok(())
}

fn write_if_needed(
  path: &Path,
  content: &[u8],
  new_sha: &str,
  force: bool,
  dry_run: bool,
) -> Result<InstallAction> {
  if !force
    && let Ok(existing) = std::fs::read(path)
  {
    let existing_sha = sha256_hex(&existing);
    if existing_sha == new_sha {
      return Ok(InstallAction::UpToDate);
    }
  }
  if dry_run {
    return Ok(InstallAction::Wrote);
  }
  let parent = path.parent().ok_or_else(|| eyre!("no parent for {}", path.display()))?;
  std::fs::create_dir_all(parent)?;
  let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
  tmp.write_all(content)?;
  tmp.flush()?;
  tmp.persist(path).map_err(|e| eyre!("persist: {}", e.error))?;
  Ok(InstallAction::Wrote)
}

fn sha256_hex(bytes: &[u8]) -> String {
  let mut hasher = Sha256::new();
  hasher.update(bytes);
  format!("{:x}", hasher.finalize())
}
