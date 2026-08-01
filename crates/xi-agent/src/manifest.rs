use std::{
  collections::{BTreeMap, HashSet, VecDeque},
  path::{Path, PathBuf},
  time::Instant,
};

use color_eyre::eyre::Result;

use crate::{
  args::ManifestArgs,
  schema::{
    Diagnostic, Envelope, ManifestEntry, ManifestKind, ManifestPayload,
    elapsed_ms, emit,
  },
};

pub const CMD: &str = "manifest";

pub fn run(_args: ManifestArgs) -> Result<()> {
  let start = Instant::now();
  match collect() {
    Ok(payload) => emit(&Envelope::ok(CMD, payload, elapsed_ms(start)))?,
    Err(diag) => {
      let payload = ManifestPayload {
        root:  PathBuf::from("flake.nix"),
        files: Vec::new(),
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

pub fn collect() -> Result<ManifestPayload, Diagnostic> {
  let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
  let root = cwd.join("flake.nix");
  if !root.exists() {
    return Err(
      Diagnostic::new("workspace.not-a-flake", "flake.nix not found in cwd")
        .with_hint("run xi agent from a flake root"),
    );
  }

  // BFS through `.nix` files following any `./relative.nix` reference and
  // any bare `path` literal that resolves to a file on disk.
  let mut queue: VecDeque<PathBuf> = VecDeque::new();
  let mut seen: HashSet<PathBuf> = HashSet::new();
  let mut edges: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();

  queue.push_back(root.clone());
  seen.insert(root.clone());

  while let Some(file) = queue.pop_front() {
    let Ok(text) = std::fs::read_to_string(&file) else {
      continue;
    };
    let base = file.parent().unwrap_or(&cwd);
    for referenced in extract_nix_paths(&text, base) {
      let canonical = canonicalize_or_keep(&referenced);
      edges
        .entry(canonical.clone())
        .or_default()
        .push(file.clone());
      if seen.insert(canonical.clone()) {
        queue.push_back(canonical);
      }
    }
  }

  let cwd_ref = cwd.as_path();
  let mut files: Vec<ManifestEntry> = seen
    .into_iter()
    .map(|abs| {
      let rel = abs.strip_prefix(cwd_ref).unwrap_or(&abs).to_path_buf();
      let imported_by = edges
        .get(&abs)
        .map(|list| {
          list
            .iter()
            .map(|p| {
              p.strip_prefix(cwd_ref).unwrap_or(p).to_path_buf()
            })
            .collect()
        })
        .unwrap_or_default();
      let kind = infer_kind(&rel);
      ManifestEntry {
        path: rel,
        imported_by,
        kind,
      }
    })
    .collect();

  files.sort_by(|a, b| a.path.cmp(&b.path));
  Ok(ManifestPayload {
    root: PathBuf::from("flake.nix"),
    files,
  })
}

fn extract_nix_paths(text: &str, base: &Path) -> Vec<PathBuf> {
  let mut out = Vec::new();
  // Match `./foo` or `./foo.nix` or `../foo/bar.nix` — a leading dot,
  // slash, then a run of path characters. Kept purely lexical; no
  // evaluator involved.
  let mut i = 0;
  let bytes = text.as_bytes();
  while i < bytes.len() {
    let c = bytes[i];
    if c == b'.'
      && i + 1 < bytes.len()
      && (bytes[i + 1] == b'/' || bytes[i + 1] == b'.')
    {
      let start = i;
      while i < bytes.len() {
        let b = bytes[i];
        let is_path = b.is_ascii_alphanumeric()
          || matches!(b, b'.' | b'/' | b'_' | b'-');
        if !is_path {
          break;
        }
        i += 1;
      }
      let raw = &text[start..i];
      let candidate = base.join(raw);
      let with_nix = if candidate.extension().is_none() {
        candidate.with_extension("nix")
      } else {
        candidate.clone()
      };
      if with_nix.exists() && with_nix.is_file() {
        out.push(with_nix);
      } else if candidate.exists() && candidate.is_file() {
        out.push(candidate);
      }
      continue;
    }
    i += 1;
  }
  out
}

fn canonicalize_or_keep(p: &Path) -> PathBuf {
  std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn infer_kind(rel: &Path) -> ManifestKind {
  let s = rel.to_string_lossy();
  if s == "flake.nix" {
    ManifestKind::FlakeRoot
  } else if s.contains("modules/") {
    ManifestKind::Module
  } else if s.contains("overlay") {
    ManifestKind::Overlay
  } else if s.contains("lib/") {
    ManifestKind::Lib
  } else if s.contains("package") || s.contains("pkgs/") {
    ManifestKind::Package
  } else {
    ManifestKind::Other
  }
}
