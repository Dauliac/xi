# Bootstrap / `curl | sh` patterns for Nix machines

Research for the `xi system switch https://my.repo.git` one-liner bootstrap
flow. Primary sources only; URL or `path:line` citations.

## 0. Convergent patterns (universal rules, 2026)

1. **Two-stage bootstrap.** The shell script is a stub: it detects OS/arch,
   picks a binary from a known CDN, downloads it to `mktemp -d`, `chmod +x`, and
   `exec`s it. All real logic (planners, actions, receipts, rollback) lives in
   the compiled binary. Rustup, Starship, DetSys `nix-installer`, Lix installer
   all follow this. Reference:
   [DetSys `nix-installer.sh:35-124`](https://github.com/DeterminateSystems/nix-installer/blob/main/nix-installer.sh) -
   the whole file is literally derived from `rustup-init.sh`, see comment at
   `nix-installer.sh:14`.
2. **TTY handoff for `curl | sh`.** When piped, stdin is the pipe, not the user,
   so interactive prompts die silently. Universal fix: detect `[ ! -t 0 ]` and
   re-attach `< /dev/tty` for the child invocation. See
   `nix-installer.sh:103-116`.
3. **Downloader fallback and TLS pinning.** Prefer `curl` → `wget` → `fetch`.
   Enforce `--proto '=https' --tlsv1.2` and probe for strong cipher suites via
   `curl -V | grep OpenSSL/LibreSSL/BoringSSL/GnuTLS`. Reject snap-packaged curl
   (missing permissions). See `nix-installer.sh:298-397`,
   `starship/install.sh:88-125`.

## 1. Divergence points

| Dimension                                  | NixOS install.sh                                                      | DetSys nix-installer                                                             | Lix installer         | nixos-anywhere                                                                                                                                           | rustup | starship |
| ------------------------------------------ | --------------------------------------------------------------------- | -------------------------------------------------------------------------------- | --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ | -------- |
| SHA-256 verify                             | **Yes** - hash baked into script at build time (`install.in:120-122`) | No inline - trusts TLS + binary self-check                                       | Same as DetSys (fork) | No (trusts SSH + flake eval)                                                                                                                             | No     | No       |
| Receipt file                               | No                                                                    | **Yes** - `/nix/receipt.json` (`plan.rs:15`)                                     | Yes (fork)            | N/A                                                                                                                                                      | No     | No       |
| Rollback on failure                        | Partial                                                               | **Yes** - auto-uninstall if plan fails (`lib.rs:17-31`)                          | Yes                   | N/A                                                                                                                                                      | No     | No       |
| Confinement checks                         | None                                                                  | SELinux, WSL1, NixOS, ostree, APFS encryption, Steam Deck                        | Same                  | SSH facts + container detect                                                                                                                             | None   | None     |
| Substituters configured BEFORE first build | N/A (installs Nix only)                                               | Yes - written to `/etc/nix/nix.conf` before daemon starts, closure fetched after | Same                  | **Yes - reads flake's `nix.settings.substituters` via `nix eval`, writes to `~/.config/nix/nix.conf` before `nix copy`** (`nixos-anywhere.sh:1045-1054`) | N/A    | N/A      |

## 2. Threat model notes

A `curl | sh` installer that does not do all of the following is a supply-chain
disaster:

- **TLS-only is not enough** if the installer binary subsequently fetches more
  code without verification. DetSys solves this by having the Rust binary itself
  ship a vendored expected hash for the Nix tarball it extracts (see
  `src/action/base/fetch_and_unpack_nix.rs`). The shell stub can be rogue, but
  it would have to pass sha2 verification once the Rust binary runs.
- **TOFU on the first substituter is lethal.** Any `extra-substituters` added
  before any build happens becomes a code-signing authority for everything built
  afterwards. Hence `trusted-public-keys` MUST be set in the same atomic write
  (`place_nix_configuration.rs:142-162`).
- **No eval of untrusted flake code with network access.** `nix eval` of
  `nix.settings.substituters` is safe only because it reads static attrs;
  nixos-anywhere relies on `--no-write-lock-file` and does _not_ IFD-build. xi
  must do the same.
- **TTY confusion:** `/dev/tty` reattach must only happen if `[ -t 1 ]` -
  otherwise an attacker can pre-fill stdin.

## 3. Steal list for `xi` bootstrap

- (**DetSys**) Planner trait + Receipt file at `/nix/xi-receipt.json`. Lets
  `xi system uninstall` reverse everything.
- (**DetSys**) Pre-install checks: `check_not_nixos`,
  `check_nix_not_already_installed`, `check_not_wsl1`, `check_systemd_active`,
  `detect_selinux`. See `src/planner/linux.rs:184-235`.
- (**DetSys**) `extra-trusted-substituters` + `extra-trusted-public-keys` in a
  dedicated `/etc/nix/nix.custom.conf` with `!include` from `nix.conf`. See
  `place_nix_configuration.rs:17-25`.
- (**nixos-anywhere**)
  `nix eval --apply toString ${flake}#${attr}.nix.settings.substituters` to read
  user substituters BEFORE the first build. This is the key move for xi's
  cache-first requirement.
- (**nixos-anywhere**) Facts script (`get-facts.sh`) emits a KV blob consumed by
  the orchestrator - a clean separation of discovery from decision.
- (**rustup**) `check_help_for` capability detection on curl/wget - graceful
  downgrade for weird platforms.
- (**official Nix script**) SHA-256 pinned at release time, verified with
  sha256sum/shasum/openssl fallback chain - do this for xi's own binary.
- (**rustup**) `--no-confirm` env override `NIX_INSTALLER_NO_CONFIRM`. xi:
  `XI_BOOTSTRAP_NO_CONFIRM`.

## 4. Per-source notes

### 4.1 Determinate Nix Installer - https://github.com/DeterminateSystems/nix-installer

- **Trust:** TLS 1.2+ with cipher-suite enforcement; no inline hash of the Rust
  binary itself. Once the binary runs, it fetches a `nix-*.tar.xz` from
  `install.determinate.systems` and verifies via a signature path internal to
  the binary. The shell stub (`nix-installer.sh`) explicitly disclaims being
  more than a selector.
- **Portability:** `uname -s` + `uname -m` with Rosetta/Darwin
  `sysctl hw.optional.*` disambiguation, Android via `uname -o`, illumos via
  `/usr/bin/uname -o` + `isainfo -n`. See `nix-installer.sh:145-237`.
- **Idempotency:** Checks `nix-env` presence before planning; refuses to run on
  NixOS; `pre_install_check` + `pre_uninstall_check` in every planner
  (`src/planner/mod.rs:146`, `linux.rs:174-196`).
- **Elevation:** Rust binary re-execs itself under `sudo` if not root; never
  trusts stdin for a sudo password.
- **Cache / substituter ORDER:** Linux planner executes in order
  `ProvisionDeterminateNixd → ProvisionNix → CreateUsersAndGroups → ConfigureNix → ProvisionSelinux → ConfigureInitService → Cleanup`
  (`src/planner/linux.rs:60-132`). `ConfigureNix` writes `/etc/nix/nix.conf`
  with `extra-trusted-substituters = https://cache.flakehub.com` and
  `extra-trusted-public-keys = cache.flakehub.com-3:…` BEFORE
  `ConfigureInitService` starts the daemon
  (`place_nix_configuration.rs:138-163`). So the daemon's first fetch already
  honors the custom cache.
- **Second-stage:** Binary spawns `systemctl start nix-daemon`; shell profile
  sourcing is written to `~/.bashrc` etc. via `ConfigureShellProfile`. The
  user's follow-up is a fresh shell.
- **Teardown:** `/nix/receipt.json` is a serialized `InstallPlan`;
  `nix-installer uninstall` replays actions in reverse. `plan.rs:15, 61-183`.
- **Confinement:** SELinux probe (`src/planner/linux.rs:222-235`), WSL1 reject
  (`:214-220`), NixOS reject (`:206-212`), ostree variant
  (`src/planner/ostree.rs`), Steam Deck variant (`src/planner/steam_deck.rs`),
  macOS APFS encryption probe (`src/planner/macos/mod.rs:166`).

### 4.2 Lix installer - https://git.lix.systems/lix-project/lix-installer

Hard fork of DetSys nix-installer. Identical architecture (Rust, planner,
receipt). Diverges on: no FlakeHub cache preseeded; replaces DetSys-specific
`determinate-nixd` with Lix's own daemon; substituter config writes
`https://cache.lix.systems` + `cache.lix.systems-1:aBnZUw8zA7H4KoCMhhQR` keys.
Same trust model: TLS + binary self-check, no inline sha256. (Confirmed via web
search; direct repo fetch returned 403 behind Forgejo auth.)

### 4.3 Official Nix install script - https://nixos.org/download.html → `install.in`

- **Trust:** SHA-256 of the per-arch tarball is **templated into the script at
  Nix build time**: `install.in:28-77` has `hash=@tarballHash_x86_64-linux@`.
  Script verifies with `sha256sum`/`shasum`/`openssl dgst` fallback
  (`install.in:110-122`). **Only installer in the family with inline hash
  verification.**
- **Portability:** `uname -s`.`uname -m` with explicit per-arch cases
  (`install.in:27-79`).
- **Idempotency:** None. Overwrites.
- **Elevation:** Delegates to the extracted `install` script inside the tarball
  which asks the user.
- **Substituters:** Default `cache.nixos.org` only; no user substituter
  plumbing.

### 4.4 nixos-anywhere - https://github.com/nix-community/nixos-anywhere

- **Trust:** SSH + known_hosts (or `StrictHostKeyChecking=no` for first boot).
  Zero inline hashes; trusts the local machine's flake eval and `nix copy` NAR
  signatures.
- **Portability:** `get-facts.sh` runs on the remote and emits `isOs`, `isArch`,
  `isNixos`, `isInstaller`, `isContainer`, `isRoot`, `hasIpv6Only`, `hasTar`,
  `hasCpio`, `hasSudo`, `hasDoas`, `hasWget`, `hasCurl`, `hasSetsid`,
  `hasNixOSFacter`. (`src/get-facts.sh:1-23`).
- **Idempotency:** Re-running is safe only up to kexec. After disko, re-running
  wipes.
- **Elevation:** `maybeSudo` string substituted into every remote command.
- **Cache / substituter ORDER (KEY FOR XI):** phases
  `kexec → disko → install → reboot`. BEFORE `nixosInstall`, lines 1045-1054:
  ```sh
  substituters=$(nix eval ${nixOptions[@]} --apply toString ${flake}#${flakeAttr}.nix.settings.substituters)
  trustedPublicKeys=$(nix eval ${nixOptions[@]} --apply toString ${flake}#${flakeAttr}.nix.settings.trusted-public-keys)
  runSsh sh <<SSH
  mkdir -p ~/.config/nix
  echo "extra-substituters = ${substituters}" >> ~/.config/nix/nix.conf
  echo "extra-trusted-public-keys = ${trustedPublicKeys}" >> ~/.config/nix/nix.conf
  SSH
  ```
  This answers "how do you read substituters without building the flake":
  `nix eval --apply toString` with
  `--extra-experimental-features 'nix-command flakes' --no-write-lock-file`. The
  attribute is read directly from the NixOS module's `nix.settings.substituters`
  option - pure eval, no IFD, no build.
- **Second-stage:** `nixosInstall` runs `nixos-install` on the target over SSH,
  which then `nix copy`s the closure using the just-configured substituters.
- **Teardown:** None; reboot is final.
- **Confinement:** Detects container (`systemd-detect-virt --container`),
  ipv6-only, nixos-installer variant.

### 4.5 disko bootstrap - https://github.com/nix-community/disko

Disko itself doesn't bootstrap - it's invoked by nixos-anywhere or by
`disko-install`. The `disko-install` flow (`lib/make-disk-image.nix:188`)
explicitly passes `--option binary-caches ""` for reproducible image builds,
i.e. it goes the opposite direction: **no substituters** at image build time.
The test fixtures (`tests/disko-install/configuration.nix:18`) `lib.mkForce [ ]`
substituters. Lesson for xi: when generating an image artifact (not
bootstrapping a running machine) the caller must choose between cache-fast and
cache-reproducible.

### 4.6 rustup install.sh - https://github.com/rust-lang/rustup/blob/master/rustup-init.sh

Canonical reference the Nix family copied. 933 lines. Same two-stage structure.
No sha256 verification. Introduces the `check_help_for` capability probe, the
`/dev/tty` stdin reattach, and the "snap curl is broken" check. Elevation: never
asks for sudo in the shell; the Rust binary writes only to `$CARGO_HOME`
(default `~/.cargo`), so sudo is unneeded.

### 4.7 Starship install.sh - https://github.com/starship/starship/blob/master/install/install.sh

POSIX-strict (`verify_shell_is_posix_or_exit`, `starship/install.sh:52-64`). No
sha256. Elevation: tests write access to `$BIN_DIR` and only prompts for sudo if
needed. Three-way downloader fallback (`curl`→`wget`→`fetch`) at
`install.sh:88-107`.

## 5. Proposed xi bootstrap flow

### Phase 0 - `curl | sh` downloads `xi`

- Host a `bootstrap.sh` on a dedicated subdomain (e.g. `install.xi.dev`) that
  mirrors `nix-installer.sh` almost 1:1. Pin TLS 1.2+, cipher suites, snap-curl
  reject, `/dev/tty` reattach, `uname` detection with Rosetta disambiguation.
- Script does ONE thing: pick arch, download `xi-${arch}` from GitHub Releases,
  verify a **pinned SHA-256 shipped inline** (steal from official Nix
  `install.in`), `chmod +x`, `exec`. Templating the hash at release time is a
  `release-please` or Nix build step.
- Env toggles: `XI_BOOTSTRAP_NO_CONFIRM`, `XI_BOOTSTRAP_URL_OVERRIDE`,
  `XI_BOOTSTRAP_FORCE_ALLOW_HTTP`.

### Phase 1 - `xi` installs Nix if absent

- `which nix` → skip. Otherwise **delegate to DetSys installer binary**:
  `curl -LsSf https://install.determinate.systems/nix | sh -s -- install --no-confirm --determinate=false`.
  Rationale: reimplementing SELinux/APFS/Steam Deck/receipt logic is a
  multi-month trap. xi can later vendor it via Cargo dep (crate is published as
  `nix-installer` on crates.io) if we want to drive the planner directly.
- xi records "Nix was installed by xi" in `/var/lib/xi/bootstrap.json` so
  `xi system uninstall` can call `/nix/nix-installer uninstall` on the way out.

### Phase 2 - Configure substituters BEFORE any build (CRITICAL)

- `git clone --depth=1 <repo-url> $XI_STATE_DIR/repo` into a tmp state dir.
- `nix flake metadata --no-write-lock-file $XI_STATE_DIR/repo` - fail fast if
  the repo is not a flake.
- Resolve the right attribute path: `nixosConfigurations.<host>` /
  `darwinConfigurations.<host>` / `homeConfigurations.<user>@<host>`. Host
  defaults to `hostname -s`.
- **Steal nixos-anywhere's trick:**
  ```sh
  xi_subs=$(nix eval --no-write-lock-file --extra-experimental-features 'nix-command flakes' \
    --apply toString \
    "$XI_STATE_DIR/repo#nixosConfigurations.$host.config.nix.settings.substituters")
  xi_keys=$(nix eval --no-write-lock-file --extra-experimental-features 'nix-command flakes' \
    --apply toString \
    "$XI_STATE_DIR/repo#nixosConfigurations.$host.config.nix.settings.trusted-public-keys")
  ```
  For home-manager where `nix.settings` is not necessarily in the module, fall
  back to a well-known attribute: `#xi.bootstrap.substituters` (xi contract -
  see Open Questions).
- Write `extra-substituters = …` + `extra-trusted-public-keys = …` to
  `/etc/nix/xi.conf`; `!include xi.conf` from `/etc/nix/nix.conf` (DetSys
  pattern, `place_nix_configuration.rs:25`).
- `systemctl restart nix-daemon` (Linux) /
  `launchctl kickstart -k system/org.nixos.nix-daemon` (macOS).

### Phase 3 - xi clones the repo into state dir

Already done opportunistically in Phase 2 (we needed the flake for eval). Move
the tmp clone to `$XI_STATE_DIR/current` on success.

### Phase 4 - `xi system switch`

- NixOS: `nixos-rebuild switch --flake $XI_STATE_DIR/current#$host` (xi already
  shells out to this per
  `/home/juliendauliac/ghq/github.com/Dauliac/xi/crates/xi-nixos`).
- Darwin: `darwin-rebuild switch --flake $XI_STATE_DIR/current#$host`.
- Home-manager: `home-manager switch --flake $XI_STATE_DIR/current#$user@$host`.
- All closures fetched through the user's own substituters configured in Phase 2
  → first build is cache-fast.

### Phase 5 - Optional gitops daemon

- `xi system switch --watch` or `xi daemon enable` installs a systemd user unit
  / launchd agent that polls the repo and re-runs Phase 4 on changes. Mirrors
  nix-darwin's `nix-darwin.pkgs.rebuild` or deploy-rs's `activate` wrappers.

## 6. Open questions

1. **How to read `nix.settings.substituters` for home-manager-only flakes?**
   NixOS has `config.nix.settings.*` as a well-known path; home-manager does
   not. Options: (a) require the flake to publish a well-known
   `xi.bootstrap.substituters` attribute (bootstrap contract); (b) read
   `/etc/nix/nix.conf` already on the host (no help on fresh boxes); (c) teach
   xi to probe both and pick whichever evaluates. Leaning (a).
2. **Where does xi's own binary live, and who signs it?** Hosting on
   `install.xi.dev` means operating that CDN. Alternative: pin to GitHub
   Releases + inline sha256 (official Nix pattern). Still need code signing on
   macOS or Gatekeeper will quarantine it.
3. **Chicken-and-egg: Phase 2 needs `nix eval`, which needs Nix, which Phase 1
   installed with the DEFAULT substituters.** That is fine for Phase 2's own
   fetch (xi binary and `nix eval` are tiny), but Phase 2 must happen BEFORE
   Phase 4's big build. Confirmed OK with nixos-anywhere's ordering - document
   it explicitly.
4. **What happens on repeat `curl | sh`?** DetSys nix-installer refuses if nix
   exists; xi should instead treat it as `xi system switch` on an existing
   install (idempotent gitops reconvergence). Needs a bootstrap-receipt
   distinction.
5. **How do we uninstall atomically?** Two layers: (a) xi state + units (easy,
   we own them); (b) Nix itself (delegate to `nix-installer uninstall` if we
   invoked it, otherwise leave Nix alone). Receipt file at
   `/var/lib/xi/bootstrap.json` must track the "did we install Nix" bit so we
   don't rip out a Nix the user had before.
