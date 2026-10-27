# Change: xi-sources-bootstrap

## Why

The user asked for a one-liner that turns a bare machine into a running
NixOS/darwin/home-manager host managed by `xi`: "boostrap a machine with a curl
| sh that: download xi in a tmp dir, and run xi system switch
https:mynixRepo.git to install a fresh machin with just a curl".

The bootstrap research
(`openspec/research/xi-sources/
bootstrap-curl-pipe-sh.md`) found that the field
has converged on a two-stage pattern: a thin shell stub does OS/arch detection,
downloads a signed binary, and `exec`s it; the binary carries all planners,
receipts, and rollback. Determinate's `nix-installer`, Lix's installer, rustup,
and Starship all follow this pattern. The official Nix script is the only one
with inline SHA-256 pinning. nixos-anywhere is the only tool that solves the
cache-prestage chicken-and-egg (now owned by [[xi-sources-cache-prestage]]).

This change wires those four lessons together into a single xi bootstrap flow
that is reversible, honours the user's remote-cache config BEFORE the first
build, and composes with the rest of the sources model.

## What Changes

- Introduce capability `xi-sources-bootstrap` that owns:
  - A **shell stub** `bootstrap.sh` hosted at a stable URL (e.g.
    `install.xi.dev`) that:
    - Detects OS/arch via `uname -s` / `uname -m` with Rosetta disambiguation
      (steal from `nix-installer.sh:145-237`).
    - Enforces `curl --proto '=https' --tlsv1.2`; `wget` and `fetch` fallback
      chain (steal from `nix-installer.sh:298-397`).
    - Reattaches `/dev/tty` for interactive flows when piped (`[ ! -t 0
      ]`
      → `< /dev/tty`) - this is the universal rule from
      `nix-installer.sh:103-116`.
    - Downloads the `xi` binary from GitHub Releases.
    - Verifies the binary against an **inline SHA-256** templated at release
      time (steal from official `install.in:120-122`; this is the one place the
      field disagrees and xi picks the safer option).
    - `chmod +x` and `exec xi system switch "$@"` (or `xi home switch`, or
      `xi darwin switch`, depending on invocation).
  - A **`xi system switch <remote-url>`** path that recognises a URL (not a
    local flake) argument and triggers the full bootstrap sequence:
    1. **Preflight** - check not-already-xi-managed (receipt absent); check sudo
       access; check the machine's OS matches the declared scope; detect SELinux
       / WSL1 / ostree variants (steal from
       `nix-installer src/planner/linux.rs:184-235`).
    2. **Nix install** (only if `nix` is absent) via one of two pluggable
       installer backends, selected by `--installer` or by
       `xi.sources.bootstrap.installer` in the module:
       - `installer = "detsys"` (default on non-NixOS Linux and macOS):
         shell-exec the Determinate Systems `nix-installer`. Reference command:
         `curl -LsSf https://install.determinate.systems/nix
         | sh -s -- install --no-confirm --determinate=false`.
       - `installer = "lix"` (recommended when the user's declared substituters
         list includes `cache.lix.systems`): shell-exec the Lix installer, which
         is a Determinate Systems fork configured for the Lix daemon and the Lix
         cache. Reference:
         `curl -sSf -L https://install.lix.systems/lix | sh -s --
         install --no-confirm`.
       - `installer = "none"` (opt-out): skip the phase and resume at phase 3;
         useful when a configuration-management system has already installed
         Nix. On success, record `nix-installed-by: <backend>` in the receipt so
         `xi system uninstall` can call the matching uninstaller. On failure, xi
         exits non-zero with no partial state. The planner complexity (SELinux,
         WSL, APFS) stays in the delegated installer; xi does not vendor it.
    3. **Clone sources** - git clone the remote URL into the sources dir (per
       [[xi-sources-config]] path table). On a brand-new machine the state dir
       is also created at this step.
    4. **Cache prestage** - delegate to [[xi-sources-cache-prestage]] to
       configure substituters BEFORE the first build. Fatal unless
       `--no-prestage` is passed; a machine without a cache config can still
       proceed slowly.
    5. **First switch** - invoke the backend
       (`nixos-rebuild switch --flake <sources-dir>#<host>` /
       `darwin-rebuild switch` / `home-manager switch`). Build and activation.
    6. **Optional: enable gitops** - if `--enable-gitops` is passed OR the
       module declares `xi.sources.gitops.enable = true`, enable the daemon
       ([[xi-sources-gitops]]) after the first successful switch.
    7. **Write receipt** - `/var/lib/xi/bootstrap-receipt.json` listing every
       reversible action for `xi system uninstall`.
  - A **`xi system switch <ssh://user@host> <remote-url>`** path for
    bootstrapping a remote machine over SSH: xi copies itself to the remote
    machine via `scp`, exec's the same flow on the far side. This composes with
    [[xi-sources-push]]'s `--phase install` by sharing the planner.
  - A **`xi system uninstall`** path that reverses every receipt entry in
    reverse order, including the DetSys installer call if xi installed Nix.

## Impact

- **Affected specs**: `xi-sources-bootstrap` (new capability).
- **Affected code (planned)**: `scripts/bootstrap.sh` (shell stub);
  `crates/xi/src/bootstrap.rs` (planner, receipt types); release pipeline
  templates the sha256 into `bootstrap.sh` and uploads both the sh and the
  binary to a release asset bucket.
- **Coordination**: Phase 4 is [[xi-sources-cache-prestage]]; Phase 5 reuses the
  standard `switch` dispatch; Phase 6 enables [[xi-sources-gitops]]; the
  remote-SSH path shares a planner with [[xi-sources-push]].
- **Breaking?**: No. `xi system switch <local-flake>` continues to work
  unchanged; the URL-recognising code path is additive.
- **Migration**: None.

## Success Criteria

- **SC-B-001**:
  `curl install.xi.dev | sh -s -- system switch
  https://git.example/dotfiles`
  on a fresh NixOS install succeeds and produces a running host managed by xi.
- **SC-B-002**: The shell stub refuses to run if its inline SHA-256 does not
  match the downloaded binary.
- **SC-B-003**: Preflight on an already-xi-managed machine refuses without
  `--force`; receipt is consulted.
- **SC-B-004**: When `nix` is absent and `installer = "detsys"`, the Determinate
  Systems installer is invoked via shell exec; on exit code 0 bootstrap
  continues; on non-zero, xi exits non-zero and the receipt records no actions
  taken.
- **SC-B-004b**: When `nix` is absent and `installer = "lix"`, the Lix installer
  is invoked instead; the receipt records `nix-installed-by: lix`.
- **SC-B-004c**: When `nix` is present, both installer backends are skipped
  regardless of the `installer` setting.
- **SC-B-005**: Cache prestage runs between clone and first switch; the first
  build consults the user's declared substituters.
- **SC-B-006**: Receipt at `/var/lib/xi/bootstrap-receipt.json` lists each
  action with its timestamp and reversal command; `xi system
  uninstall`
  replays them in reverse.
- **SC-B-007**: Remote SSH form
  (`xi system switch
  ssh://user@host https://git.example/dotfiles`) copies xi
  to the target, exec's the same planner there, and streams logs back.
- **SC-B-008**: `XI_BOOTSTRAP_NO_CONFIRM=1` suppresses interactive prompts;
  absent, the planner prints its plan and asks for confirmation.
- **SC-B-009**: The shell stub honours `XI_BOOTSTRAP_URL_OVERRIDE` so users can
  host their own mirror.
