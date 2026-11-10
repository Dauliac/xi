//! Resolve the sources context a push deploys from, before anything
//! else runs.
//!
//! `xi push` deploys whatever the sources model ([[xi-sources-model]])
//! says is deployed for its scope, not an ad hoc flake path the operator
//! happens to be standing in. Before discovery or any mutation, the push
//! has to load that context and refuse outright if there's nothing to
//! push from: `xi-sources-push` tasks.md §2.2, "resolve sources context
//! before dispatch; refuse if sources are `Missing`".

use std::collections::BTreeMap;

use color_eyre::{Result, eyre::bail};
use xi_core::sources::{
  GitopsHealth,
  Scope,
  SourcesContext,
  SourcesId,
  SourcesStatus,
  args::resolve_layers,
  config::{ScopePaths, SourcesConfig},
  read_state_file,
  secrets::SecretSpec,
};

/// Resolve the sources context `scope` deploys from.
///
/// Mirrors `xi sources status`'s remote resolution (config ladder, then
/// the state dir's recorded `remote-url`, so a machine adopted via
/// `xi deploy --adopt <remote>` without a `.xi.toml` still resolves) so
/// a push sees exactly the source `xi sources status` would report for
/// the same scope.
///
/// # Errors
///
/// Refuses with an error naming every sanctioned way to configure a
/// sources repo when `scope` has none configured
/// ([`SourcesStatus::Missing`]). Propagates config-load and state-dir
/// I/O errors otherwise.
pub fn resolve_context(scope: Scope) -> Result<SourcesContext> {
  resolve_context_at(scope, &ScopePaths::resolve(scope))
}

/// [`resolve_context`] over an explicit [`ScopePaths`] table, so a test
/// can point every path at a scratch dir instead of mutating the
/// process environment `ScopePaths::resolve` reads from  -  which no
/// parallel test can do safely (the same reason `ConfPaths::resolve_with`
/// and `ScopePaths::resolve_with` elsewhere in `xi-core` take a lookup
/// closure rather than reading `std::env` directly).
fn resolve_context_at(
  scope: Scope,
  paths: &ScopePaths,
) -> Result<SourcesContext> {
  let resolved = resolve_layers(SourcesConfig::default(), paths)?;

  let remote = match resolved.remote {
    Some(remote) => remote,
    None => {
      read_state_file(&paths.state_dir, "remote-url")?.unwrap_or_default()
    },
  };
  let sources_id = SourcesId::new(scope, &remote);

  let context = SourcesContext::load(
    sources_id,
    paths.sources_dir.clone(),
    paths.state_dir.clone(),
  )?;

  if let SourcesStatus::Missing { configure_with } =
    context.status(&GitopsHealth::Disabled)
  {
    let hints = configure_with
      .iter()
      .map(|hint| {
        format!("  - {} ({:?}): {}", hint.name, hint.kind, hint.detail)
      })
      .collect::<Vec<_>>()
      .join("\n");
    bail!(
      "no sources configured for the {scope} scope; `xi push` deploys from \
       the sources model, so there is nothing to push yet. Configure one \
       of:\n{hints}"
    );
  }

  Ok(context)
}

/// The `[sources.secrets.<name>]` stanzas visible to `scope`, merged
/// across the same ladder [`resolve_context`] reads (`.xi.toml` in the
/// current directory over the Nix-module-written per-scope config).
///
/// Separate from [`resolve_context`] because [`SourcesContext`] has no
/// room for a secret map - it is the on-disk *model* (refs, dirs), not
/// configuration - and duplicating the (cheap, file-read-only) layer
/// resolution here is simpler than growing that struct for one caller.
///
/// # Errors
///
/// Propagates a `.xi.toml` or Nix-module config parse error.
pub fn resolve_secrets(scope: Scope) -> Result<BTreeMap<String, SecretSpec>> {
  let paths = ScopePaths::resolve(scope);
  let resolved = resolve_layers(SourcesConfig::default(), &paths)?;
  Ok(resolved.secrets)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::fs;

  use tempfile::TempDir;

  use super::*;

  /// A scratch [`ScopePaths`] table, standing in for the real
  /// filesystem and process environment the same way `xi-core`'s own
  /// `*_with` config tests do: every path lives under a `TempDir`
  /// instead of `$XDG_STATE_HOME` et al, so no test here has to mutate
  /// process-global env vars (unsafe under parallel `cargo test`).
  struct Machine {
    _root: TempDir,
    paths: ScopePaths,
  }

  impl Machine {
    fn new(with_sources_dir: bool) -> Self {
      let root = TempDir::new().expect("tempdir");
      let sources_dir = root.path().join("sources");
      let state_dir = root.path().join("state");
      let config_file = root.path().join("sources.toml");
      fs::create_dir_all(&state_dir).expect("state dir");
      if with_sources_dir {
        fs::create_dir_all(&sources_dir).expect("sources dir");
      }
      Self {
        _root: root,
        paths: ScopePaths {
          sources_dir,
          state_dir,
          config_file,
        },
      }
    }
  }

  #[test]
  fn a_missing_sources_dir_reports_missing_status() {
    let machine = Machine::new(false);
    let id = SourcesId::new(Scope::System, "https://example/repo");
    let status = SourcesContext::load(
      id,
      &machine.paths.sources_dir,
      &machine.paths.state_dir,
    )
    .expect("load context")
    .status(&GitopsHealth::Disabled);
    assert!(
      matches!(status, SourcesStatus::Missing { .. }),
      "got: {status:?}"
    );
  }

  #[test]
  fn resolve_context_at_refuses_when_the_sources_dir_is_missing() {
    let machine = Machine::new(false);
    let err = resolve_context_at(Scope::System, &machine.paths)
      .expect_err("no sources dir exists under the scratch root");
    let message = format!("{err}");
    assert!(message.contains("no sources configured"), "got: {message}");
    assert!(message.contains("system"), "got: {message}");
    assert!(message.contains("XI_SOURCES_REMOTE"), "got: {message}");
    assert!(message.contains("deploy --adopt"), "got: {message}");
  }

  #[test]
  fn resolve_context_at_succeeds_once_a_sources_dir_exists() {
    let machine = Machine::new(true);
    let context = resolve_context_at(Scope::User, &machine.paths)
      .expect("sources dir exists, so this must not refuse");
    assert_eq!(context.sources_dir, machine.paths.sources_dir);
    assert!(
      !matches!(
        context.status(&GitopsHealth::Disabled),
        SourcesStatus::Missing { .. }
      ),
      "a present sources dir must never report Missing"
    );
  }

  #[test]
  fn resolve_context_at_falls_back_to_the_recorded_remote_url() {
    // No `.xi.toml`, no env var, no nix-module `sources.toml` - the only
    // remote this scenario can resolve through is the state dir's
    // `remote-url` file, the same fallback `xi sources status` uses for
    // a machine adopted via `xi deploy --adopt <remote>`.
    let machine = Machine::new(true);
    fs::write(
      machine.paths.state_dir.join("remote-url"),
      "https://example/adopted\n",
    )
    .expect("write remote-url");

    let context = resolve_context_at(Scope::System, &machine.paths)
      .expect("resolves via the state-dir fallback");
    assert_eq!(context.sources_id.canonical_url, "https://example/adopted");
  }
}
