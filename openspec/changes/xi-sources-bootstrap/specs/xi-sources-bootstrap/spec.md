# Capability: xi-sources-bootstrap

## ADDED Requirements

### Requirement: Shell stub

The capability MUST provide a shell stub (`scripts/bootstrap.sh`) hosted at a
stable URL that, when piped to `sh`, prepares and `exec`s the compiled `xi`
binary with the user's arguments.

The stub MUST:

- Detect OS and arch via `uname -s` / `uname -m` with Rosetta disambiguation on
  Darwin (`sysctl hw.optional.arm64`).
- Prefer `curl --proto '=https' --tlsv1.2`; fall back to `wget`, then `fetch`.
- Refuse snap-packaged curl (missing permissions bug upstream).
- Re-attach `/dev/tty` to the child when run via `curl | sh` (`[ ! -t 0 ]`
  branch).
- Verify the downloaded binary against an inline SHA-256 templated at release
  time; mismatch MUST abort before any `exec`.
- Honour env toggles `XI_BOOTSTRAP_NO_CONFIRM` and `XI_BOOTSTRAP_URL_OVERRIDE`.

#### Scenario: Tampered binary refused

- **GIVEN** the stub's inline SHA-256 is `A` and the download hashes to `B`
- **WHEN** the stub runs
- **THEN** it exits non-zero before any `exec`
- **AND** the tmp directory is cleaned

### Requirement: `xi system switch <remote-url>`

The capability MUST recognise a config-repo remote as a positional argument to
any `xi {system,nixos,darwin,home} switch` and trigger the bootstrap sequence
instead of a local-flake dispatch. Which argument shapes count as a remote is
owned by [[xi-sources-config]]; this capability consumes that decision rather
than restating it.

The bootstrap sequence is a planner that MUST emit a plan the user can preview
before execution. Non-interactive mode (`XI_BOOTSTRAP_NO_CONFIRM=1` or
`--no-confirm`) skips the preview.

#### Scenario: URL-shaped argument triggers bootstrap

- **GIVEN** no sources configured for the resolved scope
- **WHEN** `xi system switch https://git.example/dotfiles` runs
- **THEN** the bootstrap planner starts

#### Scenario: Local-flake argument retains old behaviour

- **GIVEN** `./flake.nix` exists in the current directory
- **WHEN** `xi system switch .` runs
- **THEN** the direct dispatch path is used, not the bootstrap

### Requirement: Installer pre-check on every switch

Probing for a usable Nix MUST be a pre-check on **every**
`xi {system,nixos,darwin,home} switch`, not only on the bootstrap path. Reaching
a switch on a machine without Nix is the same problem whether the operator
passed a remote URL, a local flake, or nothing at all, and a bare
`nix: command not found` is a worse answer than installing it.

The pre-check MUST:

1. Run before any evaluation or build work.
2. Be a no-op, with no network access and no prompt, when a usable `nix` is
   already on PATH. This is the overwhelmingly common case and it MUST NOT slow
   the everyday switch down.
3. When Nix is absent and the resolved `installer` is not `none`, delegate to
   the backend below, then continue the switch.
4. When Nix is absent and `installer = "none"`, halt with an error naming the
   config key that would let xi install it.

This is what lets the shell stub be nothing more than "download xi, exec
switch": the binary carries the whole flow, so a bare machine and a managed
machine take the same command.

#### Scenario: Nix present is a silent no-op

- **GIVEN** a usable `nix` on PATH
- **WHEN** any `xi <scope> switch` runs
- **THEN** no installer is invoked, nothing is downloaded, and no prompt is
  shown

#### Scenario: Nix absent on a plain local switch

- **GIVEN** no `nix` on PATH and `installer = "detsys"`
- **WHEN** `xi system switch .` runs, with no remote URL anywhere
- **THEN** the installer pre-check still fires and installs Nix
- **AND** the switch continues against the local flake

#### Scenario: Opting out names the escape hatch

- **GIVEN** no `nix` on PATH and `installer = "none"`
- **WHEN** any `xi <scope> switch` runs
- **THEN** the command exits non-zero
- **AND** the message names the `installer` key and its accepted values

### Requirement: Ordered planner phases

The planner MUST execute in this order:

1. `CheckPreflight` - reject if receipt exists (unless `--force`), if sudo is
   unavailable, if the OS mismatches the scope, or if SELinux is enforcing /
   WSL1.
2. `InstallNix`, if `which nix` fails, shell-exec the installer backend selected
   by the `installer` config key (see the "Pluggable installer backend"
   requirement below). Any non-zero exit from the chosen backend terminates the
   whole plan.
3. `CloneSources` - git clone the remote URL into the sources dir per
   [[xi-sources-config]] path table.
4. `CachePrestage` - delegate to [[xi-sources-cache-prestage]] unless
   `--no-prestage`.
5. `FirstSwitch` - invoke the correct backend
   (`nixos-rebuild switch --flake <sources-dir>#<host>` /
   `darwin-rebuild switch` / `home-manager switch`).
6. `EnableGitops` (optional) - enable [[xi-sources-gitops]] if `--enable-gitops`
   is passed OR the activated module declares `xi.sources.gitops.enable = true`.
7. `WriteReceipt` - append each action and timestamp.

Any phase failing MUST stop subsequent phases and leave the receipt accurate
about what actually happened. The receipt is written incrementally after each
successful phase so a crash still enables `xi system uninstall` to reverse
partial state.

#### Scenario: Nix install fails, bootstrap halts

- **GIVEN** `which nix` returns no match AND the selected installer backend's
  shell-exec returns a non-zero exit
- **WHEN** the planner reaches phase 2
- **THEN** phases 3-7 do NOT run
- **AND** the receipt records only `CheckPreflight` as applied

### Requirement: Pluggable installer backend

Phase 2 (`InstallNix`) MUST honour an `installer` config key, resolved via the
standard precedence ladder in [[xi-sources-config]], with exactly three
permitted values:

- `detsys`, invoke
  `curl -LsSf https://install.determinate.systems/nix
  | sh -s -- install --no-confirm --determinate=false`.
- `lix`, invoke
  `curl -sSf -L https://install.lix.systems/lix | sh -s
  -- install --no-confirm`.
- `none`, skip phase 2 entirely and continue to phase 3; the planner MUST still
  verify `which nix` succeeds after the skip and halt with a clear error if Nix
  is still absent.

The default MUST be `detsys` unless the resolved flake's declared substituters
list contains `cache.lix.systems`, in which case the default is `lix`. An
explicit `installer` value always wins over this heuristic.

The receipt entry for a completed phase 2 MUST include
`nix-installed-by: detsys | lix` so that `xi system uninstall` can pick the
matching uninstaller.

#### Scenario: Default picks lix when cache.lix.systems is declared

- **GIVEN** the resolved flake declares
  `nix.settings.substituters = ["https://cache.lix.systems" ...]` and no
  explicit `installer` is set
- **WHEN** the planner reaches phase 2
- **THEN** the Lix installer is invoked

#### Scenario: Explicit installer flag wins over heuristic

- **GIVEN** the same flake AND the user passed `--installer detsys`
- **WHEN** the planner reaches phase 2
- **THEN** the Determinate Systems installer is invoked

#### Scenario: installer = none skips but still verifies

- **GIVEN** `installer = "none"` AND `which nix` fails before phase 2
- **WHEN** phase 2 runs
- **THEN** the phase is skipped
- **AND** phase 3 is NOT reached; the planner halts with a message naming the
  missing `nix` binary and the three valid `installer` values

#### Scenario: Sources clone fails, Nix install reversal available

- **GIVEN** phase 2 installed Nix, phase 3 fails (bad URL)
- **WHEN** the planner halts
- **THEN** the receipt records `InstallNix` as applied
- **AND** `xi system uninstall` reverses `InstallNix` via
  `nix-installer uninstall`

### Requirement: Receipt file

The capability MUST write `/var/lib/xi/bootstrap-receipt.json` (system) or
`${XDG_STATE_HOME}/xi/bootstrap-receipt.json` (user) listing each applied phase,
timestamp, and the exact command that reverses it.

`xi system uninstall` reads the receipt and executes the reverse commands in
reverse order.

#### Scenario: Full round trip is reversible

- **GIVEN** a full bootstrap on a fresh machine succeeded
- **WHEN** `xi system uninstall` runs
- **THEN** the sources dir is removed, cache-prestage is unstaged, Nix (if
  installed by bootstrap) is uninstalled, and the receipt is removed
- **AND** `/etc/nix/nix.conf` is byte-identical to its state before the
  bootstrap (confirmed by [[xi-sources-cache-prestage]] receipt)

### Requirement: Remote SSH form

The capability MUST accept
`xi {system,darwin,home} switch
ssh://user@host <remote-url>` and execute the
SAME planner on the remote host, with the following transport:

1. `scp` the local xi binary to `~/xi` on the remote.
2. `ssh user@host "~/xi system switch <remote-url>"` with log streaming.
3. On success, `ssh user@host "rm ~/xi"` cleanup.

The remote planner is identical to the local one; preflight is re-run on the
remote so detecting its OS/variants is authoritative.

#### Scenario: Remote bootstrap over SSH

- **GIVEN** a reachable host `example.net` running Debian with sudo
- **WHEN**
  `xi system switch ssh://admin@example.net
  https://git.example/dotfiles` runs
- **THEN** the local xi binary is copied to the remote, the planner runs there,
  and logs are streamed back to the operator's terminal

### Requirement: Preflight exact behaviour

Preflight MUST reject on any of:

- A valid `bootstrap-receipt.json` already exists on the host (unless `--force`)
- `sudo -n true` fails (unless `--interactive-sudo`)
- `uname -s` does not match the declared scope (`system switch` on macOS without
  `darwin` scope → reject)
- SELinux is in `Enforcing` mode (unless `--allow-selinux`)
- The host is WSL1 (WSL2 is accepted)

#### Scenario: Re-bootstrap requires --force

- **GIVEN** `bootstrap-receipt.json` exists
- **WHEN** `xi system switch <remote-url>` runs without `--force`
- **THEN** the planner refuses before any phase runs
