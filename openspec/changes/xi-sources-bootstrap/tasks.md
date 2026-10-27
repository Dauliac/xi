# Tasks: xi-sources-bootstrap

Live state in beads under `change:xi-sources-bootstrap`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-bootstrap/spec.md`.
- [ ] 1.2 Cross-link with model/config/cache-prestage/gitops/push.

## 2. Shell stub `scripts/bootstrap.sh` (P1)

- [ ] 2.1 OS/arch detection, downloader chain (`curl` → `wget` → `fetch`),
      snap-curl refusal, `/dev/tty` reattach - port literally from
      `nix-installer.sh:145-237` and `:298-397`.
- [ ] 2.2 Inline SHA-256 verification of the downloaded binary; template the
      hash into the file at release time (steal from
      `install.in:
      120-122`).
- [ ] 2.3 Env toggles: `XI_BOOTSTRAP_NO_CONFIRM`, `XI_BOOTSTRAP_URL_OVERRIDE`.
- [ ] 2.4 `exec` the downloaded binary with the user's remaining args.

## 3. Planner (P1)

- [ ] 3.1 `crates/xi/src/bootstrap.rs` with a `BootstrapPlan` struct enumerating
      actions; each action has `apply()` and `reverse()`.
- [ ] 3.2 Actions: `CheckPreflight`, `InstallNix` (optional), `CloneSources`,
      `CachePrestage` (optional via `--no-prestage`), `FirstSwitch`,
      `EnableGitops` (optional), `WriteReceipt`.
- [ ] 3.3 Receipt serialized to `/var/lib/xi/bootstrap-receipt.json` or user XDG
      path.

## 4. Preflight (P1)

- [ ] 4.1 Check receipt absent (else refuse unless `--force`).
- [ ] 4.2 Check sudo (`sudo -n true`); interactive fallback with
      `--interactive-sudo`.
- [ ] 4.3 Reject SELinux-enforcing, WSL1 (steal from DetSys `linux.rs:206-220`).

## 5. Nix install delegation (P1)

- [ ] 5.1 `which nix` succeeds, skip the phase.
- [ ] 5.2 Add an `installer` enum config key with values `detsys | lix | none`.
      Default picks `lix` when the resolved flake declares `cache.lix.systems`
      as a substituter; otherwise `detsys`. An explicit `--installer` flag
      always wins.
- [ ] 5.3 `installer = "detsys"`: shell out to
      `curl -LsSf
      https://install.determinate.systems/nix | sh -s -- install
      --no-confirm --determinate=false`.
- [ ] 5.4 `installer = "lix"`: shell out to
      `curl -sSf -L
      https://install.lix.systems/lix | sh -s -- install --no-confirm`.
- [ ] 5.5 `installer = "none"`: skip the phase, verify `which nix` succeeds
      after, halt with a clear error if still absent.
- [ ] 5.6 On any installer failure, exit non-zero; receipt records no actions.
      On success, record `nix-installed-by: <backend>` for the matching
      uninstaller path.

## 6. Remote SSH form (P2)

- [ ] 6.1 `xi system switch ssh://user@host <remote-url>` detects the ssh://
      scheme and runs the planner remotely.
- [ ] 6.2 `scp` the xi binary; exec `~/xi system switch <remote-url>` over SSH;
      stream logs via a well-known pipe.
- [ ] 6.3 Share preflight implementation with [[xi-sources-push]]
      `--phase install`.

## 7. Uninstall (P1)

- [ ] 7.1 `xi system uninstall` reads the receipt and reverses in order (last
      action first).
- [ ] 7.2 Includes `nix-installer uninstall` when xi installed Nix.

## 8. Tests (P1)

- [ ] 8.1 BDD: full bootstrap in a NixOS container fixture.
- [ ] 8.2 Unit: inline-sha256 refusal on tampered binary.
- [ ] 8.3 Unit: receipt round-trip (apply → reverse → identical state).

## 9. Follow-ups (P4)

- [ ] 9.1 First-boot disko partitioning via `xi push --phase
      kexec,disko`
      composition - deferred to push spec.
- [ ] 9.2 Secondary signing (SSH-key-based attestation) for the inline hash.
      Deferred.
- [ ] 9.3 Interactive planner TUI. Deferred.
