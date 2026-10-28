# Change: xi-sources-model

## Why

`xi` currently has a per-flake devshell daemon, per-platform `switch`
subcommands (`xi nixos switch`, `xi home switch`, `xi darwin switch`,
`xi system switch`), and a pluggable `xi-deploy` crate. What it does not have is
a **data model for the user's config source repo** that is shared across all
four surfaces (local switch, remote push, pull-mode GitOps, curl-pipe-sh
bootstrap).

Every one of those surfaces answers the same three questions today in a
different place: _which clone of the config repo does this command operate on?_,
_where does its state live?_, and _how does the local working copy relate to the
remote?_ Without a single named data model, every subcommand has to re-derive
the answer and the user has to re-explain their intent.

The user wants a universal entry point: run `xi system switch` from any CWD, and
the tool knows (a) where the local working copy is, (b) which pinned ref is
currently "deployed", (c) which remote the GitOps daemon is tracking. Then other
features (bootstrap, push, gitops) are just verbs over the same model.

## What Changes

- Introduce capability `xi-sources-model` that owns three named concepts for
  every scope (`nixos` / `darwin` / `home` / `standalone-user`):

  1. **Sources dir** - a managed clone of the config repo, owned by the daemon
     when gitops is enabled, read-only to the user. This is the canonical
     "what's deployed" view.
  2. **State dir** - metadata only: last-synced sha, trust decisions,
     generations map, lockfile, pointer to the working dir. Never contains
     source code.
  3. **Working dir** - the user's _optional_ editable clone (where they do
     `git commit`). Path is explicit (`--working-dir`,
     `[sources]
     working-dir`, `XI_SOURCES_WORKING_DIR`) and recorded in the
     state dir so `xi system switch` run from any CWD can locate it.

- Define the **ref lineage** state machine: `remote-head` → `working-head` →
  `sources-head` → `active-generation`. Every xi command reports which pair of
  these are divergent; conflict resolution rules are owned by
  [[xi-sources-conflict]].

- Name the **sanctioned write flows** between the three on-disk concepts. These
  are the only directions xi code is allowed to modify a user-visible tree:

  - remote `→` sources dir, via the gitops daemon's fetch
    ([[xi-sources-gitops]])
  - sources dir `→` working dir, via the hermetic chezmoi-style sync
    ([[xi-sources-conflict]], `xi sources sync`)
  - working dir `→` remote, via the user's own `git push` (xi does not wrap
    this)

  There is no sanctioned write from the working dir to the sources dir, and no
  sanctioned write from either sources or working dir back to the remote by xi.
  This set is deliberately minimal; adding a new flow requires amending this
  spec.

- Define a **scope resolver** that picks system vs user based on the invoking
  command (`xi system|nixos|darwin switch` → system scope; `xi home switch` →
  user scope; `xi switch` with no verb is ambiguous and errors out).

- Define a **sources identity** keyed by `(scope, canonical-remote-url)` so the
  same repo cloned into two paths on the same machine is one logical source, not
  two. Mirrors the `flake_id` pattern established by
  `xi-devshell-flake-discovery` and `xi-devshell-trust`.

- Add a read-only machine surface `xi sources status` returning the full model
  as a `{schema version, status Ready|Pending|Missing|Degraded}` envelope
  matching the house convention (`xi-core` response types).

## Impact

- **Affected specs**: `xi-sources-model` (new capability).
- **Affected code (planned)**: new crate `crates/xi-sources/` carrying the model
  types; new call site in each `*/switch` command resolving the sources context
  before invoking the backend; integration with
  `crates/xi-core/src/flake_output.rs` taxonomy for the
  `nixosConfigurations
  / darwinConfigurations / homeConfigurations` lookup.
- **Coordination**: downstream changes `xi-sources-config`,
  `xi-sources-conflict`, `xi-sources-gitops`, `xi-sources-push`,
  `xi-sources-bootstrap`, `xi-sources-cache-prestage` all depend on this model.
  Each spec cross-links its state transitions to the ones defined here.
- **Breaking?**: No. `xi {nixos,home,darwin,system} switch <installable>`
  continues to work with an inline installable. The sources resolver only
  activates when the user has configured a sources repo OR when the installable
  argument is a URL.
- **Migration**: None for existing users who pass an explicit installable. Users
  who want the "run from any CWD" ergonomic opt in via
  `xi sources init <remote-url>`.

## Success Criteria

- **SC-M-001**: `xi sources status` on a machine with no sources configured
  returns `status: Missing` and names which env/config/flag could configure it.
- **SC-M-002**: `xi sources status` on a machine with sources configured returns
  `status: Ready` carrying
  `(sources_dir, state_dir, working_dir?,
  remote_head, working_head, sources_head, active_generation)`.
- **SC-M-003**: Two different scopes on the same machine (`system` and `home`)
  tracking the same remote URL produce two independent sources identities and
  two independent state dirs.
- **SC-M-004**: `xi home switch` run from any CWD (including `/`) succeeds when
  a user-scope sources repo is configured and fails cleanly otherwise.
- **SC-M-005**: The model exposes a canonical `sources_id` that is stable across
  machine reboots, hostname changes, and user renames (keyed only on canonical
  remote URL + scope).
- **SC-M-006**: Switching scopes without a configured sources repo returns the
  SAME error-kind as calling an unconfigured subcommand - not a crash, not a
  silent default.
