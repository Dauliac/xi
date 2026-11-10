use std::{
  collections::{HashMap, HashSet},
  path::Path,
  thread,
  time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::{Result, eyre::bail};
use tracing::debug;
use xi_core::flake_output::FlakeOutput;
use yansi::{Color, Paint};

use crate::{
  args::DoctorArgs,
  project_config::ProjectDoctorConfig,
  resolve_local_flake_dir,
};

// ---------------------------------------------------------------------------
// Supported nixpkgs branches (built-in defaults)
// ---------------------------------------------------------------------------

const DEFAULT_SUPPORTED_BRANCHES: &[&str] = &[
  "nixos-25.05",
  "nixos-25.05-small",
  "nixos-24.11",
  "nixos-24.11-small",
  "nixos-unstable",
  "nixos-unstable-small",
  "nixpkgs-unstable",
  "nixpkgs-25.05-darwin",
  "nixpkgs-24.11-darwin",
];

const DEFAULT_MAX_INPUT_AGE_DAYS: u64 = 30;
const SECONDS_PER_DAY: u64 = 86400;

// ---------------------------------------------------------------------------
// Check result types
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[allow(dead_code)]
pub enum CheckStatus {
  Ok(String),
  Warn(String),
  Fail(String),
}

#[derive(Debug)]
pub struct CheckResult {
  /// Stable, kebab-case identifier used to look this check up in
  /// `.xi.toml`'s `[doctor.rules]` table (e.g. `rules.dirty-tree = "off"`).
  /// Never changes across releases once assigned -- it is a public,
  /// documented config key, not a display label.
  pub code:   &'static str,
  pub name:   &'static str,
  pub status: CheckStatus,
}

impl CheckResult {
  fn print(&self) {
    match &self.status {
      CheckStatus::Ok(msg) => {
        println!(
          "    {} {} [{}]",
          Paint::new(self.name).bold(),
          Paint::new(format!("ok — {msg}")).fg(Color::Green),
          self.code,
        );
      },
      CheckStatus::Warn(msg) => {
        println!(
          "    {} {} [{}]",
          Paint::new(self.name).bold(),
          Paint::new(format!("warn — {msg}")).fg(Color::Yellow),
          self.code,
        );
      },
      CheckStatus::Fail(msg) => {
        println!(
          "    {} {} [{}]",
          Paint::new(self.name).bold(),
          Paint::new(format!("FAIL — {msg}")).fg(Color::Red),
          self.code,
        );
      },
    }
  }

  pub const fn is_warn(&self) -> bool {
    matches!(self.status, CheckStatus::Warn(_))
  }

  pub const fn is_fail(&self) -> bool {
    matches!(self.status, CheckStatus::Fail(_))
  }
}

// ---------------------------------------------------------------------------
// Flake lock parsing (minimal, no external dep)
// ---------------------------------------------------------------------------

/// A direct flake input with its locked metadata.
#[derive(Debug)]
struct FlakeInput {
  name:             String,
  owner:            Option<String>,
  repo:             Option<String>,
  git_ref:          Option<String>,
  last_modified:    Option<u64>,
  #[allow(dead_code)]
  source_type:      Option<String>,
  /// Whether the input's `original` node in `flake.lock` carried an
  /// explicit `rev` key, i.e. the input was pinned directly to a commit
  /// hash (e.g. `github:owner/repo/abc123...`).
  ///
  /// This is **not** sufficient to detect tag pins: a tag pin
  /// (`github:owner/repo/v1.2.3`) stores the tag only in `original.ref`,
  /// not `original.rev`  -  Nix never sets `original.rev` for tag-shaped
  /// refs, only for explicit commit hashes. See `check_floating_input_refs`
  /// / `is_tag_shaped_ref` for the heuristic that also recognizes
  /// tag-shaped `ref`s as pinned.
  has_explicit_rev: bool,
}

/// Parse flake.lock and extract direct (root-level) inputs.
fn parse_flake_lock(lock_path: &Path) -> Result<Vec<FlakeInput>> {
  let content = std::fs::read_to_string(lock_path)?;
  let lock: serde_json::Value = serde_json::from_str(&content)?;

  let nodes = lock
    .get("nodes")
    .and_then(serde_json::Value::as_object)
    .ok_or_else(|| {
      color_eyre::eyre::eyre!("Invalid flake.lock: missing nodes")
    })?;

  let root_name = lock
    .get("root")
    .and_then(serde_json::Value::as_str)
    .unwrap_or("root");

  let root_node = nodes
    .get(root_name)
    .and_then(serde_json::Value::as_object)
    .ok_or_else(|| {
      color_eyre::eyre::eyre!("Invalid flake.lock: missing root node")
    })?;

  let root_inputs = root_node
    .get("inputs")
    .and_then(serde_json::Value::as_object);

  let Some(root_inputs) = root_inputs else {
    return Ok(vec![]);
  };

  let mut inputs = Vec::new();

  for (input_name, target) in root_inputs {
    // Target can be a string (node name) or array (follows)
    let node_name = match target {
      serde_json::Value::String(s) => s.as_str(),
      serde_json::Value::Array(arr) => {
        // "follows" reference — resolve the chain
        arr.last().and_then(serde_json::Value::as_str).unwrap_or("")
      },
      _ => continue,
    };

    let Some(node) =
      nodes.get(node_name).and_then(serde_json::Value::as_object)
    else {
      continue;
    };

    let locked = node.get("locked").and_then(serde_json::Value::as_object);
    let original = node.get("original").and_then(serde_json::Value::as_object);

    inputs.push(FlakeInput {
      name:             input_name.clone(),
      owner:            locked
        .and_then(|l| l.get("owner"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      repo:             locked
        .and_then(|l| l.get("repo"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      git_ref:          original
        .and_then(|o| o.get("ref"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      last_modified:    locked
        .and_then(|l| l.get("lastModified"))
        .and_then(serde_json::Value::as_u64),
      source_type:      locked
        .and_then(|l| l.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      has_explicit_rev: original
        .and_then(|o| o.get("rev"))
        .and_then(serde_json::Value::as_str)
        .is_some(),
    });
  }

  Ok(inputs)
}

// ---------------------------------------------------------------------------
// Full flake.lock graph (all nodes, for duplicate-input detection)
// ---------------------------------------------------------------------------

/// Where a lock node's named input resolves to.
#[derive(Debug, Clone)]
enum LockInputTarget {
  /// A direct reference to another node, by name.
  Direct(String),
  /// A `follows` reference: a path of input names, walked from the root,
  /// per the same string-vs-array shape `parse_flake_lock` already
  /// handles for root-level inputs.
  Follows(Vec<String>),
}

/// One node from `flake.lock`'s `nodes` map, with just enough locked
/// metadata and outgoing edges to detect un-followed duplicates.
#[derive(Debug)]
struct LockNodeInfo {
  owner:  Option<String>,
  repo:   Option<String>,
  rev:    Option<String>,
  inputs: Vec<(String, LockInputTarget)>,
}

/// The full node graph of a `flake.lock`, keyed by node name.
#[derive(Debug)]
struct LockGraph {
  nodes: HashMap<String, LockNodeInfo>,
  root:  String,
}

/// Parse *every* node in `flake.lock` (not just the root's direct
/// inputs), for checks that need to see transitively-pulled inputs  -
/// currently only [`check_duplicate_inputs`].
fn parse_all_lock_nodes(lock_path: &Path) -> Result<LockGraph> {
  let content = std::fs::read_to_string(lock_path)?;
  let lock: serde_json::Value = serde_json::from_str(&content)?;

  let nodes_json = lock
    .get("nodes")
    .and_then(serde_json::Value::as_object)
    .ok_or_else(|| {
      color_eyre::eyre::eyre!("Invalid flake.lock: missing nodes")
    })?;

  let root = lock
    .get("root")
    .and_then(serde_json::Value::as_str)
    .unwrap_or("root")
    .to_string();

  let mut nodes = HashMap::new();

  for (node_name, node) in nodes_json {
    let Some(node) = node.as_object() else {
      continue;
    };
    let locked = node.get("locked").and_then(serde_json::Value::as_object);

    let mut inputs = Vec::new();
    if let Some(node_inputs) =
      node.get("inputs").and_then(serde_json::Value::as_object)
    {
      for (input_name, target) in node_inputs {
        let target = match target {
          serde_json::Value::String(s) => LockInputTarget::Direct(s.clone()),
          serde_json::Value::Array(arr) => {
            LockInputTarget::Follows(
              arr
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect(),
            )
          },
          _ => continue,
        };
        inputs.push((input_name.clone(), target));
      }
    }

    nodes.insert(node_name.clone(), LockNodeInfo {
      owner: locked
        .and_then(|l| l.get("owner"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      repo: locked
        .and_then(|l| l.get("repo"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      rev: locked
        .and_then(|l| l.get("rev"))
        .and_then(serde_json::Value::as_str)
        .map(String::from),
      inputs,
    });
  }

  Ok(LockGraph { nodes, root })
}

/// Resolve a `follows` path (a sequence of input names walked from the
/// root) to the final node name it points at.
fn resolve_follows_path(graph: &LockGraph, path: &[String]) -> Option<String> {
  let mut current = graph.root.clone();
  for segment in path {
    let node = graph.nodes.get(&current)?;
    let target = node.inputs.iter().find(|(name, _)| name == segment)?;
    match &target.1 {
      LockInputTarget::Direct(name) => current = name.clone(),
      // Nested follows: per the lock format, an array is always a path
      // from the root, so resolve it the same way rather than relative
      // to `current`.
      LockInputTarget::Follows(sub_path) => {
        current = resolve_follows_path(graph, sub_path)?;
      },
    }
  }
  Some(current)
}

/// Every node name that is the resolve-target of at least one `follows`
/// entry anywhere in the lock.
///
/// Heuristic limitation: this only checks whether a node is followed
/// *somewhere* in the lock, not whether the specific follows edge
/// originates from the other half of a divergent pair or a shared
/// ancestor of both  -  a flake with many unrelated follows chains could
/// under-warn as a result. See proposal.md Open Question 2.
fn collect_followed_targets(graph: &LockGraph) -> HashSet<String> {
  let mut followed = HashSet::new();
  for node in graph.nodes.values() {
    for (_, target) in &node.inputs {
      if let LockInputTarget::Follows(path) = target
        && let Some(resolved) = resolve_follows_path(graph, path)
      {
        followed.insert(resolved);
      }
    }
  }
  followed
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

/// Check if a nixpkgs input is on a supported branch.
fn check_nixpkgs_branch(
  input: &FlakeInput,
  supported: &[String],
) -> Option<CheckResult> {
  if !is_nixpkgs_input(input) {
    return None;
  }

  let Some(ref git_ref) = input.git_ref else {
    return Some(CheckResult {
      code:   "nixpkgs-branch",
      name:   "Nixpkgs branch",
      status: CheckStatus::Warn(format!(
        "input '{}' has no branch ref",
        input.name
      )),
    });
  };

  if supported.iter().any(|b| b == git_ref) {
    Some(CheckResult {
      code:   "nixpkgs-branch",
      name:   "Nixpkgs branch",
      status: CheckStatus::Ok(format!("{} ({})", input.name, git_ref)),
    })
  } else {
    Some(CheckResult {
      code:   "nixpkgs-branch",
      name:   "Nixpkgs branch",
      status: CheckStatus::Warn(format!(
        "input '{}' uses branch '{}' which is not in the supported list",
        input.name, git_ref
      )),
    })
  }
}

/// Check if an input is stale (older than threshold).
fn check_input_freshness(
  input: &FlakeInput,
  max_age_days: u64,
) -> Option<CheckResult> {
  let last_modified = input.last_modified?;

  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs();

  let age_days = now.saturating_sub(last_modified) / SECONDS_PER_DAY;

  if age_days <= max_age_days {
    Some(CheckResult {
      code:   "input-freshness",
      name:   "Input freshness",
      status: CheckStatus::Ok(format!(
        "{} — {} days old (threshold: {})",
        input.name, age_days, max_age_days
      )),
    })
  } else {
    Some(CheckResult {
      code:   "input-freshness",
      name:   "Input freshness",
      status: CheckStatus::Warn(format!(
        "input '{}' is {} days old (threshold: {}). Run `xi update` to \
         refresh.",
        input.name, age_days, max_age_days
      )),
    })
  }
}

/// Check if a nixpkgs input comes from the official NixOS org.
fn check_nixpkgs_owner(input: &FlakeInput) -> Option<CheckResult> {
  if !is_nixpkgs_input(input) {
    return None;
  }

  let owner = input.owner.as_ref()?;

  if owner == "NixOS" {
    Some(CheckResult {
      code:   "nixpkgs-owner",
      name:   "Nixpkgs source",
      status: CheckStatus::Ok(format!(
        "{} (github:{}/{})",
        input.name,
        owner,
        input.repo.as_deref().unwrap_or("nixpkgs")
      )),
    })
  } else {
    Some(CheckResult {
      code:   "nixpkgs-owner",
      name:   "Nixpkgs source",
      status: CheckStatus::Warn(format!(
        "input '{}' uses fork from '{}' instead of NixOS",
        input.name, owner
      )),
    })
  }
}

/// Check if a flake formatter is declared.
fn check_formatter(flake_ref: &str) -> CheckResult {
  let system = crate::current_nix_system();
  let output = nix_command::NixCommand::new(nix_command::CommandKind::Eval)
    .arg(format!("{flake_ref}#formatter.{system}"))
    .arg("--apply")
    .arg("_: true")
    .arg("--json")
    .output();

  match output {
    Ok(o) if o.status.success() => {
      CheckResult {
        code:   "formatter",
        name:   "Formatter",
        status: CheckStatus::Ok("declared".to_string()),
      }
    },
    _ => {
      CheckResult {
        code:   "formatter",
        name:   "Formatter",
        status: CheckStatus::Warn(
          "no formatter declared. Consider adding one or using \
           `xi.formatter.enable` in the xi flake-parts module."
            .to_string(),
        ),
      }
    },
  }
}

/// Heuristic: is this input a nixpkgs input?
fn is_nixpkgs_input(input: &FlakeInput) -> bool {
  let name_match = input.name.contains("nixpkgs");
  let repo_match = input.repo.as_deref().is_some_and(|r| r == "nixpkgs");
  name_match || repo_match
}

/// Heuristic: does `git_ref` look like an immutable version tag (e.g.
/// `v1.2.3`, `1.36.0`, `0.2.0.0`) rather than a floating branch name
/// (e.g. `main`, `master`, `nixos-unstable`, `release-1.0`)?
///
/// `flake.lock`'s `original.rev` is only populated when the input is
/// pinned to an explicit commit hash (`github:owner/repo/<40-char-sha>`).
/// A tag pin (`github:owner/repo/v1.2.3`) stores the tag *only* in
/// `original.ref`, in exactly the same shape as a branch pin  -  Nix does
/// not distinguish tag-shaped refs from branch-shaped ones in
/// `flake.lock`. So `has_explicit_rev` alone cannot detect tag pins;
/// `check_floating_input_refs` also runs this heuristic over the `ref`
/// string itself to recognize version-tag-shaped refs as pinned.
fn is_tag_shaped_ref(git_ref: &str) -> bool {
  let rest = git_ref.strip_prefix('v').unwrap_or(git_ref);
  !rest.is_empty()
    && rest.bytes().any(|b| b.is_ascii_digit())
    && rest.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

/// Check that a flake declares a floating-ref-free, pinned input.
///
/// Warns when the input's declared (`original`) ref names a branch (no
/// `rev=`/tag) rather than an immutable reference  -  independent of
/// [`check_input_freshness`], which only looks at how old the *currently
/// locked* revision is, not whether the next `nix flake update` could
/// silently move it.
fn check_floating_input_refs(input: &FlakeInput) -> Option<CheckResult> {
  let git_ref = input.git_ref.as_ref()?;

  if input.has_explicit_rev || is_tag_shaped_ref(git_ref) {
    return None;
  }

  Some(CheckResult {
    code:   "floating-input-ref",
    name:   "Floating input ref",
    status: CheckStatus::Warn(format!(
      "input '{}' is pinned to floating ref '{}' with no explicit rev  -  it \
       will drift on the next `nix flake update`",
      input.name, git_ref
    )),
  })
}

/// Check `flake.lock`'s divergent-rev duplicate inputs: nodes sharing the
/// same `(locked.owner, locked.repo)` but locked at different revisions,
/// with no `follows` connecting them.
///
/// This is deliberately heuristic (see proposal.md Open Question 2): it
/// groups purely on `(owner, repo)`, exactly like [`is_nixpkgs_input`]
/// does for nixpkgs specifically, and suppresses the warning for any
/// node that is the resolve-target of *some* `follows` entry anywhere in
/// the lock, rather than building a precise ancestor-reachability graph.
/// This can both under-warn (an unrelated follows chain elsewhere in the
/// lock happens to target one of the divergent nodes) and over-warn (a
/// legitimately-followed node whose follows edge this heuristic fails to
/// resolve, e.g. a malformed path) on deeply nested follows chains.
fn check_duplicate_inputs(graph: &LockGraph) -> Vec<CheckResult> {
  let followed = collect_followed_targets(graph);

  let mut groups: HashMap<(String, String), Vec<(&String, &LockNodeInfo)>> =
    HashMap::new();

  for (name, node) in &graph.nodes {
    let (Some(owner), Some(repo)) = (&node.owner, &node.repo) else {
      continue;
    };
    groups
      .entry((owner.clone(), repo.clone()))
      .or_default()
      .push((name, node));
  }

  let mut results = Vec::new();

  for ((owner, repo), members) in groups {
    // Only nodes not already explained by some follows edge count as
    // "divergent" candidates.
    let relevant: Vec<_> = members
      .into_iter()
      .filter(|(name, _)| !followed.contains(*name))
      .collect();

    let mut distinct_revs: Vec<&str> = relevant
      .iter()
      .filter_map(|(_, node)| node.rev.as_deref())
      .collect();
    distinct_revs.sort_unstable();
    distinct_revs.dedup();

    if distinct_revs.len() > 1 {
      let detail = relevant
        .iter()
        .filter_map(|(name, node)| {
          node.rev.as_deref().map(|rev| format!("{name} ({rev})"))
        })
        .collect::<Vec<_>>()
        .join(", ");

      results.push(CheckResult {
        code:   "duplicate-inputs",
        name:   "Duplicate inputs",
        status: CheckStatus::Warn(format!(
          "multiple inputs resolve to github:{owner}/{repo} at different revs \
           with no `follows` connecting them: {detail}"
        )),
      });
    }
  }

  results
}

/// Fetch `nix flake metadata --json` for `flake_dir`, parsed as JSON.
///
/// Used by both [`check_flake_description`] and [`check_dirty_tree`] so
/// the (potentially slow, network-touching) subprocess only runs once
/// per `xi doctor` invocation.
fn fetch_flake_metadata_json(flake_dir: &Path) -> Result<serde_json::Value> {
  let output = nix_command::NixCommand::new(nix_command::CommandKind::Flake)
    .arg("metadata")
    .arg("--json")
    .arg(flake_dir.to_string_lossy().as_ref())
    .output()?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("nix flake metadata --json failed: {stderr}");
  }

  Ok(serde_json::from_slice(&output.stdout)?)
}

/// Check whether the flake declares a top-level `description`.
fn check_flake_description(metadata: &serde_json::Value) -> CheckResult {
  match metadata
    .get("description")
    .and_then(serde_json::Value::as_str)
  {
    Some(desc) if !desc.trim().is_empty() => {
      CheckResult {
        code:   "flake-description",
        name:   "Flake description",
        status: CheckStatus::Ok(desc.to_string()),
      }
    },
    _ => {
      CheckResult {
        code:   "flake-description",
        name:   "Flake description",
        status: CheckStatus::Warn(
          "flake has no top-level `description`".to_string(),
        ),
      }
    },
  }
}

/// Check `nix flake metadata --json`'s dirty-tree indicators.
fn check_dirty_tree(metadata: &serde_json::Value) -> CheckResult {
  let dirty = metadata
    .get("dirtyRevision")
    .and_then(serde_json::Value::as_str)
    .is_some()
    || metadata
      .get("dirtyShortRev")
      .and_then(serde_json::Value::as_str)
      .is_some();

  if dirty {
    CheckResult {
      code:   "dirty-tree",
      name:   "Dirty tree",
      status: CheckStatus::Fail(
        "the flake's git tree is dirty  -  evaluation is not reproducible \
         until changes are committed"
          .to_string(),
      ),
    }
  } else {
    CheckResult {
      code:   "dirty-tree",
      name:   "Dirty tree",
      status: CheckStatus::Ok("clean".to_string()),
    }
  }
}

/// Check whether `flake.lock` is present but matched by the repo's
/// `.gitignore` (and therefore would never actually be committed).
///
/// `flake.lock` being missing entirely is already a hard `bail!` earlier
/// in [`run_health_checks`]  -  this only covers the "present on disk but
/// git-ignored" case.
fn check_lock_present_and_tracked(flake_dir: &Path) -> CheckResult {
  let output = std::process::Command::new("git")
    .arg("check-ignore")
    .arg("flake.lock")
    .current_dir(flake_dir)
    .output();

  // `git check-ignore` exits 0 when the path IS matched by .gitignore,
  // 1 when it is not, and other codes when it can't tell (e.g. not a
  // git repo)  -  the latter are treated as "can't tell, assume fine"
  // rather than failing the whole doctor run on it.
  match output {
    Ok(o) if o.status.success() => {
      CheckResult {
        code:   "lock-tracked",
        name:   "Lock file tracked",
        status: CheckStatus::Fail(
          "flake.lock is matched by .gitignore and would never be committed"
            .to_string(),
        ),
      }
    },
    _ => {
      CheckResult {
        code:   "lock-tracked",
        name:   "Lock file tracked",
        status: CheckStatus::Ok("flake.lock is not git-ignored".to_string()),
      }
    },
  }
}

/// Find the index (relative to `s`, which must start with `{`) of the
/// matching closing brace.
fn find_matching_brace(s: &str) -> Option<usize> {
  let mut depth = 0i32;
  for (i, c) in s.char_indices() {
    match c {
      '{' => depth += 1,
      '}' => {
        depth -= 1;
        if depth == 0 {
          return Some(i);
        }
      },
      _ => {},
    }
  }
  None
}

/// Extract a Nix string-list's quoted values, e.g. `key = [ "a" "b" ];`
/// → `["a", "b"]`. Simple quote-splitting, not a full Nix parser  -  fine
/// for the flat string lists `nixConfig` actually carries.
fn extract_quoted_list(block: &str, key: &str) -> Option<Vec<String>> {
  let key_idx = block.find(key)?;
  let after_key = &block[key_idx + key.len()..];
  let open = after_key.find('[')?;
  let close = after_key[open..].find(']')? + open;
  let list_str = &after_key[open + 1..close];

  // Splitting on `"` turns `[ "a" "b" ]` into `["", "a", " ", "b", ""]`  -
  // the quoted values sit at every odd index.
  Some(
    list_str
      .split('"')
      .skip(1)
      .step_by(2)
      .map(str::to_string)
      .collect(),
  )
}

/// Check that every `nixConfig.extra-substituters` entry has a
/// corresponding `nixConfig.extra-trusted-public-keys` entry at the same
/// index, by textually extracting the `nixConfig` block from the raw
/// `flake.nix` source.
///
/// `nixConfig` is read by the Nix CLI itself before any evaluation
/// happens, not something `nix eval` can report on as a flake output  -
/// hence the textual extraction rather than an eval-based check.
fn check_nixconfig_substituter_pairing(flake_nix: &str) -> Vec<CheckResult> {
  let Some(nixconfig_idx) = flake_nix.find("nixConfig") else {
    return vec![];
  };
  let rest = &flake_nix[nixconfig_idx..];

  let Some(open_brace) = rest.find('{') else {
    return vec![];
  };
  let Some(close_rel) = find_matching_brace(&rest[open_brace..]) else {
    return vec![];
  };
  let block = &rest[open_brace..=open_brace + close_rel];

  let substituters =
    extract_quoted_list(block, "extra-substituters").unwrap_or_default();
  let trusted_keys =
    extract_quoted_list(block, "extra-trusted-public-keys").unwrap_or_default();

  substituters
    .iter()
    .enumerate()
    .filter(|(idx, _)| trusted_keys.get(*idx).is_none())
    .map(|(idx, sub)| {
      CheckResult {
        code:   "nixconfig-substituter-pairing",
        name:   "Substituter/key pairing",
        status: CheckStatus::Warn(format!(
          "nixConfig.extra-substituters[{idx}] '{sub}' has no corresponding \
           extra-trusted-public-keys entry"
        )),
      }
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Derivation metadata checks (xi-spmx.3.2 - xi-spmx.3.5)
//
// All four checks below read from `PackageMetaInfo`, discovered in one
// `nix eval --apply --json` call by `crate::discover_package_meta` (see its
// doc comment in `lib.rs` and `show::PACKAGE_META_NIX` for the exact
// IFD-free expression). None of these checks shell out themselves - they
// are pure functions over already-fetched data, which is also what keeps
// them cheap to unit-test with hand-built fixtures.
// ---------------------------------------------------------------------------

/// Check a package's `meta.description`/`meta.license`/`meta.maintainers`/
/// `meta.platforms` completeness.
///
/// Emits one `Warn` per missing attribute - never a single combined
/// message per package - naming both the package and the specific missing
/// attribute, per the spec's "Doctor flags packages missing core meta
/// attributes" requirement.
fn check_package_meta_completeness(
  name: &str,
  info: &crate::PackageMetaInfo,
) -> Vec<CheckResult> {
  let mut results = Vec::new();

  if !info.has_description {
    results.push(CheckResult {
      code:   "package-meta-completeness",
      name:   "Package meta",
      status: CheckStatus::Warn(format!(
        "packages.*.{name} is missing meta.description"
      )),
    });
  }
  if !info.has_license {
    results.push(CheckResult {
      code:   "package-meta-completeness",
      name:   "Package meta",
      status: CheckStatus::Warn(format!(
        "packages.*.{name} is missing meta.license"
      )),
    });
  }
  if info.maintainers_count == 0 {
    results.push(CheckResult {
      code:   "package-meta-completeness",
      name:   "Package meta",
      status: CheckStatus::Warn(format!(
        "packages.*.{name} is missing meta.maintainers"
      )),
    });
  }
  if !info.has_platforms {
    results.push(CheckResult {
      code:   "package-meta-completeness",
      name:   "Package meta",
      status: CheckStatus::Warn(format!(
        "packages.*.{name} is missing meta.platforms"
      )),
    });
  }

  results
}

/// Check a package's `version` attribute.
///
/// Warns when `version` is absent, or equals the literal placeholder
/// string `"unstable"`, per the spec's "Doctor flags missing or placeholder
/// versions" requirement.
fn check_package_version(
  name: &str,
  info: &crate::PackageMetaInfo,
) -> Option<CheckResult> {
  match info.version.as_deref() {
    None => {
      Some(CheckResult {
        code:   "package-version",
        name:   "Package version",
        status: CheckStatus::Warn(format!(
          "packages.*.{name} has no version attribute"
        )),
      })
    },
    Some("unstable") => {
      Some(CheckResult {
        code:   "package-version",
        name:   "Package version",
        status: CheckStatus::Warn(format!(
          "packages.*.{name} uses placeholder version \"unstable\""
        )),
      })
    },
    Some(_) => None,
  }
}

/// Check a package exposed via `apps.<system>.*` for a missing
/// `meta.mainProgram`.
///
/// Per the spec's "Doctor flags apps missing mainProgram" requirement:
/// warns whenever `meta.mainProgram` is unset, for any app-exposed
/// package, regardless of its actual binary count.
///
/// **One documented, intentional limitation** (see `bd note xi-spmx.3.4`
/// for the full rationale):
///
/// Matching an app to a package requires knowing which package
/// `apps.<system>.<name>.program` points at. For simplicity, callers of
/// this function match `apps.<system>.<name>` to
/// `packages.<system>.<name>` by identical attribute name - a known
/// heuristic limitation that will miss apps whose `program` points at
/// a differently-named package (see `run_health_checks`).
///
/// **Why no binary-count precondition:** an earlier draft of the spec
/// additionally required counting executables under `bin/` and warning
/// only when there were two or more. That was dropped (amendment landed
/// in `proposal.md`/`spec.md`) because counting `bin/` entries requires
/// reading a package's *built* output (e.g. `builtins.readDir` on
/// `${pkg}/bin`), which is import-from-derivation and is explicitly
/// disallowed for this check group - see `show::PACKAGE_META_NIX`'s doc
/// comment: it would force a full build of every app-exposed package
/// just to run `xi doctor`, defeating the point of a fast health check.
fn check_app_main_program(
  app_name: &str,
  info: &crate::PackageMetaInfo,
) -> Option<CheckResult> {
  if info.main_program.is_some() {
    return None;
  }

  Some(CheckResult {
    code:   "app-main-program",
    name:   "App main program",
    status: CheckStatus::Warn(format!(
      "apps.*.{app_name} has no meta.mainProgram set"
    )),
  })
}

/// Check a package reachable from `packages.<system>.*` (the flake's
/// default-visible outputs) for `meta.insecure` or any
/// `meta.knownVulnerabilities` entries.
///
/// Per the spec's "Doctor flags insecure packages reachable from default
/// outputs" requirement.
fn check_package_insecure(
  name: &str,
  info: &crate::PackageMetaInfo,
) -> Option<CheckResult> {
  if info.insecure || info.known_vulnerabilities_count > 0 {
    Some(CheckResult {
      code:   "package-insecure",
      name:   "Package security",
      status: CheckStatus::Warn(format!(
        "packages.*.{name} is marked insecure (meta.insecure={}, \
         knownVulnerabilities count={})",
        info.insecure, info.known_vulnerabilities_count
      )),
    })
  } else {
    None
  }
}

// ---------------------------------------------------------------------------
// Output hygiene checks (xi-spmx.4.2 - xi-spmx.4.4)
//
// All three checks below read from one `nix flake show --json` result,
// fetched once per `xi doctor` run by `crate::fetch_flake_show_json` (see
// its doc comment in `lib.rs`) and shared across them  -  none of these
// checks shell out themselves.
// ---------------------------------------------------------------------------

/// Flake output categories nested per-system (`<category>.<system>.<name>`)
/// that [`check_per_system_output_gaps`] compares across systems.
const PER_SYSTEM_GAP_CATEGORIES: &[&str] =
  &["packages", "apps", "devShells", "checks"];

/// Check `packages`/`apps`/`devShells`/`checks` for per-system output gaps.
///
/// There is no separate flake-level `systems` list to consult from
/// `nix flake show --json` alone (per xi-spmx.4.2), so for each category
/// the "declared systems" set is inferred as the union of every system
/// name appearing anywhere under that category. For each leaf key (e.g. a
/// package name) present under at least one of those systems, warns once
/// per `(key, missing-system)` pair where the key is absent from another
/// declared system.
fn check_per_system_output_gaps(
  show_json: &serde_json::Value,
) -> Vec<CheckResult> {
  let mut results = Vec::new();

  for category in PER_SYSTEM_GAP_CATEGORIES {
    let Some(systems) = show_json
      .get(category)
      .and_then(serde_json::Value::as_object)
    else {
      continue;
    };

    // leaf_name -> set of systems it's present under.
    let mut leaf_systems: HashMap<String, HashSet<String>> = HashMap::new();
    let mut declared_systems: Vec<String> = Vec::new();

    for (system_name, leaves) in systems {
      declared_systems.push(system_name.clone());
      let Some(leaves) = leaves.as_object() else {
        continue;
      };
      for leaf_name in leaves.keys() {
        leaf_systems
          .entry(leaf_name.clone())
          .or_default()
          .insert(system_name.clone());
      }
    }

    // Sorted for deterministic ordering (HashMap iteration order isn't
    // stable) - stable output and stable test assertions.
    let mut leaf_names: Vec<&String> = leaf_systems.keys().collect();
    leaf_names.sort();

    for leaf_name in leaf_names {
      let present_on = &leaf_systems[leaf_name];
      let mut missing_systems: Vec<&String> = declared_systems
        .iter()
        .filter(|system| !present_on.contains(*system))
        .collect();
      missing_systems.sort();

      for system in missing_systems {
        results.push(CheckResult {
          code:   "per-system-output-gap",
          name:   "Per-system output gap",
          status: CheckStatus::Warn(format!(
            "{category}.{system}.{leaf_name} is missing, but {leaf_name} is \
             present under {category} for at least one other declared system"
          )),
        });
      }
    }
  }

  results
}

/// Check whether the flake declares a `checks` output at all.
///
/// Warns when the top-level `checks` key is entirely absent from the
/// `nix flake show --json` result (or present with no system entries at
/// all) - i.e. no system has any checks. Distinct from
/// [`check_per_system_output_gaps`], which only compares systems that
/// already have *some* entries under a category.
fn check_checks_output_present(show_json: &serde_json::Value) -> CheckResult {
  let has_checks = show_json
    .get("checks")
    .and_then(serde_json::Value::as_object)
    .is_some_and(|systems| !systems.is_empty());

  if has_checks {
    CheckResult {
      code:   "checks-output-missing",
      name:   "Checks output",
      status: CheckStatus::Ok("checks output declared".to_string()),
    }
  } else {
    CheckResult {
      code:   "checks-output-missing",
      name:   "Checks output",
      status: CheckStatus::Warn(
        "flake declares no `checks` output for any system".to_string(),
      ),
    }
  }
}

/// Check top-level output keys against the conventional allowlist.
///
/// Diffs the top-level keys of a `nix flake show --json` result against
/// [`xi_core::flake_output::FlakeOutput::from_nix_name`], warning for
/// every key that allowlist doesn't recognize. Worded explicitly as
/// informational, never as a validation failure: per proposal.md Open
/// Question 3 (and the spec's "Doctor flags non-standard top-level output
/// keys as informational" requirement), this deliberately does not read
/// or consult any `schemas`/`exportedSchemas` output the flake might
/// itself expose.
fn check_nonstandard_output_keys(
  show_json: &serde_json::Value,
) -> Vec<CheckResult> {
  let Some(top_level) = show_json.as_object() else {
    return vec![];
  };

  let mut keys: Vec<&String> = top_level.keys().collect();
  keys.sort();

  keys
    .into_iter()
    .filter(|key| FlakeOutput::from_nix_name(key).is_none())
    .map(|key| {
      CheckResult {
        code:   "nonstandard-output-key",
        name:   "Non-standard output key",
        status: CheckStatus::Warn(format!(
          "output key '{key}' is not part of the conventional flake-output \
           set (informational only, not invalid or an error)"
        )),
      }
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Run all health checks
// ---------------------------------------------------------------------------

/// Re-raise a worker thread's panic in the joining thread.
///
/// `resume_unwind` rather than `expect`: the panic is re-raised carrying its
/// original payload and location, instead of being flattened into a generic
/// "thread panicked" message that discards what actually failed.
fn rejoin<T>(r: std::thread::Result<T>) -> T {
  r.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// Run all flake health checks and return results.
pub fn run_health_checks(
  flake_dir: &Path,
  flake_ref: &str,
  config: &ProjectDoctorConfig,
) -> Result<Vec<CheckResult>> {
  let lock_path = flake_dir.join("flake.lock");
  if !lock_path.exists() {
    bail!(
      "No flake.lock found in {}. Run `nix flake lock` first.",
      flake_dir.display()
    );
  }

  let inputs = parse_flake_lock(&lock_path)?;
  debug!("Parsed {} direct inputs from flake.lock", inputs.len());

  let max_age = config
    .max_input_age_days
    .unwrap_or(DEFAULT_MAX_INPUT_AGE_DAYS);

  let supported_branches: Vec<String> = if config.supported_branches.is_empty()
  {
    DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect()
  } else {
    config.supported_branches.clone()
  };

  let mut results = Vec::new();

  // Per-input checks (no subprocess calls, cheap enough to stay sequential)
  for input in &inputs {
    if let Some(r) = check_nixpkgs_branch(input, &supported_branches) {
      results.push(r);
    }
    if let Some(r) = check_input_freshness(input, max_age) {
      results.push(r);
    }
    if config.require_official_nixpkgs
      && let Some(r) = check_nixpkgs_owner(input)
    {
      results.push(r);
    }
    if !config.disable_structure_checks
      && let Some(r) = check_floating_input_refs(input)
    {
      results.push(r);
    }
  }

  let system = crate::current_nix_system();

  // Every check group below shells out to a slow, independent `nix`
  // subprocess (eval / flake metadata / flake show) that does not depend
  // on any other group's output. Running them one after another made
  // `xi doctor`'s wall-clock time the SUM of every group's subprocess
  // call; running them concurrently on `std::thread::scope` makes it the
  // MAX of whichever group is slowest. Only the subprocess calls
  // themselves are parallelized here -- interpreting each result into
  // `CheckResult`s afterward is cheap, in-memory, and stays sequential.
  let (
    formatter_result,
    metadata_result,
    package_meta_result,
    app_names_result,
    show_result,
  ) = thread::scope(|scope| {
    let formatter_handle = scope.spawn(|| check_formatter(flake_ref));

    let metadata_handle = (!config.disable_structure_checks)
      .then(|| scope.spawn(|| fetch_flake_metadata_json(flake_dir)));

    let package_meta_handle = (!config.disable_metadata_checks).then(|| {
      scope.spawn(|| crate::discover_package_meta(flake_ref, &system))
    });

    let app_names_handle = (!config.disable_metadata_checks)
      .then(|| scope.spawn(|| crate::discover_app_names(flake_ref, &system)));

    let show_handle = (!config.disable_output_checks)
      .then(|| scope.spawn(|| crate::fetch_flake_show_json(flake_ref)));

    (
      rejoin(formatter_handle.join()),
      metadata_handle.map(|h| rejoin(h.join())),
      package_meta_handle.map(|h| rejoin(h.join())),
      app_names_handle.map(|h| rejoin(h.join())),
      show_handle.map(|h| rejoin(h.join())),
    )
  });

  // Formatter check.
  results.push(formatter_result);

  // Structural checks (xi-spmx.2.1 - 2.6): description, lock-tracked,
  // duplicate-inputs, dirty-tree, substituter-pairing. Floating-ref
  // checking above is part of this same group. Gated by the
  // `disable-structure-checks` opt-out (xi-spmx.5.1), mirroring the
  // metadata/output groups below -- skipped entirely, including their
  // subprocess calls, rather than run and discarded.
  if config.disable_structure_checks {
    debug!("structure check group disabled via .xi.toml [doctor]");
  } else {
    // flake.lock present-but-gitignored check.
    results.push(check_lock_present_and_tracked(flake_dir));

    // Duplicate (un-followed, divergent-rev) transitive inputs -- walks
    // every node in flake.lock, not just the direct ones parsed above.
    match parse_all_lock_nodes(&lock_path) {
      Ok(graph) => results.extend(check_duplicate_inputs(&graph)),
      Err(e) => debug!("failed to walk full flake.lock node graph: {e}"),
    }

    // Flake description + dirty-tree checks share the one `nix flake
    // metadata --json` call fetched concurrently above.
    match metadata_result {
      Some(Ok(metadata)) => {
        results.push(check_flake_description(&metadata));
        results.push(check_dirty_tree(&metadata));
      },
      Some(Err(e)) => {
        debug!("failed to fetch nix flake metadata --json: {e}");
      },
      None => debug!("metadata thread was not spawned"),
    }

    // nixConfig substituter/trusted-key pairing -- textual flake.nix parse.
    if let Ok(flake_nix) = std::fs::read_to_string(flake_dir.join("flake.nix"))
    {
      results.extend(check_nixconfig_substituter_pairing(&flake_nix));
    }
  }

  // Derivation metadata checks (xi-spmx.3.2 - 3.5) -- the `nix eval
  // --apply --json` calls across `packages.<system>.*` and
  // `apps.<system>.*` were fetched concurrently above, gated by the
  // `disable-metadata-checks` opt-out (xi-spmx.3.6).
  if config.disable_metadata_checks {
    debug!("metadata check group disabled via .xi.toml [doctor]");
  } else {
    match package_meta_result {
      Some(Ok(package_meta)) => {
        for (name, info) in &package_meta {
          results.extend(check_package_meta_completeness(name, info));
          if let Some(r) = check_package_version(name, info) {
            results.push(r);
          }
          if let Some(r) = check_package_insecure(name, info) {
            results.push(r);
          }
        }

        // apps.<system>.<name> matched to packages.<system>.<name> by
        // identical attribute name -- see check_app_main_program's doc
        // comment for the heuristic limitation this carries.
        match app_names_result {
          Some(Ok(app_names)) => {
            for app_name in &app_names {
              if let Some(info) = package_meta.get(app_name)
                && let Some(r) = check_app_main_program(app_name, info)
              {
                results.push(r);
              }
            }
          },
          Some(Err(e)) => debug!("failed to discover app names: {e}"),
          None => debug!("app-names thread was not spawned"),
        }
      },
      Some(Err(e)) => debug!("failed to discover package meta: {e}"),
      None => debug!("package-meta thread was not spawned"),
    }
  }

  // Output hygiene checks (xi-spmx.4.2 - 4.4) -- the `nix flake show
  // --json` call shared across all three was fetched concurrently above,
  // gated by the `disable-output-checks` opt-out.
  if config.disable_output_checks {
    debug!("output check group disabled via .xi.toml [doctor]");
  } else {
    match show_result {
      Some(Ok(show_json)) => {
        results.extend(check_per_system_output_gaps(&show_json));
        results.push(check_checks_output_present(&show_json));
        results.extend(check_nonstandard_output_keys(&show_json));
      },
      Some(Err(e)) => {
        debug!("failed to fetch nix flake show --json: {e}");
      },
      None => debug!("flake-show thread was not spawned"),
    }
  }

  Ok(apply_rule_overrides(config, results))
}

/// Apply `[doctor.rules]` overrides to every produced result: drop a rule
/// set to `Off` entirely, or force the severity of one set to `Warn`/
/// `Fail` when it fires. A passing (`Ok`) result is only ever dropped by
/// `Off` -- a severity override on an `Ok` result is a no-op, since there
/// is nothing to escalate or downgrade.
///
/// Applied once over the whole accumulated list rather than threaded
/// through every individual check call site, since it only needs each
/// result's `code`, not any of the check-specific context that produced
/// it.
fn apply_rule_overrides(
  config: &ProjectDoctorConfig,
  results: Vec<CheckResult>,
) -> Vec<CheckResult> {
  results
    .into_iter()
    .filter_map(|mut result| {
      match config.rules.get(result.code) {
        None => Some(result),
        Some(crate::project_config::RuleSetting::Off) => None,
        Some(crate::project_config::RuleSetting::Warn) => {
          if let CheckStatus::Fail(msg) = result.status {
            result.status = CheckStatus::Warn(msg);
          }
          Some(result)
        },
        Some(crate::project_config::RuleSetting::Fail) => {
          if let CheckStatus::Warn(msg) = result.status {
            result.status = CheckStatus::Fail(msg);
          }
          Some(result)
        },
      }
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Doctor command
// ---------------------------------------------------------------------------

impl DoctorArgs {
  /// Run the doctor command.
  ///
  /// # Errors
  ///
  /// Returns an error if no flake.lock is found.
  pub fn run(self) -> Result<()> {
    let flake_ref = self.flake_ref.as_deref().unwrap_or(".");
    let local_dir = resolve_local_flake_dir(Some(flake_ref));

    let Some(ref flake_dir) = local_dir else {
      bail!("doctor requires a local flake reference, got: {flake_ref}");
    };

    let config = crate::project_config::load_project_config(Some(flake_dir));

    println!();
    println!(
      "  {}",
      Paint::new(format!("Flake health ({flake_ref})")).bold()
    );
    println!();

    let results = run_health_checks(flake_dir, flake_ref, &config.doctor)?;

    for result in &results {
      result.print();
    }

    let warnings = results.iter().filter(|r| r.is_warn()).count();
    let failures = results.iter().filter(|r| r.is_fail()).count();

    println!();

    if failures > 0 {
      println!(
        "  {}",
        Paint::new(format!("{failures} issue(s), {warnings} warning(s)"))
          .fg(Color::Red)
          .bold()
      );
      bail!("{failures} health check(s) failed");
    } else if warnings > 0 {
      println!(
        "  {}",
        Paint::new(format!("{warnings} warning(s)"))
          .fg(Color::Yellow)
          .bold()
      );
    } else {
      println!(
        "  {}",
        Paint::new("All checks passed").fg(Color::Green).bold()
      );
    }

    println!();
    Ok(())
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  fn make_nixpkgs_input(
    branch: &str,
    owner: &str,
    age_days: u64,
  ) -> FlakeInput {
    let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_secs();

    FlakeInput {
      name:             "nixpkgs".to_string(),
      owner:            Some(owner.to_string()),
      repo:             Some("nixpkgs".to_string()),
      git_ref:          Some(branch.to_string()),
      last_modified:    Some(now - (age_days * SECONDS_PER_DAY)),
      source_type:      Some("github".to_string()),
      has_explicit_rev: false,
    }
  }

  #[test]
  fn supported_branch_passes() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 5);
    let supported: Vec<String> = DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect();
    let result = check_nixpkgs_branch(&input, &supported);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Ok(_)));
  }

  #[test]
  fn unsupported_branch_warns() {
    let input = make_nixpkgs_input("nixos-23.05", "NixOS", 5);
    let supported: Vec<String> = DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect();
    let result = check_nixpkgs_branch(&input, &supported);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn fresh_input_passes() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 10);
    let result = check_input_freshness(&input, 30);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Ok(_)));
  }

  #[test]
  fn stale_input_warns() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 60);
    let result = check_input_freshness(&input, 30);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn official_owner_passes() {
    let input = make_nixpkgs_input("nixos-unstable", "NixOS", 5);
    let result = check_nixpkgs_owner(&input);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Ok(_)));
  }

  #[test]
  fn fork_owner_warns() {
    let input = make_nixpkgs_input("nixos-unstable", "someuser", 5);
    let result = check_nixpkgs_owner(&input);
    assert!(result.is_some());
    let r = result.expect("should have result");
    assert!(matches!(r.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn non_nixpkgs_skips_branch_check() {
    let input = FlakeInput {
      name:             "crane".to_string(),
      owner:            Some("ipetkov".to_string()),
      repo:             Some("crane".to_string()),
      git_ref:          Some("master".to_string()),
      last_modified:    Some(0),
      source_type:      Some("github".to_string()),
      has_explicit_rev: false,
    };
    let supported: Vec<String> = DEFAULT_SUPPORTED_BRANCHES
      .iter()
      .map(|s| (*s).to_string())
      .collect();
    assert!(check_nixpkgs_branch(&input, &supported).is_none());
    assert!(check_nixpkgs_owner(&input).is_none());
  }

  #[test]
  fn parse_lock_file_realistic() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let lock_content = r#"{
      "nodes": {
        "nixpkgs": {
          "locked": {
            "lastModified": 1700000000,
            "owner": "NixOS",
            "repo": "nixpkgs",
            "rev": "abc123",
            "type": "github"
          },
          "original": {
            "owner": "NixOS",
            "repo": "nixpkgs",
            "ref": "nixos-unstable",
            "type": "github"
          }
        },
        "crane": {
          "locked": {
            "lastModified": 1700000000,
            "owner": "ipetkov",
            "repo": "crane",
            "rev": "def456",
            "type": "github"
          },
          "original": {
            "owner": "ipetkov",
            "repo": "crane",
            "ref": "master",
            "type": "github"
          }
        },
        "root": {
          "inputs": {
            "nixpkgs": "nixpkgs",
            "crane": "crane"
          }
        }
      },
      "root": "root",
      "version": 7
    }"#;

    std::fs::write(dir.path().join("flake.lock"), lock_content)
      .expect("write lock");

    let inputs =
      parse_flake_lock(&dir.path().join("flake.lock")).expect("parse");
    assert_eq!(inputs.len(), 2);

    let nixpkgs = inputs
      .iter()
      .find(|i| i.name == "nixpkgs")
      .expect("nixpkgs");
    assert_eq!(nixpkgs.owner.as_deref(), Some("NixOS"));
    assert_eq!(nixpkgs.git_ref.as_deref(), Some("nixos-unstable"));

    let crane = inputs.iter().find(|i| i.name == "crane").expect("crane");
    assert_eq!(crane.owner.as_deref(), Some("ipetkov"));
  }

  // -- 2.1 check_flake_description -----------------------------------------

  #[test]
  fn missing_description_warns() {
    let metadata = serde_json::json!({});
    let result = check_flake_description(&metadata);
    assert!(matches!(result.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn blank_description_warns() {
    let metadata = serde_json::json!({ "description": "   " });
    let result = check_flake_description(&metadata);
    assert!(matches!(result.status, CheckStatus::Warn(_)));
  }

  #[test]
  fn present_description_passes() {
    let metadata = serde_json::json!({ "description": "my flake" });
    let result = check_flake_description(&metadata);
    assert!(matches!(result.status, CheckStatus::Ok(_)));
  }

  // -- 2.2 check_lock_present_and_tracked ----------------------------------

  #[test]
  fn gitignored_lock_fails() {
    let dir = tempfile::tempdir().expect("create tempdir");

    let init = std::process::Command::new("git")
      .arg("init")
      .arg("--quiet")
      .current_dir(dir.path())
      .status();
    if !matches!(init, Ok(s) if s.success()) {
      eprintln!("skipping gitignored_lock_fails: git not available");
      return;
    }

    std::fs::write(dir.path().join(".gitignore"), "flake.lock\n")
      .expect("write .gitignore");
    std::fs::write(dir.path().join("flake.lock"), "{}").expect("write lock");

    let result = check_lock_present_and_tracked(dir.path());
    assert!(
      matches!(result.status, CheckStatus::Fail(_)),
      "expected Fail, got {:?}",
      result.status
    );
  }

  #[test]
  fn tracked_lock_passes() {
    let dir = tempfile::tempdir().expect("create tempdir");

    let init = std::process::Command::new("git")
      .arg("init")
      .arg("--quiet")
      .current_dir(dir.path())
      .status();
    if !matches!(init, Ok(s) if s.success()) {
      eprintln!("skipping tracked_lock_passes: git not available");
      return;
    }

    std::fs::write(dir.path().join("flake.lock"), "{}").expect("write lock");

    let result = check_lock_present_and_tracked(dir.path());
    assert!(
      matches!(result.status, CheckStatus::Ok(_)),
      "expected Ok, got {:?}",
      result.status
    );
  }

  // -- 2.3 check_floating_input_refs ---------------------------------------

  #[test]
  fn floating_ref_warns_even_when_fresh() {
    let mut input = make_nixpkgs_input("main", "NixOS", 0);
    input.has_explicit_rev = false;
    let result = check_floating_input_refs(&input);
    assert!(result.is_some());
    assert!(matches!(
      result.expect("result").status,
      CheckStatus::Warn(_)
    ));
  }

  #[test]
  fn pinned_rev_input_does_not_warn() {
    let mut input = make_nixpkgs_input("nixos-unstable", "NixOS", 0);
    input.has_explicit_rev = true;
    assert!(check_floating_input_refs(&input).is_none());
  }

  #[test]
  fn no_ref_input_does_not_warn() {
    let mut input = make_nixpkgs_input("nixos-unstable", "NixOS", 0);
    input.git_ref = None;
    input.has_explicit_rev = false;
    assert!(check_floating_input_refs(&input).is_none());
  }

  #[test]
  fn is_tag_shaped_ref_recognizes_version_tags() {
    // Real-world tag pins from this repo's own docs/flake.lock: `rev` is
    // absent for all of these (only `ref` is set), so `has_explicit_rev`
    // alone would be false for every one.
    for tag in [
      "v0.12.1", "v0.21.1", "v0.2.2", "0.2.1", "1.36.0", "0.2.0", "v0.5.0",
      "0.2.0.0",
    ] {
      assert!(is_tag_shaped_ref(tag), "expected '{tag}' to be tag-shaped");
    }
  }

  #[test]
  fn is_tag_shaped_ref_rejects_branch_names() {
    for branch in [
      "main",
      "master",
      "nixos-unstable",
      "nixos-23.11",
      "release-1.0",
      "develop",
    ] {
      assert!(
        !is_tag_shaped_ref(branch),
        "expected '{branch}' to NOT be tag-shaped"
      );
    }
  }

  /// Regression test for the real-world shape documented on
  /// `FlakeInput::has_explicit_rev`: a tag pin like
  /// `github:ipetkov/crane/v0.12.1` stores the tag only in
  /// `original.ref`  -  `original.rev` is absent, exactly like this
  /// repo's own `docs/flake.lock` entry for `crane`. Parsing such a lock
  /// must not produce a floating-ref warning.
  #[test]
  fn tag_pinned_input_from_realistic_lock_does_not_warn() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let lock_content = r#"{
      "nodes": {
        "crane": {
          "locked": {
            "lastModified": 1700000000,
            "owner": "ipetkov",
            "repo": "crane",
            "rev": "aabbccddeeff00112233445566778899aabbccd",
            "type": "github"
          },
          "original": {
            "owner": "ipetkov",
            "repo": "crane",
            "ref": "v0.12.1",
            "type": "github"
          }
        },
        "root": {
          "inputs": {
            "crane": "crane"
          }
        }
      },
      "root": "root",
      "version": 7
    }"#;

    std::fs::write(dir.path().join("flake.lock"), lock_content)
      .expect("write lock");

    let inputs =
      parse_flake_lock(&dir.path().join("flake.lock")).expect("parse");
    let crane = inputs.iter().find(|i| i.name == "crane").expect("crane");

    // The real-world shape: `original.rev` is absent for a tag pin.
    assert!(!crane.has_explicit_rev);
    assert_eq!(crane.git_ref.as_deref(), Some("v0.12.1"));

    assert!(
      check_floating_input_refs(crane).is_none(),
      "tag-pinned input 'crane' (ref 'v0.12.1') must not warn as floating"
    );
  }

  // -- 2.4 check_duplicate_inputs -------------------------------------------

  fn write_duplicate_lock(
    dir: &std::path::Path,
    follows: bool,
  ) -> std::path::PathBuf {
    let dep_nixpkgs_input = if follows {
      r#""nixpkgs": ["nixpkgs"]"#
    } else {
      r#""nixpkgs": "nixpkgs_2""#
    };

    let lock_content = format!(
      r#"{{
        "nodes": {{
          "nixpkgs": {{
            "locked": {{
              "owner": "NixOS",
              "repo": "nixpkgs",
              "rev": "aaa111",
              "type": "github"
            }},
            "original": {{
              "owner": "NixOS",
              "repo": "nixpkgs",
              "ref": "nixos-unstable",
              "type": "github"
            }}
          }},
          "nixpkgs_2": {{
            "locked": {{
              "owner": "NixOS",
              "repo": "nixpkgs",
              "rev": "bbb222",
              "type": "github"
            }},
            "original": {{
              "owner": "NixOS",
              "repo": "nixpkgs",
              "ref": "nixos-24.11",
              "type": "github"
            }}
          }},
          "dep": {{
            "inputs": {{
              {dep_nixpkgs_input}
            }}
          }},
          "root": {{
            "inputs": {{
              "nixpkgs": "nixpkgs",
              "dep": "dep"
            }}
          }}
        }},
        "root": "root",
        "version": 7
      }}"#
    );

    let lock_path = dir.join("flake.lock");
    std::fs::write(&lock_path, lock_content).expect("write lock");
    lock_path
  }

  #[test]
  fn divergent_inputs_without_follows_warn() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let lock_path = write_duplicate_lock(dir.path(), false);

    let graph = parse_all_lock_nodes(&lock_path).expect("parse graph");
    let results = check_duplicate_inputs(&graph);

    assert_eq!(
      results.len(),
      1,
      "expected exactly one warning, got {results:?}"
    );
    assert!(matches!(results[0].status, CheckStatus::Warn(_)));
  }

  #[test]
  fn divergent_inputs_with_follows_do_not_warn() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let lock_path = write_duplicate_lock(dir.path(), true);

    let graph = parse_all_lock_nodes(&lock_path).expect("parse graph");
    let results = check_duplicate_inputs(&graph);

    assert!(
      results.is_empty(),
      "expected no warnings once follows connects the pair, got {results:?}"
    );
  }

  // -- 2.5 check_dirty_tree --------------------------------------------------

  #[test]
  fn dirty_short_rev_fails() {
    let metadata = serde_json::json!({ "dirtyShortRev": "abc123-dirty" });
    let result = check_dirty_tree(&metadata);
    assert!(matches!(result.status, CheckStatus::Fail(_)));
  }

  #[test]
  fn dirty_revision_fails() {
    let metadata = serde_json::json!({ "dirtyRevision": "abc123-dirty" });
    let result = check_dirty_tree(&metadata);
    assert!(matches!(result.status, CheckStatus::Fail(_)));
  }

  #[test]
  fn clean_tree_passes() {
    let metadata = serde_json::json!({ "rev": "abc123" });
    let result = check_dirty_tree(&metadata);
    assert!(matches!(result.status, CheckStatus::Ok(_)));
  }

  // -- 2.6 check_nixconfig_substituter_pairing -------------------------------

  #[test]
  fn unpaired_substituter_warns() {
    let flake_nix = r#"
      {
        description = "test";
        nixConfig = {
          extra-substituters = [
            "https://example.cachix.org"
          ];
          extra-trusted-public-keys = [ ];
        };
        outputs = { self }: { };
      }
    "#;

    let results = check_nixconfig_substituter_pairing(flake_nix);
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].status, CheckStatus::Warn(_)));
  }

  #[test]
  fn paired_substituter_does_not_warn() {
    let flake_nix = r#"
      {
        nixConfig = {
          extra-substituters = [
            "https://example.cachix.org"
          ];
          extra-trusted-public-keys = [
            "example.cachix.org-1:abcdef="
          ];
        };
      }
    "#;

    let results = check_nixconfig_substituter_pairing(flake_nix);
    assert!(results.is_empty(), "expected no warnings, got {results:?}");
  }

  #[test]
  fn no_nixconfig_produces_no_results() {
    let flake_nix = r#"
      {
        description = "test";
        outputs = { self }: { };
      }
    "#;

    assert!(check_nixconfig_substituter_pairing(flake_nix).is_empty());
  }

  // -- 3.2 check_package_meta_completeness ----------------------------------

  fn complete_package_meta() -> crate::PackageMetaInfo {
    crate::PackageMetaInfo {
      has_description:             true,
      has_license:                 true,
      maintainers_count:           1,
      has_platforms:               true,
      version:                     Some("1.0.0".to_string()),
      pname:                       Some("foo".to_string()),
      main_program:                Some("foo".to_string()),
      insecure:                    false,
      known_vulnerabilities_count: 0,
    }
  }

  #[test]
  fn fully_described_package_has_no_meta_warnings() {
    let info = complete_package_meta();
    let results = check_package_meta_completeness("foo", &info);
    assert!(results.is_empty(), "expected no warnings, got {results:?}");
  }

  #[test]
  fn missing_description_warns_once_naming_package_and_attribute() {
    let mut info = complete_package_meta();
    info.has_description = false;
    let results = check_package_meta_completeness("foo", &info);
    assert_eq!(
      results.len(),
      1,
      "expected exactly one warning, got {results:?}"
    );
    assert!(matches!(results[0].status, CheckStatus::Warn(_)));
    let CheckStatus::Warn(ref msg) = results[0].status else {
      unreachable!()
    };
    assert!(
      msg.contains("foo"),
      "message should name the package: {msg}"
    );
    assert!(
      msg.contains("meta.description"),
      "message should name the specific attribute: {msg}"
    );
  }

  #[test]
  fn missing_license_warns_once_independent_of_other_attrs() {
    let mut info = complete_package_meta();
    info.has_license = false;
    let results = check_package_meta_completeness("foo", &info);
    assert_eq!(
      results.len(),
      1,
      "expected exactly one warning, got {results:?}"
    );
    let CheckStatus::Warn(ref msg) = results[0].status else {
      unreachable!()
    };
    assert!(msg.contains("meta.license"), "{msg}");
  }

  #[test]
  fn zero_maintainers_warns_once() {
    let mut info = complete_package_meta();
    info.maintainers_count = 0;
    let results = check_package_meta_completeness("foo", &info);
    assert_eq!(
      results.len(),
      1,
      "expected exactly one warning, got {results:?}"
    );
    let CheckStatus::Warn(ref msg) = results[0].status else {
      unreachable!()
    };
    assert!(msg.contains("meta.maintainers"), "{msg}");
  }

  #[test]
  fn missing_platforms_warns_once() {
    let mut info = complete_package_meta();
    info.has_platforms = false;
    let results = check_package_meta_completeness("foo", &info);
    assert_eq!(
      results.len(),
      1,
      "expected exactly one warning, got {results:?}"
    );
    let CheckStatus::Warn(ref msg) = results[0].status else {
      unreachable!()
    };
    assert!(msg.contains("meta.platforms"), "{msg}");
  }

  #[test]
  fn multiple_missing_attrs_produce_independent_warnings() {
    let mut info = complete_package_meta();
    info.has_description = false;
    info.has_license = false;
    info.maintainers_count = 0;
    info.has_platforms = false;
    let results = check_package_meta_completeness("foo", &info);
    assert_eq!(
      results.len(),
      4,
      "expected one independent warning per missing attribute, got {results:?}"
    );
  }

  // -- 3.3 check_package_version ---------------------------------------------

  #[test]
  fn present_version_does_not_warn() {
    let info = complete_package_meta();
    assert!(check_package_version("foo", &info).is_none());
  }

  #[test]
  fn missing_version_warns() {
    let mut info = complete_package_meta();
    info.version = None;
    let result = check_package_version("foo", &info);
    assert!(result.is_some());
    assert!(matches!(
      result.expect("result").status,
      CheckStatus::Warn(_)
    ));
  }

  #[test]
  fn unstable_placeholder_version_warns() {
    let mut info = complete_package_meta();
    info.version = Some("unstable".to_string());
    let result = check_package_version("foo", &info);
    assert!(result.is_some());
    assert!(matches!(
      result.expect("result").status,
      CheckStatus::Warn(_)
    ));
  }

  #[test]
  fn non_placeholder_version_string_does_not_warn() {
    let mut info = complete_package_meta();
    info.version = Some("unstable-2024-01-01".to_string());
    assert!(
      check_package_version("foo", &info).is_none(),
      "only the exact literal \"unstable\" should warn"
    );
  }

  // -- 3.4 check_app_main_program
  // ----------------------------------------------

  #[test]
  fn app_with_main_program_does_not_warn() {
    let info = complete_package_meta();
    assert!(check_app_main_program("foo", &info).is_none());
  }

  #[test]
  fn app_without_main_program_warns() {
    let mut info = complete_package_meta();
    info.main_program = None;
    let result = check_app_main_program("foo", &info);
    assert!(result.is_some());
    assert!(matches!(
      result.expect("result").status,
      CheckStatus::Warn(_)
    ));
  }

  // -- 3.5 check_package_insecure
  // ----------------------------------------------

  #[test]
  fn secure_package_does_not_warn() {
    let info = complete_package_meta();
    assert!(check_package_insecure("foo", &info).is_none());
  }

  #[test]
  fn insecure_flag_warns() {
    let mut info = complete_package_meta();
    info.insecure = true;
    let result = check_package_insecure("foo", &info);
    assert!(result.is_some());
    assert!(matches!(
      result.expect("result").status,
      CheckStatus::Warn(_)
    ));
  }

  #[test]
  fn known_vulnerabilities_warns() {
    let mut info = complete_package_meta();
    info.known_vulnerabilities_count = 2;
    let result = check_package_insecure("foo", &info);
    assert!(result.is_some());
    assert!(matches!(
      result.expect("result").status,
      CheckStatus::Warn(_)
    ));
  }

  // -- 4.2 check_per_system_output_gaps --------------------------------------

  #[test]
  fn package_missing_on_one_system_warns_naming_both() {
    let show_json = serde_json::json!({
      "packages": {
        "x86_64-linux": {
          "foo": { "type": "derivation", "name": "foo" },
          "bar": { "type": "derivation", "name": "bar" }
        },
        "aarch64-darwin": {
          "foo": { "type": "derivation", "name": "foo" }
        }
      }
    });

    let results = check_per_system_output_gaps(&show_json);
    assert_eq!(
      results.len(),
      1,
      "expected exactly one gap warning, got {results:?}"
    );
    let CheckStatus::Warn(ref msg) = results[0].status else {
      unreachable!()
    };
    assert!(msg.contains("bar"), "message should name the key: {msg}");
    assert!(
      msg.contains("aarch64-darwin"),
      "message should name the missing system: {msg}"
    );
    assert!(
      !msg.contains("foo"),
      "message should not mention the unaffected key: {msg}"
    );
  }

  #[test]
  fn package_present_on_all_declared_systems_does_not_warn() {
    let show_json = serde_json::json!({
      "packages": {
        "x86_64-linux": { "foo": { "type": "derivation" } },
        "aarch64-darwin": { "foo": { "type": "derivation" } }
      }
    });
    assert!(check_per_system_output_gaps(&show_json).is_empty());
  }

  #[test]
  fn gap_check_covers_apps_and_devshells_independently() {
    let show_json = serde_json::json!({
      "apps": {
        "x86_64-linux": { "myapp": { "type": "app" } },
        "aarch64-darwin": {}
      },
      "devShells": {
        "x86_64-linux": { "default": { "type": "derivation" } },
        "aarch64-darwin": { "default": { "type": "derivation" } }
      }
    });

    let results = check_per_system_output_gaps(&show_json);
    assert_eq!(
      results.len(),
      1,
      "expected exactly one gap (apps.myapp), got {results:?}"
    );
    let CheckStatus::Warn(ref msg) = results[0].status else {
      unreachable!()
    };
    assert!(msg.contains("myapp"), "{msg}");
    assert!(msg.contains("aarch64-darwin"), "{msg}");
  }

  #[test]
  fn no_relevant_categories_produce_no_gap_warnings() {
    let show_json = serde_json::json!({ "formatter": { "x86_64-linux": {} } });
    assert!(check_per_system_output_gaps(&show_json).is_empty());
  }

  // -- 4.3 check_checks_output_present ---------------------------------------

  #[test]
  fn missing_checks_key_warns() {
    let show_json = serde_json::json!({ "packages": {} });
    let result = check_checks_output_present(&show_json);
    assert!(
      matches!(result.status, CheckStatus::Warn(_)),
      "expected Warn, got {:?}",
      result.status
    );
  }

  #[test]
  fn empty_checks_object_warns() {
    let show_json = serde_json::json!({ "checks": {} });
    let result = check_checks_output_present(&show_json);
    assert!(
      matches!(result.status, CheckStatus::Warn(_)),
      "expected Warn, got {:?}",
      result.status
    );
  }

  #[test]
  fn checks_present_for_at_least_one_system_passes() {
    let show_json = serde_json::json!({
      "checks": {
        "x86_64-linux": { "my-check": { "type": "derivation" } }
      }
    });
    let result = check_checks_output_present(&show_json);
    assert!(
      matches!(result.status, CheckStatus::Ok(_)),
      "expected Ok, got {:?}",
      result.status
    );
  }

  // -- 4.4 check_nonstandard_output_keys -------------------------------------

  #[test]
  fn nonstandard_output_key_warns_informationally() {
    let show_json = serde_json::json!({
      "packages": {},
      "ociImages": {
        "x86_64-linux": { "my-image": { "type": "derivation" } }
      }
    });

    let results = check_nonstandard_output_keys(&show_json);
    assert_eq!(
      results.len(),
      1,
      "expected exactly one warning, got {results:?}"
    );
    let CheckStatus::Warn(ref msg) = results[0].status else {
      unreachable!()
    };
    assert!(msg.contains("ociImages"), "{msg}");
    assert!(
      msg.contains("not part of the conventional"),
      "message should be worded informationally: {msg}"
    );
  }

  #[test]
  fn all_standard_keys_produce_no_warnings() {
    let show_json = serde_json::json!({
      "packages": {},
      "apps": {},
      "devShells": {},
      "checks": {},
      "formatter": {},
      "nixosConfigurations": {}
    });
    assert!(check_nonstandard_output_keys(&show_json).is_empty());
  }

  #[test]
  fn multiple_nonstandard_keys_each_warn_independently() {
    let show_json = serde_json::json!({
      "packages": {},
      "ociImages": {},
      "customThing": {}
    });

    let results = check_nonstandard_output_keys(&show_json);
    assert_eq!(
      results.len(),
      2,
      "expected one warning per key, got {results:?}"
    );
  }

  // -- 5.1/5.2 group gating (SC-FD-013) --------------------------------------

  /// Write a minimal, self-contained (no external inputs, so no network
  /// access is needed to evaluate) flake fixture into `dir`:
  ///
  /// - no top-level `description`, which triggers the structure group's
  ///   `check_flake_description` warning via `nix flake metadata --json`.
  /// - an empty `outputs` set, which triggers the output group's
  ///   `check_checks_output_present` warning via `nix flake show --json` (no
  ///   `checks` output exists at all).
  fn write_group_gating_fixture(dir: &std::path::Path) {
    std::fs::write(
      dir.join("flake.nix"),
      r"{
  outputs = { self }: { };
}
",
    )
    .expect("write flake.nix");

    std::fs::write(
      dir.join("flake.lock"),
      r#"{
  "nodes": {
    "root": {}
  },
  "root": "root",
  "version": 7
}
"#,
    )
    .expect("write flake.lock");
  }

  #[test]
  fn disabling_structure_group_suppresses_only_its_warnings() {
    let dir = tempfile::tempdir().expect("create tempdir");
    write_group_gating_fixture(dir.path());
    let flake_ref = dir.path().to_string_lossy().to_string();

    let config = ProjectDoctorConfig {
      disable_structure_checks: true,
      ..Default::default()
    };
    let results = run_health_checks(dir.path(), &flake_ref, &config)
      .expect("run health checks");

    assert!(
      !results.iter().any(|r| r.name == "Flake description"),
      "structure group's description check must not run at all when the group \
       is disabled: {results:?}"
    );
    let checks_output =
      results.iter().find(|r| r.name == "Checks output").expect(
        "output group's checks-output check must still run when only the \
         structure group is disabled",
      );
    assert!(
      matches!(checks_output.status, CheckStatus::Warn(_)),
      "expected a warning for the missing `checks` output: {checks_output:?}"
    );
  }

  #[test]
  fn disabling_output_group_suppresses_only_its_warnings() {
    let dir = tempfile::tempdir().expect("create tempdir");
    write_group_gating_fixture(dir.path());
    let flake_ref = dir.path().to_string_lossy().to_string();

    let config = ProjectDoctorConfig {
      disable_output_checks: true,
      ..Default::default()
    };
    let results = run_health_checks(dir.path(), &flake_ref, &config)
      .expect("run health checks");

    let description = results
      .iter()
      .find(|r| r.name == "Flake description")
      .expect(
        "structure group's description check must still run when only the \
         output group is disabled",
      );
    assert!(
      matches!(description.status, CheckStatus::Warn(_)),
      "expected a warning for the missing top-level description: \
       {description:?}"
    );
    assert!(
      !results.iter().any(|r| r.name == "Checks output"),
      "output group's checks-output check must not run at all when the group \
       is disabled: {results:?}"
    );
  }

  /// Write the same minimal flake as [`write_group_gating_fixture`] (no
  /// top-level `description`), plus one `packages.<system>.foo` output
  /// that is a bare empty attrset  -  enough for `discover_package_meta`'s
  /// `pkg.meta or {}` / `pkg.version or null` / `pkg.pname or null` reads
  /// to resolve (all missing), without needing a real derivation or any
  /// build (the metadata check group's eval walk is IFD-free; see
  /// `show::PACKAGE_META_NIX`'s doc comment).
  fn write_metadata_group_gating_fixture(dir: &std::path::Path, system: &str) {
    std::fs::write(
      dir.join("flake.nix"),
      format!(
        r"{{
  outputs = {{ self }}: {{
    packages.{system}.foo = {{ }};
  }};
}}
"
      ),
    )
    .expect("write flake.nix");

    std::fs::write(
      dir.join("flake.lock"),
      r#"{
  "nodes": {
    "root": {}
  },
  "root": "root",
  "version": 7
}
"#,
    )
    .expect("write flake.lock");
  }

  /// SC-FD-013, taken directly from the proposal's own scenario: a flake
  /// with both a package missing `meta.description` and a missing
  /// top-level `description`; disabling the metadata group suppresses
  /// only the former.
  #[test]
  fn disabling_metadata_group_suppresses_only_its_warnings() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let system = crate::current_nix_system();
    write_metadata_group_gating_fixture(dir.path(), &system);
    let flake_ref = dir.path().to_string_lossy().to_string();

    // Baseline: with the metadata group enabled (the default), the
    // fixture's undocumented 'foo' package must actually trigger a
    // Package meta warning  -  otherwise the "suppressed" assertion below
    // would be vacuously true.
    let enabled_results = run_health_checks(
      dir.path(),
      &flake_ref,
      &ProjectDoctorConfig::default(),
    )
    .expect("run health checks (metadata enabled)");
    assert!(
      enabled_results.iter().any(|r| r.name == "Package meta"),
      "expected the fixture's undocumented 'foo' package to produce a Package \
       meta warning when the metadata group is enabled: {enabled_results:?}"
    );

    let config = ProjectDoctorConfig {
      disable_metadata_checks: true,
      ..Default::default()
    };
    let results = run_health_checks(dir.path(), &flake_ref, &config)
      .expect("run health checks (metadata disabled)");

    assert!(
      !results.iter().any(|r| r.name == "Package meta"),
      "metadata group's package-meta check must not run at all when the group \
       is disabled: {results:?}"
    );
    let description = results
      .iter()
      .find(|r| r.name == "Flake description")
      .expect(
        "structure group's description check must still run when only the \
         metadata group is disabled",
      );
    assert!(
      matches!(description.status, CheckStatus::Warn(_)),
      "expected a warning for the missing top-level description: \
       {description:?}"
    );
  }

  // -- [doctor.rules] per-rule overrides (apply_rule_overrides) --------
  //
  // Unlike the group-disable tests above, these exercise the pure
  // `apply_rule_overrides` function directly on hand-built `CheckResult`s
  // -- no flake fixture or subprocess needed, since the function only
  // ever looks at `code` and `status`.

  fn rules_with(
    pairs: impl IntoIterator<
      Item = (&'static str, crate::project_config::RuleSetting),
    >,
  ) -> ProjectDoctorConfig {
    ProjectDoctorConfig {
      rules: pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
      ..Default::default()
    }
  }

  #[test]
  fn rule_set_to_off_drops_the_result_entirely() {
    let config = rules_with([("x", crate::project_config::RuleSetting::Off)]);
    let results = vec![CheckResult {
      code:   "x",
      name:   "X",
      status: CheckStatus::Warn("whatever".to_string()),
    }];

    assert!(apply_rule_overrides(&config, results).is_empty());
  }

  #[test]
  fn rule_set_to_fail_escalates_a_warn() {
    let config = rules_with([("x", crate::project_config::RuleSetting::Fail)]);
    let results = vec![CheckResult {
      code:   "x",
      name:   "X",
      status: CheckStatus::Warn("message".to_string()),
    }];

    let overridden = apply_rule_overrides(&config, results);
    assert_eq!(overridden.len(), 1);
    assert!(
      matches!(overridden[0].status, CheckStatus::Fail(ref m) if m == "message")
    );
  }

  #[test]
  fn rule_set_to_warn_downgrades_a_fail() {
    let config = rules_with([("x", crate::project_config::RuleSetting::Warn)]);
    let results = vec![CheckResult {
      code:   "x",
      name:   "X",
      status: CheckStatus::Fail("message".to_string()),
    }];

    let overridden = apply_rule_overrides(&config, results);
    assert_eq!(overridden.len(), 1);
    assert!(
      matches!(overridden[0].status, CheckStatus::Warn(ref m) if m == "message")
    );
  }

  #[test]
  fn severity_override_is_a_no_op_on_an_ok_result() {
    let config = rules_with([("x", crate::project_config::RuleSetting::Fail)]);
    let results = vec![CheckResult {
      code:   "x",
      name:   "X",
      status: CheckStatus::Ok("fine".to_string()),
    }];

    let overridden = apply_rule_overrides(&config, results);
    assert_eq!(overridden.len(), 1);
    assert!(matches!(overridden[0].status, CheckStatus::Ok(_)));
  }

  #[test]
  fn override_on_one_code_does_not_affect_another() {
    let config = rules_with([("x", crate::project_config::RuleSetting::Off)]);
    let results = vec![
      CheckResult {
        code:   "x",
        name:   "X",
        status: CheckStatus::Warn("x warning".to_string()),
      },
      CheckResult {
        code:   "y",
        name:   "Y",
        status: CheckStatus::Warn("y warning".to_string()),
      },
    ];

    let overridden = apply_rule_overrides(&config, results);
    assert_eq!(overridden.len(), 1);
    assert_eq!(overridden[0].code, "y");
  }
}
