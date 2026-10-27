# xi sources: on-disk state layout research

Scope: pick canonical paths for three concepts the `xi sources` feature
introduces - the **sources dir** (xi-managed clone of the user's config repo),
the **state dir** (metadata: last sha, trust, generations, lock), and the
**working dir** (user's own editable clone, pointer kept by xi so
`xi system switch` works from any CWD). Decisions must hold for NixOS system
scope, nix-darwin system scope, home-manager scope, and standalone-user scope.

## 1. Primary standards

### XDG Base Directory Specification

([specifications.freedesktop.org/basedir/latest](http://specifications.freedesktop.org/basedir/latest/))

| Var               | Default              | Purpose (quote)                                                                                                                                                                                                      |
| ----------------- | -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `XDG_CONFIG_HOME` | `$HOME/.config`      | "user-specific configuration files"                                                                                                                                                                                  |
| `XDG_DATA_HOME`   | `$HOME/.local/share` | "user-specific data files"                                                                                                                                                                                           |
| `XDG_STATE_HOME`  | `$HOME/.local/state` | "state data persisting between application restarts but not essential enough for data storage"; includes "actions history (logs, history, …)" and "current state of the application that can be reused on a restart" |
| `XDG_CACHE_HOME`  | `$HOME/.cache`       | "non-essential cached data"                                                                                                                                                                                          |

The distinction that matters here: **DATA = user content you'd back up and lose
sleep over; STATE = runtime bookkeeping you can regenerate but want preserved
across restarts; CACHE = re-fetchable.** A git clone the daemon manages sits
uncomfortably between data and cache - it is re-fetchable from the remote but
losing it between runs would mean a full re-clone, which argues for STATE or
DATA, not CACHE.

### FHS 3.0 (system scope)

FHS 3.0 §5.8 (`/var/lib`): "variable state information for programs"; packages
must use a subdirectory `/var/lib/<name>`. `/var/cache` is "application cache
data"; "files SHOULD be able to be deleted without loss of data" (so clones here
are legal but regenerable). `/etc` is for host-specific **static** config - a
daemon-managed git clone is not static.

### systemd exec directories

([freedesktop.org/software/systemd/man/systemd.exec.html](https://www.freedesktop.org/software/systemd/man/systemd.exec.html))

| Setting                     | System service  | User service          | Env                        |
| --------------------------- | --------------- | --------------------- | -------------------------- |
| `StateDirectory=xi`         | `/var/lib/xi`   | `$XDG_STATE_HOME/xi`  | `$STATE_DIRECTORY`         |
| `CacheDirectory=xi`         | `/var/cache/xi` | `$XDG_CACHE_HOME/xi`  | `$CACHE_DIRECTORY`         |
| `ConfigurationDirectory=xi` | `/etc/xi`       | `$XDG_CONFIG_HOME/xi` | `$CONFIGURATION_DIRECTORY` |

Key behaviour: directories are auto-created with correct ownership/mode and the
full path is exposed via env. **If xi ships a systemd unit, these are what we
want to pass through** - the service never hard-codes `/var/lib/xi`, it reads
`$STATE_DIRECTORY`.

## 2. Prior art survey

### comin (NixOS pull-based deployer)

Source: `~/ghq/github.com/nlewo/comin`. The NixOS module (`nix/module.nix`)
ships a `systemd.services.comin` unit that sets no explicit `StateDirectory=` -
comin's Go code (`internal/repository/git.go` references `config.Path`,
`internal/executor/nix_flake.go` holds `repositoryPath string`). The repo path
is user-configured via `services.comin.git.path`-style options, with the systemd
service's `/var/lib/comin` as the de-facto convention. Takeaway: comin treats
the clone as a _configurable state path_, defaulted via the standard "systemd
system-service state dir" idiom.

### chezmoi

Source: `~/ghq/github.com/twpayne/chezmoi/internal/cmd/config.go:1244-1264`
(`defaultSourceDir`): walks `XDG_DATA_HOME` + `XDG_DATA_DIRS` and falls back to
`~/.local/share/chezmoi`. State DB (`chezmoistate.boltdb`) lives _next to the
config file_ (`config.go:342`, `:2720-2731`) - i.e.
`~/.config/chezmoi/chezmoistate.boltdb`, **not** `XDG_STATE_HOME`, which is a
known chezmoi quirk the maintainer kept for backwards compat
([chezmoi variables ref](https://www.chezmoi.io/reference/configuration-file/variables/)).
Working-dir discovery: chezmoi has a single source dir, resolved from config;
`chezmoi apply` from any CWD works because it reads
`~/.config/chezmoi/chezmoi.toml` first.

Model: **source = `XDG_DATA_HOME/chezmoi`** (data, user owns it), **state = next
to config** (legacy), **config = `XDG_CONFIG_HOME/chezmoi/chezmoi.toml`**.

### yadm

Source: `~/ghq/github.com/TheLocehiliosan/yadm/yadm:1666-1681`. `YADM_DATA`
defaults to `$XDG_DATA_HOME/yadm`, `YADM_DIR` (config + bare repo) defaults to
`$XDG_CONFIG_HOME/yadm`. Legacy fallback `~/.yadm`. Same split as chezmoi: data
under DATA_HOME, config/metadata under CONFIG_HOME.

### ansible-pull

Clones to `~/.ansible/pull/<playbook-name>` by default; `-d/--directory`
overrides
([manpages.debian.org - ansible-pull](https://manpages.debian.org/unstable/ansible-core/ansible-pull.1.en.html)).
This predates widespread XDG adoption and is now considered non-conformant;
still useful as a counter-example of what xi should **not** do (dot-directory in
`$HOME`).

### ArgoCD repo-server

Clones to `/tmp/_argocd-repo` by default (configurable via `TMPDIR`), backed by
an `emptyDir` so clones re-fetch on pod restart
([argo-cd.readthedocs.io - HA ops](https://argo-cd.readthedocs.io/en/stable/operator-manual/high_availability/)).
xi can't replicate this (we're not in a pod) but the lesson is: **treat the
managed clone as regenerable but prefer persistence to avoid cold-start cost.**
That lands on STATE, not CACHE.

### nix-darwin

System config conventionally lives at
`/etc/nix-darwin/{flake.nix,configuration.nix}`
([nix-darwin manual](https://nix-darwin.github.io/nix-darwin/manual/)). The
flake path is user-authored, not daemon-managed - darwin-rebuild just reads it.
nothing writes a clone back.

### NixOS

`/etc/nixos/{flake.nix,configuration.nix}` is the historical anchor
([wiki.nixos.org - Nixos-rebuild](https://wiki.nixos.org/wiki/Nixos-rebuild)).
Generations are tracked under `/nix/var/nix/profiles/system-*` and the live
symlink is `/run/current-system`. Note: `/etc/nixos` is **user-authored static
config**, not a daemon-managed clone, so FHS-wise it fits `/etc`.

## 3. Recommended layout

| Concept                                                                   | NixOS system                                                        | nix-darwin system     | home-manager                                       | Standalone user                                    | Rationale                                                                                                                                                                                                                                                  |
| ------------------------------------------------------------------------- | ------------------------------------------------------------------- | --------------------- | -------------------------------------------------- | -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **sources** (xi-managed clone)                                            | `/var/lib/xi/sources`                                               | `/var/lib/xi/sources` | `${XDG_DATA_HOME:-$HOME/.local/share}/xi/sources`  | `${XDG_DATA_HOME:-$HOME/.local/share}/xi/sources`  | System: FHS §5.8 "variable state", + systemd `StateDirectory=xi/sources` auto-creates it. User: chezmoi/yadm convention - the clone is data-the-user-owns.                                                                                                 |
| **state** (sha, trust DB, lock, generations map, working-dir pointer)     | `/var/lib/xi/state`                                                 | `/var/lib/xi/state`   | `${XDG_STATE_HOME:-$HOME/.local/state}/xi`         | `${XDG_STATE_HOME:-$HOME/.local/state}/xi`         | System: colocated under same `StateDirectory=xi` to keep ownership/backup atomic. User: XDG spec explicitly names "current state of the application that can be reused on a restart" - this is the textbook case. Avoid chezmoi's XDG_CONFIG_HOME mistake. |
| **config** (user-authored xi settings: remote URL, trust policy, backend) | `/etc/xi/config.toml`                                               | `/etc/xi/config.toml` | `${XDG_CONFIG_HOME:-$HOME/.config}/xi/config.toml` | `${XDG_CONFIG_HOME:-$HOME/.config}/xi/config.toml` | systemd `ConfigurationDirectory=xi` for system; XDG for user. Static, hand-edited, survives uninstall.                                                                                                                                                     |
| **working dir** (user's editable clone)                                   | pointer in state, path user-chosen (e.g. `~user/ghq/.../xi-config`) | same                  | same                                               | same                                               | xi records the absolute path in `state/working.toml`; discovery is "look up in state", not "search the filesystem". Mirrors chezmoi's "config names the source" model but at a layer up: config names the _working_ repo, state names the _managed_ clone. |
| **cache** (nix eval cache, git packfile scratch)                          | `/var/cache/xi`                                                     | `/var/cache/xi`       | `${XDG_CACHE_HOME:-$HOME/.cache}/xi`               | same                                               | FHS: deletable without loss. systemd `CacheDirectory=xi`.                                                                                                                                                                                                  |

Resulting system-service unit stanza is just:
`StateDirectory=xi/sources xi/state`, `CacheDirectory=xi`,
`ConfigurationDirectory=xi`. No hard-coded paths in the Rust code - read
`$STATE_DIRECTORY`, `$CACHE_DIRECTORY`, `$CONFIGURATION_DIRECTORY`, fall back to
the computed XDG/FHS path when running outside systemd.

## 4. Trade-offs considered

- **`/var/lib` vs `/etc/xi` for sources (system).** `/etc` is for static config
  (FHS §5); a daemon that rewrites the tree on every pull violates that. Picked
  `/var/lib`.
- **`/var/lib` vs `/nix/var/xi` for sources (system).** `/nix/var` is
  Nix-daemon-private; putting xi state there couples uninstall semantics to Nix
  and confuses `nix-collect-garbage` operators. Picked `/var/lib`.
- **XDG_DATA_HOME vs XDG_STATE_HOME for sources (user).** The clone contains the
  user's config repo content - losing it isn't catastrophic (re-clone from
  remote) but it is semantically "their stuff", matching chezmoi/yadm. Picked
  DATA_HOME. Keeping state (sha, trust, lock) separately under STATE_HOME means
  a user can `rm -rf ~/.local/state/xi` to reset metadata without losing the
  clone.
- **XDG_CACHE_HOME for sources?** Rejected: cache can be purged by the OS at any
  moment; a re-clone every boot is expensive and race-prone with a watcher
  daemon.
- **macOS.** XDG is not Apple-blessed but `dirs-next` / `directories-rs` on
  macOS maps `XDG_DATA_HOME`→`~/Library/Application Support`,
  `XDG_STATE_HOME`→`~/Library/Application Support` (no distinct state),
  `XDG_CACHE_HOME`→`~/Library/Caches`. Chezmoi, yadm, and gh deliberately
  override that and **use XDG paths literally** on macOS so dotfile repos are
  portable. xi should do the same: honour `$XDG_*` env, fall back to
  `~/.local/share|state|config|cache/xi` on macOS too, **not** `~/Library/...`.
  nix-darwin system scope still uses `/var/lib/xi` - Darwin does have `/var/lib`
  and systemd-equivalent dirs are irrelevant since nix-darwin uses launchd,
  which has no `StateDirectory=` analogue (xi will have to create the dir itself
  with correct mode).

## 5. Multi-user safety

User scope paths all live under `$HOME`, so two home-manager users never
collide. System scope is single-tenant by definition (one `/var/lib/xi`, one
systemd unit, one trust DB) - this is correct for NixOS/darwin where the system
config is a machine-wide artefact. If two users on the same machine both want
`xi home switch` with their own repos, they each get their own
`~/.local/state/xi/working.toml` pointer and
`~/.local/share/xi/sources/<remote-hash>/` clone - the `<remote-hash>` subdir
prevents accidental overlap if the same remote appears in multiple user configs.
(Comin and chezmoi both lack this because they assume one repo per scope; xi
should design for N.)

## 6. Working-dir discovery

The user requirement - "run `xi system switch` from any CWD" - maps directly to
chezmoi's model: **config names the source**, CLI reads config first, resolves
paths second. Concretely:

1. xi reads `$XDG_CONFIG_HOME/xi/config.toml` (user) or `/etc/xi/config.toml`
   (system). This file names the remote and optionally the local working repo
   path (`working_dir = "/home/alice/ghq/.../my-nix-config"`).
2. If `working_dir` is set and exists, `xi X switch` operates against it (fast
   path, no fetch).
3. If unset or stale, xi uses the managed clone under `sources/` and refreshes
   from the remote.
4. State dir records last-used sha + a resolved working-dir pointer for
   debugging (`xi state show`).

This is the inverse of ansible-pull's "give me a URL on every invocation" and
inherits chezmoi's ergonomic win: zero flags for daily use.

## 7. Open questions

- **Multi-remote per scope.** Chezmoi and comin assume one source repo per
  scope. Should xi allow `[sources.work]` + `[sources.personal]` subprofiles,
  and if so does each get its own `sources/<name>/` + `state/<name>/` subtree?
- **Trust DB format.** sqlite (chezmoi's bolt) vs flat TOML vs sops-encrypted
  TOML. Impacts whether `state/` can be world-readable or must be mode 0700.
- **Working-dir vs sources-dir sha drift.** If the user hand-edits their working
  dir and runs `xi system switch`, do we deploy their uncommitted tree, warn, or
  refuse? Comin refuses (bare repo only); chezmoi deploys the working tree. Pick
  one and document.
- **macOS launchd parity.** Should xi ship a `~/Library/LaunchAgents/xi.plist`
  that writes to `~/.local/state/xi` (XDG-literal) or to
  `~/Library/Application Support/xi` (Apple-native)? Portability vs convention.
- **Garbage collection of old `sources/<remote-hash>/` trees.** When a user
  removes a remote from config, do we auto-delete the clone, require
  `xi sources prune`, or leave it forever? systemd `StateDirectory=` never
  auto-cleans.

## Sources

- [XDG Base Directory Specification](http://specifications.freedesktop.org/basedir/latest/)
- [systemd.exec(5) - StateDirectory=, CacheDirectory=, ConfigurationDirectory=](https://www.freedesktop.org/software/systemd/man/systemd.exec.html)
- [chezmoi source `internal/cmd/config.go` defaultSourceDir](/home/juliendauliac/ghq/github.com/twpayne/chezmoi/internal/cmd/config.go)
- [chezmoi configuration variables](https://www.chezmoi.io/reference/configuration-file/variables/)
- [yadm source `yadm` script path resolution](/home/juliendauliac/ghq/github.com/TheLocehiliosan/yadm/yadm)
- [comin NixOS module `nix/module-options.nix`](/home/juliendauliac/ghq/github.com/nlewo/comin/nix/module-options.nix)
- [ansible-pull(1) Debian manpage](https://manpages.debian.org/unstable/ansible-core/ansible-pull.1.en.html)
- [ArgoCD operator manual - repo-server HA](https://argo-cd.readthedocs.io/en/stable/operator-manual/high_availability/)
- [nix-darwin manual](https://nix-darwin.github.io/nix-darwin/manual/)
- [NixOS wiki - nixos-rebuild](https://wiki.nixos.org/wiki/Nixos-rebuild)
